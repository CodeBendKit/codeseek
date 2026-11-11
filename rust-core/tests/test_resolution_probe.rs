//! Phase P0 探针（不参与常规 `cargo test` 断言，需 `--ignored --nocapture` 手动运行）。
//!
//! 目的：把 5+2 类消歧场景在**真实 parser 链路**上的产出完整打印出来，
//! 作为 `test_resolution_baseline.rs` 里"断言当前真实行为"的依据来源。
//! 后续阶段（P2 改造解析语义）改完后重跑本探针，即可逐字段对比 precision 变化。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::treesitter::TreeSitterParser;

/// 把 fixture case 复制到唯一的临时目录，并用 `--force-reindex` 语义全量建图。
fn build_case(case: &str) -> (tempfile::TempDir, codeseek::codegraph::types::PetCodeGraph) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resolution")
        .join(case);
    assert!(src.is_dir(), "fixture case 不存在: {}", src.display());

    let temp_dir = tempfile::TempDir::new().expect("创建临时目录失败");
    // 复制后的目录名带上 case 名 + 随机后缀，避免与真实项目的 project_id 撞名
    let project_dir = temp_dir
        .path()
        .join(format!("p0probe_{}_{}", case, std::process::id()));
    copy_tree(&src, &project_dir);

    let mut parser = CodeParser::new();
    parser.set_force_reindex(true);
    let graph = parser
        .build_petgraph_code_graph(&project_dir)
        .unwrap_or_else(|e| panic!("{} 建图失败: {}", case, e));

    (temp_dir, graph)
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("创建目标目录失败");
    for entry in std::fs::read_dir(src).expect("读取 fixture 目录失败").flatten() {
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("复制 fixture 文件失败");
        }
    }
}

/// 打印单个源文件的 AST 符号清单：这是"原料层"的事实（名称/命名空间/行号）。
fn dump_symbols(file: &Path) -> Vec<String> {
    let parser = TreeSitterParser::new();
    let path_buf = file.to_path_buf();
    let symbols = parser
        .parse_file(&path_buf)
        .unwrap_or_else(|e| panic!("解析 {} 失败: {:?}", file.display(), e));

    let mut rows = Vec::new();
    for symbol in symbols {
        let guard = symbol.read();
        let s = guard.as_ref();
        rows.push(format!(
            "  {:<20} name={:<24} namespace={:<16} line={}",
            format!("{:?}", s.symbol_type()),
            s.name(),
            s.namespace(),
            s.full_range().start_point.row + 1
        ));
    }
    rows
}

#[test]
#[ignore = "P0 探针：需要 --ignored --nocapture 手动运行，不参与常规断言"]
fn probe_dump_actual_resolution_behavior() {
    let cases = [
        "cross_file_same_name",
        "python_import_alias",
        "rust_qualified_path",
        "python_dotted_qualified",
        "method_receiver",
        "nested_function_caller",
        "ambiguous_trait_impl",
    ];

    for case in cases {
        println!("\n================ CASE: {case} ================");
        let (_temp, graph) = build_case(case);
        let stats = graph.get_stats();
        println!(
            "[stats] functions={} files={} languages={:?} resolved={} unresolved={}",
            stats.total_functions,
            stats.total_files,
            stats.languages,
            stats.resolved_calls,
            stats.unresolved_calls
        );

        // 函数节点
        let mut nodes: BTreeSet<String> = BTreeSet::new();
        for function in graph.get_all_functions() {
            nodes.insert(format!(
                "{}::{} [{}..{}] ns={} lang={}",
                short_path(&function.file_path),
                function.name,
                function.line_start,
                function.line_end,
                function.namespace,
                function.language
            ));
        }
        println!("[functions]");
        for node in &nodes {
            println!("  {node}");
        }

        // 调用边
        let mut edges: BTreeSet<String> = BTreeSet::new();
        for edge in graph.graph.edge_references() {
            let rel = edge.weight();
            let describe = |id: &uuid::Uuid| match graph.get_function_by_id(id) {
                Some(f) => format!("{}::{}", short_path(&f.file_path), f.name),
                None => "<not-a-node>".to_string(),
            };
            edges.insert(format!(
                "L{:<3} {:<34} --{:} ({})--> {}",
                rel.line_number,
                describe(&rel.caller_id),
                rel.callee_name,
                rel.is_resolved,
                describe(&rel.callee_id)
            ));
        }
        println!("[edges] total={}", edges.len());
        for edge in &edges {
            println!("  {edge}");
        }

        // 同名候选表（歧义的直接证据）
        let mut ambiguous: Vec<(&String, usize)> = graph
            .function_names
            .iter()
            .map(|(name, ids)| (name, ids.len()))
            .filter(|(_, count)| *count > 1)
            .collect();
        ambiguous.sort();
        println!("[same-name candidates] {ambiguous:?}");

        // AST 层原料
        println!("[ast symbols]");
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resolution").join(case), &mut files);
        files.sort();
        for file in files {
            println!("  --- {}", short_path(&file));
            for row in dump_symbols(&file) {
                println!("{row}");
            }
        }
    }

    println!("\n================ 探针结束 ================");
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("读取目录失败").flatten() {
        if entry.path().is_dir() {
            collect_files(&entry.path(), out);
        } else if is_supported_source(&entry.path()) {
            out.push(entry.path());
        }
    }
}

fn short_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    let marker = "resolution/";
    match text.find(marker) {
        Some(idx) => text[idx + marker.len()..].to_string(),
        // 复制到临时目录后的形状是 <tmp>/p0probe_<case>_<pid>/<file>，只留文件名即可
        None => path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| text.to_string()),
    }
}

/// 只收集 parser 支持的后缀（README.md 之类的说明文件要排除掉）。
fn is_supported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs") | Some("py") | Some("js") | Some("ts") | Some("java") | Some("go") | Some("c") | Some("cpp")
    )
}
