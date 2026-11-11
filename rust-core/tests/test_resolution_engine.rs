//! Phase P2a 交付物 4：**legacy vs layered 离线对比 harness**（P2a 最有价值的产出）。
//!
//! # 这个文件在做什么
//! 对 `tests/fixtures/resolution/` 的 7 组消歧场景 + `tests/test_repos/` 的 4 个整仓小项目，
//! 走**真实 parser** 得到 AST 符号与 `FileScopeCtx`，再用
//! [`call_sites_from_symbols_with_source`] + [`build_resolution_index_from_graph`] +
//! [`resolve_call_with_config`] 计算 layered 结果，与 legacy（当前图里真实存在的调用边）
//! 逐调用点对比，打印四张表：
//!
//! ```text
//! 表 1  7 组场景 × 全部调用点   调用点 / legacy 命中 / layered 层级 / layered 候选数 / 判定
//! 表 2  4 个整仓 × 抽样前 20 行  同上
//! 表 3  逐层定位矩阵            同一调用点在"关某层 / 只开某层"下的层级与候选数
//! 表 4  确定性矩阵              同一输入连解两次、同一目录连建两次索引的比较结果
//! ```
//!
//! # 红线：这里**只算不写**
//! 本文件不产生任何 `FunctionInfo` / `CallRelation` / `Uuid`，不落盘、不改图：
//! 图只是"legacy 答案"的只读来源，layered 结果全部活在测试进程内。
//! 五处 first-wins（`parser.rs` 的 `_find_callee_function` / `_resolve_callee_function` /
//! `_resolve_qualified_function_name` / `_build_function_name_index` 与
//! `storage/incremental.rs` 的 `_find_callee_function`）一行未改，接线在 P2b。
//!
//! # 为什么必须**按场景分组**各建一次图/索引（而不是整个 fixtures 根建一次）
//! P0/P1 的基线口径就是"每个场景目录 = 一个仓库"（`tests/test_p1_edges_unchanged.rs`
//! 的 `PER_CASE_BASELINE`）。如果把 7 个场景一次性建成一个"仓库"，
//! `cross_file_same_name/gamma.rs` 的 `helper()` 会把 `python_dotted_qualified` 里的
//! `mod_y.helper` / `other_mod.helper` 也当成候选，实测候选数从 2 变成 4 —— 那不是
//! 引擎的错，是**分母**错了。期望表里"candidate_count=2（alpha+beta）"只有在分组口径下才成立。
//!
//! # 期望表出处
//! 任务书 Phase P2a 的"期望表"（8 行），逐行断言见 [`expected_rows`]。
//! **实测不符时不改期望迁就实现**：差异与根因写在每张期望行旁边的注释里。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::resolution::index::build_resolution_index_from_graph;
use codeseek::codegraph::resolution::resolver::{
    call_sites_from_symbols_with_source, resolve_call_with_config, CallSite, ResolutionConfig,
};
use codeseek::codegraph::resolution::{
    FileScopeCtx, Resolution, ResolutionIndex, ResolutionLevel, ResolutionStatus,
};
use codeseek::codegraph::types::PetCodeGraph;
use codeseek::codegraph::treesitter::TreeSitterParser;

const RESOLUTION_ROOT: &str = "tests/fixtures/resolution";
const REPOS_ROOT: &str = "tests/test_repos";

/// 7 组消歧场景（与 `tests/test_p1_edges_unchanged.rs` 的 `PER_CASE_BASELINE` 同序）。
const SCENARIOS: &[&str] = &[
    "cross_file_same_name",
    "python_import_alias",
    "rust_qualified_path",
    "python_dotted_qualified",
    "method_receiver",
    "nested_function_caller",
    "ambiguous_trait_impl",
];

/// 4 个整仓小项目（跨语言回归面）。
const REPOS: &[&str] = &[
    "simple_rust_project",
    "simple_python_project",
    "simple_ts_project",
    "simple_js_project",
];

// ============================================================================================
// 一次"分组建图 + 分组解析"的产物
// ============================================================================================

/// 单个文件在该场景里的原料：真实 AST 符号、ctx、由符号得到的调用点。
struct FileFacts {
    path: PathBuf,
    ctx: Arc<FileScopeCtx>,
    sites: Vec<CallSite>,
}

/// 一个"仓库"（场景目录或小项目）的离线对比上下文。
struct Scenario {
    name: String,
    _temp: tempfile::TempDir,
    root: PathBuf,
    graph: PetCodeGraph,
    index: ResolutionIndex,
    files: Vec<FileFacts>,
}

/// 一条调用点的对比结果（表 1/2 的一行）。
#[derive(Debug, Clone)]
struct Row {
    scenario: String,
    /// 相对场景根的文件路径（打印用）。
    file: String,
    line: usize,
    /// 调用点写法：`helper(x)` / `calc.add(x)` / `mod_a::pick(x)`。
    call_text: String,
    /// legacy：图里这条调用点真实产出的边（可能是 0 条、1 条或多条）。
    legacy: Vec<String>,
    layered_level_name: &'static str,
    /// `Resolution::total_candidate_count`（截断前的真实个数）。
    layered_candidates: usize,
    layered_status: &'static str,
    /// layered 最优候选定位（`file:line#name`），未解析为 `-`。
    layered_primary: String,
    verdict: &'static str,
}

impl Scenario {
    /// 把 `rel_src` 复制进唯一命名的临时目录，走**真实全量建图**，再逐文件补 ctx 与调用点。
    ///
    /// 复制而非原地使用：与守卫测试同样是为了避免污染仓库、并让 `derive_module_path`
    /// 的"相对根"口径与 P0 基线一致（root = 场景目录本身）。
    fn build(label: &str, rel_src: &str) -> Self {
        let src = manifest_dir().join(rel_src);
        assert!(src.is_dir(), "fixture 目录不存在: {}", src.display());

        let temp = tempfile::TempDir::new().expect("创建临时目录失败");
        let root = temp.path().join(format!("p2a_{label}_{}", std::process::id()));
        copy_tree(&src, &root);

        let mut parser = CodeParser::new();
        parser.set_force_reindex(true);
        // Phase P3 交付物 3：本 harness 把"图里真实存在的边"当作 **legacy 答案来源**
        // （`legacy_edges()`），必须显式钉 Legacy；layered 结果全部在测试进程内用引擎
        // 现算，不依赖这里的建图档位。
        parser.set_resolver_mode(codeseek::cli::args::ResolverMode::Legacy);
        let graph = parser
            .build_petgraph_code_graph(&root)
            .unwrap_or_else(|e| panic!("[{label}] 建图失败: {e}"));

        // 逐文件补 ctx（交付物 2(b)：`ensure_file_scope_ctx` —— 命中缓存直接返回，缺失按需解析补建）。
        let mut per_file: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
        let mut files: Vec<FileFacts> = Vec::new();
        for path in collect_source_files(&root) {
            let ctx = match parser.ensure_file_scope_ctx(&path) {
                Some(ctx) => ctx,
                None => continue,
            };
            per_file.insert(path.clone(), ctx.clone());
            let ts = TreeSitterParser::new();
            let ast = ts
                .parse_file_with_metadata(&path)
                .unwrap_or_else(|e| panic!("[{label}] 解析失败 {}: {}", path.display(), e.message));
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            let mut sites =
                call_sites_from_symbols_with_source(&ast.symbols, &ctx, Some(&source));
            // 同一文件里的调用点按行号稳定排序（函数内部已按 (line, name, guid) 排过）。
            sites.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.callee_name.cmp(&b.callee_name)));
            files.push(FileFacts { path, ctx, sites });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let index = build_resolution_index_from_graph(&graph, &per_file, Some(root.as_path()));

        Self {
            name: label.to_string(),
            _temp: temp,
            root,
            graph,
            index,
            files,
        }
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// legacy 答案索引：`(caller_file, line_number)` → 该调用点在**当前图里**真实产出的边。
    ///
    /// 口径完全取自 `CallRelation`，不做任何加工 —— 这正是 P2b 要替换掉的东西。
    fn legacy_edges(&self) -> HashMap<(PathBuf, usize), Vec<String>> {
        let mut map: HashMap<(PathBuf, usize), Vec<String>> = HashMap::new();
        for edge in self.graph.graph.edge_references() {
            let rel = edge.weight();
            let callee = self.graph.get_function_by_id(&rel.callee_id);
            let text = match callee {
                Some(info) => format!(
                    "{}:{}#{}{}",
                    strip_tmp(&relative_of(&self.root, &info.file_path)),
                    info.line_start,
                    info.name,
                    if rel.is_resolved { "" } else { " [未解析]" }
                ),
                None => format!("<悬空 callee {}>", rel.callee_name),
            };
            map.entry((rel.caller_file.clone(), rel.line_number))
                .or_default()
                .push(text);
        }
        // 每个调用点的边按字符串排序，保证打印与断言与 HashMap/边插入顺序无关。
        for v in map.values_mut() {
            v.sort();
            v.dedup();
        }
        map
    }

    /// 生成该场景的全部对比行（layered = 全层开启 + 默认候选上限）。
    fn rows(&self, config: &ResolutionConfig) -> Vec<Row> {
        let legacy = self.legacy_edges();
        let mut rows = Vec::new();
        for file in &self.files {
            for site in &file.sites {
                let resolution =
                    resolve_call_with_config(site, Some(file.ctx.as_ref()), &self.index, config);
                rows.push(self.make_row(site, file, &legacy, &resolution));
            }
        }
        rows.sort_by(|a, b| {
            (&a.file, a.line, &a.call_text).cmp(&(&b.file, b.line, &b.call_text))
        });
        rows
    }

    fn make_row(
        &self,
        site: &CallSite,
        file: &FileFacts,
        legacy: &HashMap<(PathBuf, usize), Vec<String>>,
        resolution: &Resolution,
    ) -> Row {
        let legacy_hit = legacy
            .get(&(file.path.clone(), site.line))
            .cloned()
            .unwrap_or_default();
        let primary = self.primary_text(resolution);
        Row {
            scenario: self.name.clone(),
            file: self.relative(&file.path),
            line: site.line,
            call_text: call_text(site),
            legacy: legacy_hit.clone(),
            layered_level_name: level_name(resolution),
            layered_candidates: resolution.total_candidate_count,
            layered_status: status_name(&resolution.status),
            layered_primary: primary.clone(),
            verdict: verdict_of(&legacy_hit, resolution, &primary),
        }
    }

    /// 候选的 `file:line#name` 描述（uuid 无关，可跨进程比较）。
    fn describe_candidate(&self, resolution: &Resolution, idx: usize) -> String {
        match resolution.candidates.get(idx) {
            Some(candidate) => self
                .index
                .tag_by_id(&candidate.callee_id)
                .map(|tag| {
                    format!(
                        "{}:{}#{}",
                        strip_tmp(&relative_of(&self.root, &tag.file)),
                        tag.line_start,
                        tag.name
                    )
                })
                .unwrap_or_else(|| format!("<未知候选 {}>", candidate.callee_id)),
            None => "-".to_string(),
        }
    }

    fn primary_text(&self, resolution: &Resolution) -> String {
        if resolution.status == ResolutionStatus::Unresolved {
            return "-".to_string();
        }
        self.describe_candidate(resolution, 0)
    }

    /// 期望表比对：按 (场景, 相对文件后缀, 行号) 找调用点。
    fn site(&self, file_suffix: &str, line: usize) -> (&FileFacts, &CallSite) {
        for file in &self.files {
            if self.relative(&file.path).ends_with(file_suffix) {
                if let Some(site) = file.sites.iter().find(|s| s.line == line) {
                    return (file, site);
                }
            }
        }
        panic!(
            "[{}] 找不到调用点 {}:{}（场景文件 = {:?}）",
            self.name,
            file_suffix,
            line,
            self.files.iter().map(|f| self.relative(&f.path)).collect::<Vec<_>>()
        );
    }

    fn resolve(&self, file_suffix: &str, line: usize, config: &ResolutionConfig) -> Resolution {
        let (file, site) = self.site(file_suffix, line);
        resolve_call_with_config(site, Some(file.ctx.as_ref()), &self.index, config)
    }

    /// 清掉本次写进 `~/.codeseek/projects/` 的目录（与守卫测试同一口径）。
    fn cleanup_home(&self) {
        let id_by_md5 = format!(
            "{:x}",
            md5::compute(self.root.to_string_lossy().as_bytes())
        );
        cleanup_codeseek_project(&id_by_md5);
        if let Some(name) = self.root.file_name() {
            cleanup_codeseek_project(&name.to_string_lossy());
        }
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        self.cleanup_home();
    }
}

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn relative_of(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 临时目录前缀太长且每次不同，打印时只留最后两级路径，避免表格噪声。
fn strip_tmp(text: &str) -> String {
    let parts: Vec<&str> = text.split('/').collect();
    if parts.len() <= 3 {
        return text.to_string();
    }
    parts[parts.len() - 3..].join("/")
}

fn cleanup_codeseek_project(project_id: &str) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let dir = home.join(".codeseek").join("projects").join(project_id);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("创建目标目录");
    for entry in std::fs::read_dir(src).expect("读取 fixture 目录") {
        let entry = entry.expect("目录项");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("复制文件");
        }
    }
}

fn collect_source_files(dir: &Path) -> Vec<PathBuf> {
    const SUPPORTED: &[&str] = &[
        "rs", "py", "java", "go", "ts", "tsx", "js", "jsx", "mjs", "cjs", "cpp", "cc", "c", "h",
    ];
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .unwrap_or_default();
            if SUPPORTED.contains(&extension.as_str()) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// 把调用点还原成可读写法（`calc.add(x)` / `mod_a::pick(x)` / `alias(x)`）。
fn call_text(site: &CallSite) -> String {
    let mut name = if site.qualified_components.is_empty() {
        site.callee_name.clone()
    } else {
        format!(
            "{}{}{}",
            site.qualified_components.join(separator_for(&site.language)),
            separator_for(&site.language),
            site.callee_name
        )
    };
    if site.has_receiver() && !site.receiver_is_self() {
        name = format!("{}.{}", site.receiver.clone().unwrap_or_default(), name);
    }
    let args = site
        .arg_count
        .map(|n| (0..n).map(|i| format!("a{i}")).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    format!("{name}({args})")
}

fn separator_for(language: &str) -> &'static str {
    if language == "rust" {
        "::"
    } else {
        "."
    }
}

fn status_name(status: &ResolutionStatus) -> &'static str {
    match status {
        ResolutionStatus::Unresolved => "Unresolved",
        ResolutionStatus::Resolved => "Resolved",
        ResolutionStatus::Ambiguous => "Ambiguous",
    }
}

fn level_name(resolution: &Resolution) -> &'static str {
    match resolution.primary() {
        Some(candidate) => level_label(candidate.level),
        None => "Unresolved",
    }
}

fn level_label(level: ResolutionLevel) -> &'static str {
    match level {
        ResolutionLevel::Unresolved => "L0-Unresolved",
        ResolutionLevel::LocalLexical => "L1-LocalLexical",
        ResolutionLevel::EnclosingTypeMember => "L2-EnclosingType",
        ResolutionLevel::ReceiverTypeMember => "L3-ReceiverType",
        ResolutionLevel::ImportBinding => "L4-ImportBinding",
        ResolutionLevel::QualifiedPath => "L5-QualifiedPath",
        ResolutionLevel::SameModule => "L6-SameModule",
        ResolutionLevel::GlobalUnique => "L7-GlobalUnique",
        ResolutionLevel::Heuristic => "L8-Heuristic",
    }
}

/// "判定"列：legacy 与 layered 的关系。
///
/// `primary` 是 layered 最优候选的 `file:line#name`（未解析时为 `-`），
/// 与 legacy 边字符串同一口径，因此"同一目标"可以做纯字符串比较（去掉 `[未解析]` 后缀）。
fn verdict_of(legacy: &[String], resolution: &Resolution, primary: &str) -> &'static str {
    match (&resolution.status, legacy.len()) {
        // legacy 一条边都没产出，layered 也没候选 ⇒ 两边一致（真未解析）。
        (ResolutionStatus::Unresolved, 0) => "一致(双方均无)",
        // legacy 产出了边但 callee 是幽灵节点/误连，layered 判未解析 ⇒ layered 更诚实。
        (ResolutionStatus::Unresolved, _) => "分歧(legacy 有边, layered 判未解析)",
        // layered 判歧义、legacy 却声称已解析 ⇒ 就是本次要消灭的"同名塌缩"。
        (ResolutionStatus::Ambiguous, 1) => "分歧(legacy 塌缩成 1 条, layered 保留全部候选)",
        (ResolutionStatus::Ambiguous, 0) => "分歧(legacy 无边, layered 给出多候选)",
        (ResolutionStatus::Ambiguous, _) => "分歧(legacy 多条边, layered 保留歧义)",
        (ResolutionStatus::Resolved, 1) => {
            let ghost = legacy[0].contains("[未解析]");
            let legacy_target = legacy[0].replace(" [未解析]", "");
            if ghost {
                // legacy 造的是 `namespace="unresolved"` 的幽灵节点边，layered 找到了真实定义。
                "修正(legacy 幽灵边 → layered 真定义)"
            } else if legacy_target == primary {
                "一致(同一目标)"
            } else {
                "分歧(目标不同)"
            }
        }
        (ResolutionStatus::Resolved, 0) => "分歧(legacy 无边, layered 唯一命中)",
        (ResolutionStatus::Resolved, _) => "分歧(legacy 多条边, layered 唯一命中)",
    }
}

/// 打印一张对比表（表头 + 行）。
fn print_table(title: &str, rows: &[Row]) {
    println!("┌──── {title}");
    println!(
        "│ {:<22} {:<26} {:>5} │ {:<34} │ {:<19} {:>7} {:<11} │ {:<24} │ {}",
        "场景", "调用点", "行", "legacy 命中", "layered 层级", "候选", "layered 判定", "layered 最优", "结论"
    );
    println!("│{}", "─".repeat(150));
    for row in rows {
        let legacy = if row.legacy.is_empty() {
            "<无边>".to_string()
        } else {
            row.legacy.join(" , ")
        };
        println!(
            "│ {:<22} {:<26} {:>5} │ {:<34} │ {:<19} {:>7} {:<11} │ {:<24} │ {}",
            truncate(&row.scenario, 22),
            truncate(&row.call_text, 26),
            row.line,
            truncate(&legacy, 34),
            row.layered_level_name,
            row.layered_candidates,
            row.layered_status,
            truncate(&row.layered_primary, 24),
            row.verdict
        );
    }
    println!("└──── 共 {} 行\n", rows.len());
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

// ============================================================================================
// 表 1 + 期望表（7 组场景）
// ============================================================================================

/// 一行期望（任务书"分层规则 / 期望表"的逐行落地）。
struct Expectation {
    scenario: &'static str,
    file: &'static str,
    line: usize,
    level: ResolutionLevel,
    status: ResolutionStatus,
    candidates: Option<usize>,
    /// 最优候选必须是这个 `file:line#name`（None = 不断言具体目标）。
    primary: Option<&'static str>,
    /// 该行为什么这么期望 / 实测不符时的根因说明。
    note: &'static str,
}

/// P2a 期望表：8 行。
fn expected_rows() -> Vec<Expectation> {
    use ResolutionLevel as L;
    use ResolutionStatus as S;
    vec![
        Expectation {
            scenario: "python_import_alias",
            file: "consumer.py",
            line: 10,
            level: L::ImportBinding,
            status: S::Resolved,
            candidates: Some(1),
            primary: Some("mod_x.py:4#func"),
            note: "L4：`from mod_x import func as alias` 的唯一绑定展开成 (mod_x, func) 并在索引里\
                   验证到定义 ⇒ **幽灵节点消失**（legacy 这里产的是 `consumer.py:10#alias [未解析]`，\
                   即图里的 `namespace=\"unresolved\"` 假节点）。"
        },
        Expectation {
            scenario: "rust_qualified_path",
            file: "qualifier.rs",
            line: 9,
            level: L::QualifiedPath,
            status: S::Resolved,
            candidates: Some(1),
            primary: Some("mod_a.rs:3#pick"),
            note: "L5：分量链 `[mod_a]` 与候选 module path **逐段右对齐**相等。\
                   legacy 此处错指 mod_b（first-wins + `namespace.contains(prefix)` 子串匹配）。",
        },
        Expectation {
            scenario: "python_dotted_qualified",
            file: "dotted_consumer.py",
            line: 10,
            level: L::QualifiedPath,
            status: S::Resolved,
            candidates: Some(1),
            primary: Some("mod_y.py:4#helper"),
            note: "L5：`mod_y.helper` 的前缀首段 `mod_y` 既是 import 绑定也是模块段 ⇒ 展开后仍唯一命中\
                   mod_y（`other_mod.helper` 同名但前缀不符）。",
        },
        Expectation {
            scenario: "cross_file_same_name",
            file: "gamma.rs",
            line: 6,
            level: L::Heuristic,
            status: S::Ambiguous,
            candidates: Some(2),
            primary: None,
            note: "无线索（本文件无同名定义、无 receiver、无前缀、无 import）⇒ L1..L7 一律空候选，\
                   到 L8 保留 alpha+beta **两条**候选并判 Ambiguous，绝不 `.first()`。\
                   必须按场景分组建索引才等于 2（同一 fixtures 根建索引会得 4，见文件头说明）。",
        },
        Expectation {
            scenario: "method_receiver",
            file: "receiver_user.rs",
            line: 11,
            level: L::ReceiverTypeMember,
            status: S::Resolved,
            candidates: Some(1),
            primary: Some("calc.rs:9#add"),
            note: "期望表**首选**：`calc: &Calc` 是形参标注（L3 允许的三种推断之一）⇒ 类型 Calc ⇒\
                   精确匹配 `Calc::add`；同名的自由函数 `misc.rs::add` 被排除（成员调用不认自由函数）。\
                   次选（推不出类型则 Ambiguous 2）未触发。",
        },
        Expectation {
            scenario: "nested_function_caller",
            file: "nested.rs",
            line: 12,
            level: L::LocalLexical,
            status: S::Resolved,
            candidates: Some(1),
            primary: Some("nested.rs:6#leaf"),
            note: "callee 唯一；**caller 归属**必须由 `innermost_scope_containing` 得出 `inner`\
                   （由 `test_nested_caller_attribution_uses_scope_tree` 单独断言，不改 legacy 的\
                   `_find_caller_function_by_line`）。",
        },
        Expectation {
            scenario: "nested_function_caller",
            file: "nested.rs",
            line: 20,
            level: L::Unresolved,
            status: S::Unresolved,
            candidates: Some(0),
            primary: None,
            note: "闭包 `doubled`：AST 只产出 `VariableDefinition`（本轮不修，口径 D/A），\
                   全仓无名为 doubled 的函数 ⇒ level=0 Unresolved；图里那个幽灵节点保持原样不删。",
        },
        Expectation {
            scenario: "ambiguous_trait_impl",
            file: "driver.rs",
            line: 8,
            level: L::Heuristic,
            status: S::Ambiguous,
            candidates: Some(5),
            primary: None,
            note: "`runner: &dyn Runner` ⇒ L3 唯一命中是 **trait 声明（无函数体）** ⇒ 按规则拒绝唯一判定并下沉；\
                   最终 L8 保留全部 5 个同名 `run` 候选（trait/Fast/Slow/Loud/Loudish），\
                   绝不坍缩成单指 `driver.rs::run`。",
        },
    ]
}

#[test]
fn p2a_layered_engine_matches_expected_table_over_resolution_fixtures() {
    let config = ResolutionConfig::all();
    let mut mismatches: Vec<String> = Vec::new();
    let mut all_rows: Vec<Row> = Vec::new();

    for scenario in SCENARIOS {
        let built = Scenario::build(scenario, &format!("{RESOLUTION_ROOT}/{scenario}"));
        let rows = built.rows(&config);
        all_rows.extend(rows.iter().cloned());

        for expect in expected_rows()
            .into_iter()
            .filter(|e| e.scenario == *scenario)
        {
            let resolution = built.resolve(expect.file, expect.line, &config);
            let level = resolution
                .primary()
                .map(|c| c.level)
                .unwrap_or(ResolutionLevel::Unresolved);
            let primary = if resolution.status == ResolutionStatus::Unresolved {
                "-".to_string()
            } else {
                built.describe_candidate(&resolution, 0)
            };
            let mut problems: Vec<String> = Vec::new();
            if level != expect.level {
                problems.push(format!(
                    "层级：期望 {}，实测 {}",
                    level_label(expect.level),
                    level_label(level)
                ));
            }
            if resolution.status != expect.status {
                problems.push(format!(
                    "状态：期望 {}，实测 {}",
                    status_name(&expect.status),
                    status_name(&resolution.status)
                ));
            }
            if let Some(count) = expect.candidates {
                if resolution.total_candidate_count != count {
                    problems.push(format!(
                        "候选数：期望 {count}，实测 {}",
                        resolution.total_candidate_count
                    ));
                }
            }
            if let Some(want) = expect.primary {
                if !primary.ends_with(want) {
                    problems.push(format!("最优候选：期望 …{want}，实测 {primary}"));
                }
            }
            if problems.is_empty() {
                println!(
                    "PASS  {:<24} {:>3}:{:<3} {:<18} → {:<19} 候选={} {:<10} {}",
                    format!("{}/{}", expect.scenario, expect.file),
                    expect.line,
                    "",
                    call_text(&built.site(expect.file, expect.line).1.clone()),
                    level_label(level),
                    resolution.total_candidate_count,
                    status_name(&resolution.status),
                    primary
                );
            } else {
                mismatches.push(format!(
                    "{}::{}:{} —— {}",
                    expect.scenario,
                    expect.file,
                    expect.line,
                    problems.join("；")
                ));
            }
        }
    }

    println!("══════════════════════════════════════════════════════════════════════════════");
    print_table("表 1｜7 组场景 × 全部调用点（legacy vs layered，全层开启）", &all_rows);

    assert!(
        mismatches.is_empty(),
        "期望表与实测不符（**不改期望迁就实现**，逐条列差异与根因）：\n{}\n",
        mismatches
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}. {m}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// 期望表里每行的"为什么"（把 `Expectation::note` 打印出来，供人工复核差异根因）。
#[test]
fn p2a_print_expectation_notes_and_legacy_comparison() {
    println!("════ Phase P2a 期望表逐行判据 ════");
    for expect in expected_rows() {
        println!(
            "\n[{}] {}:{}  →  期望 {} / {} / 候选{} / 最优{}",
            expect.scenario,
            expect.file,
            expect.line,
            level_label(expect.level),
            status_name(&expect.status),
            expect
                .candidates
                .map(|c| c.to_string())
                .unwrap_or_else(|| "不限".to_string()),
            expect.primary.unwrap_or("不限"),
        );
        println!("  判据：{}", expect.note);
    }
}

// ============================================================================================
// 表 2：4 个整仓小项目 × 抽样（前 20 行）
// ============================================================================================

#[test]
fn p2a_engine_runs_over_whole_repos_and_prints_sample_table() {
    let config = ResolutionConfig::all();
    let mut all: Vec<Row> = Vec::new();
    let mut stats: Vec<(String, usize, usize, usize)> = Vec::new();

    for repo in REPOS {
        let built = Scenario::build(repo, &format!("{REPOS_ROOT}/{repo}"));
        let rows = built.rows(&config);
        let resolved = rows.iter().filter(|r| r.layered_status == "Resolved").count();
        let ambiguous = rows
            .iter()
            .filter(|r| r.layered_status == "Ambiguous")
            .count();
        println!(
            "仓库 {:<24} 调用点={:<6} layered Resolved={:<6} Ambiguous={:<6} 索引函数={}",
            repo,
            rows.len(),
            resolved,
            ambiguous,
            built.index.len()
        );
        stats.push((
            repo.to_string(),
            rows.len(),
            resolved,
            ambiguous,
        ));
        all.extend(rows);
    }

    println!("══════════════════════════════════════════════════════════════════════════════");
    // 抽样口径：**每个仓库各取前 5 行**（合计 20 行），保证四个语言面都露头，
    // 而不是被调用点最多的 `simple_js_project`（6080 个调用点）挤满整张表。
    let mut sample: Vec<Row> = Vec::new();
    for repo in REPOS {
        sample.extend(all.iter().filter(|r| r.scenario == *repo).take(5).cloned());
    }
    print_table("表 2｜4 个整仓 × 抽样（每仓前 5 行 = 20 行，legacy vs layered）", &sample);
    println!(
        "整仓汇总：{}",
        stats
            .iter()
            .map(|(n, total, r, a)| format!("{n}={total}调用点/解析{r}/歧义{a}"))
            .collect::<Vec<_>>()
            .join("  ")
    );

    // 整仓只要求"引擎不崩、每行都有合法判定"，不锁具体目标（那些数字是 P2b 的分叉基线，
    // 需要连同 `test_p1_edges_unchanged.rs` 的守卫表一起改，见报告"P2b 接手清单"）。
    assert!(
        !all.iter().any(|r| r.layered_status == "Unresolved" && r.layered_candidates > 0),
        "Unresolved 的结果不得带真实候选（幽灵占位不计入 total_candidate_count）"
    );
    for (repo, total, _, _) in &stats {
        assert!(
            *total > 0,
            "{repo} 一个调用点都没构造出来，说明 call_sites_from_symbols 对该语言失效"
        );
    }
}

// ============================================================================================
// gamma.rs：Ambiguous 2 候选 + 与种子无关
// ============================================================================================

#[test]
fn p2a_gamma_helper_is_ambiguous_with_exactly_alpha_and_beta() {
    let config = ResolutionConfig::all();
    let built = Scenario::build("cross_file_same_name", &format!("{RESOLUTION_ROOT}/cross_file_same_name"));
    let resolution = built.resolve("gamma.rs", 6, &config);

    assert_eq!(resolution.status, ResolutionStatus::Ambiguous, "gamma.rs 的 helper() 必须判歧义");
    assert_eq!(
        resolution.total_candidate_count, 2,
        "候选必须是 alpha.rs::helper + beta.rs::helper 两条（分组建索引口径）"
    );
    let mut targets: Vec<String> = (0..resolution.candidates.len())
        .map(|i| built.describe_candidate(&resolution, i))
        .collect();
    targets.sort();
    assert!(
        targets.iter().any(|t| t.ends_with("alpha.rs:4#helper")) && targets.iter().any(|t| t.ends_with("beta.rs:4#helper")),
        "两条同名候选都必须保留，实测 = {targets:?}"
    );
    // legacy 在同一调用点只产 1 条边并声称已解析 —— 这正是"同名塌缩"缺陷。
    let legacy = built.legacy_edges();
    let gamma_line = built.site("gamma.rs", 6).0.path.clone();
    let legacy_edges = legacy.get(&(gamma_line, 6)).cloned().unwrap_or_default();
    assert_eq!(
        legacy_edges.len(),
        1,
        "legacy 在该调用点应当且只应当产 1 条边（对比项），实测 = {legacy_edges:?}"
    );
    println!("gamma.rs:6 helper() layered → {resolution:?}\n  legacy → {legacy_edges:?}");
}

/// **确定性**：同一输入连解两次、同一目录连建两次索引，结果集必须完全相同。
///
/// 这是任务"排序键 (level, rank, uuid) 保证跨进程确定性"的落地验证：
/// P1 实测 `cross_file_edges` 随 HashMap 种子抖动（simple_ts 7↔13、simple_js 31↔35），
/// 本测试用**不含 uuid 的身份投影**（`file:line#name` + 层级 + 状态 + 候选数）比较两次
/// 独立建图，从而把"候选集合是否稳定"与"每次重建都会生成新 uuid"这两件事分开。
#[test]
fn p2a_resolution_is_deterministic_across_rebuilds_and_repeats() {
    let config = ResolutionConfig::all();

    // ---- (a) 同一份输入连解两次：逐字段相等（含 uuid，因为索引与候选 id 都来自同一次建图）----
    let built = Scenario::build("ambiguous_trait_impl", &format!("{RESOLUTION_ROOT}/ambiguous_trait_impl"));
    let first = built.resolve("driver.rs", 8, &config);
    let second = built.resolve("driver.rs", 8, &config);
    assert_eq!(
        format!("{first:?}"),
        format!("{second:?}"),
        "同一输入两次 resolve_call 必须逐字段相等"
    );

    // ---- (b) 同一目录独立建两次图/索引：uuid-free 投影必须完全相同 ----
    let mut mismatched: Vec<String> = Vec::new();
    for name in ["cross_file_same_name", "ambiguous_trait_impl", "rust_qualified_path"] {
        let a = Scenario::build(name, &format!("{RESOLUTION_ROOT}/{name}"));
        let b = Scenario::build(name, &format!("{RESOLUTION_ROOT}/{name}"));
        let projection_a = built_projection(&a, &config);
        let projection_b = built_projection(&b, &config);
        if projection_a != projection_b {
            mismatched.push(format!(
                "[{name}] 两次独立建图后的 layered 结果集不同：\n  A={:?}\n  B={:?}",
                projection_a, projection_b
            ));
        }
    }
    assert!(mismatched.is_empty(), "{}", mismatched.join("\n"));

    // ---- (c) `simple_ts_project` / `simple_js_project` 各建索引两次，by_name 每个 Vec 排序后相等 ----
    for repo in ["simple_ts_project", "simple_js_project"] {
        let a = Scenario::build(repo, &format!("{REPOS_ROOT}/{repo}"));
        let b = Scenario::build(repo, &format!("{REPOS_ROOT}/{repo}"));
        let mut keys_a: Vec<String> = a.index.by_name().keys().cloned().collect();
        let mut keys_b: Vec<String> = b.index.by_name().keys().cloned().collect();
        keys_a.sort();
        keys_b.sort();
        assert_eq!(keys_a, keys_b, "[{repo}] 两次建索引的 by_name 键集合必须相同");
        for key in &keys_a {
            let va = identity_of_tags(a.index.candidates_by_name(key));
            let vb = identity_of_tags(b.index.candidates_by_name(key));
            assert_eq!(
                va, vb,
                "[{repo}] 名字 `{key}` 的候选集合在两次独立建图后不一致（P1 的种子抖动类缺陷）"
            );
        }
    }

    println!("══════════════════════════════════════════════════════════════════════════════");
    println!("┌──── 表 4｜确定性矩阵（同一输入/同一目录重复计算，比较 uuid-free 投影）");
    println!("│ {:<40} {:<18} {}", "比较项", "口径", "结果");
    println!("│{}", "─".repeat(110));
    println!(
        "│ {:<40} {:<18} {} （同进程同索引，逐字段相等，含候选顺序与 rank）",
        "driver.rs:8 `runner.run()` 连解两次", "全字段", "结果=全等"
    );
    println!(
        "│ {:<40} {:<18} {} （两次独立建图，(调用点,层级,状态,候选数,候选身份) 全等）",
        "3 场景 × 两次重建 layered 结果集", "uuid-free 投影", "结果=全等"
    );
    println!(
        "│ {:<40} {:<18} {} （by_name 键集合 + 每个同名候选序列排序后全等）",
        "simple_ts / simple_js × 两次建索引", "候选身份 (文件:行#名 / module)", "结果=全等"
    );
    println!("└──── 表 4 结束：本阶段引擎的跨进程确定性由 `(level, 排序子键, 源码事实, uuid)` 四段排序键保证");
}

/// layered 结果的 uuid-free 投影：`(调用点文件:行, 层级, 状态, 候选数, 候选身份升序)`。
fn built_projection(scenario: &Scenario, config: &ResolutionConfig) -> Vec<(String, u8, String, usize, Vec<String>)> {
    let mut out = Vec::new();
    for file in &scenario.files {
        for site in &file.sites {
            let resolution =
                resolve_call_with_config(site, Some(file.ctx.as_ref()), &scenario.index, config);
            let level = resolution
                .primary()
                .map(|c| c.level.as_u8())
                .unwrap_or(0);
            let identities: Vec<String> = (0..resolution.candidates.len())
                .map(|i| scenario.describe_candidate(&resolution, i))
                .collect();
            out.push((
                format!("{}:{}", scenario.relative(&file.path), site.line),
                level,
                status_name(&resolution.status).to_string(),
                resolution.total_candidate_count,
                identities,
            ));
        }
    }
    out.sort();
    out
}

/// 候选身份：`(相对文件的尾两级, 行, 名字, module_path)` —— **不含 uuid**，因此可跨进程比较。
fn identity_of_tags(tags: &[codeseek::codegraph::resolution::FunctionTag]) -> Vec<String> {
    let mut out: Vec<String> = tags
        .iter()
        .map(|tag| {
            format!(
                "{}:{}#{}|{}",
                tag.file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?"),
                tag.line_start,
                tag.name,
                tag.module_path
            )
        })
        .collect();
    out.sort();
    out
}

// ============================================================================================
// 表 3：逐层定位（以 rust_qualified_path 为样例）
// ============================================================================================

#[test]
fn p2a_level_localization_on_rust_qualified_path() {
    let scenario = Scenario::build(
        "rust_qualified_path",
        &format!("{RESOLUTION_ROOT}/rust_qualified_path"),
    );

    let all = ResolutionConfig::all();
    let without_l5 = all.with_level(ResolutionLevel::QualifiedPath, false);
    let only_l5 = ResolutionConfig::from_levels_spec("l5").expect("`--levels=l5` 必须可解析");
    let none = ResolutionConfig::from_levels_spec("none").expect("`--levels=none` 必须可解析");

    // 关掉 L5 必须退化：bitmask 校验 + 结果不再落在 L5、不再唯一命中 mod_a。
    assert_eq!(
        without_l5.enabled_levels & ResolutionConfig::bit(ResolutionLevel::QualifiedPath),
        0,
        "`with_level(L5,false)` 必须清掉 L5 的 bit"
    );
    let degraded = scenario.resolve("qualifier.rs", 9, &without_l5);
    assert_ne!(
        degraded
            .primary()
            .map(|c| c.level)
            .unwrap_or(ResolutionLevel::Unresolved),
        ResolutionLevel::QualifiedPath,
        "关掉 L5 之后不得再产出 QualifiedPath 候选（退化为歧义/其它层）"
    );
    assert_eq!(
        degraded.status,
        ResolutionStatus::Ambiguous,
        "关掉 L5 后 `pick` 有两个同名候选，必须判歧义而不是猜一个：{degraded:?}"
    );

    // 只开 L5 必须正确。
    let precise = scenario.resolve("qualifier.rs", 9, &only_l5);
    assert_eq!(precise.status, ResolutionStatus::Resolved, "只开 L5 必须仍解析到 mod_a::pick");
    assert_eq!(
        precise
            .primary()
            .map(|c| c.level)
            .unwrap_or(ResolutionLevel::Unresolved),
        ResolutionLevel::QualifiedPath
    );
    assert_eq!(precise.total_candidate_count, 1);
    assert!(
        scenario.describe_candidate(&precise, 0).ends_with("mod_a.rs:3#pick"),
        "只开 L5 的最优候选必须是 mod_a.rs::pick，实测 = {}",
        scenario.describe_candidate(&precise, 0)
    );

    // 全关：任何调用点都只能 Unresolved（证明"层级来源"确实是引擎，而不是别的路径）。
    let off = scenario.resolve("qualifier.rs", 9, &none);
    assert_eq!(off.status, ResolutionStatus::Unresolved, "全关必须得到 Unresolved");

    // 打印表 3：同一调用点在若干配置下的表现。
    let specs: [(&str, Option<ResolutionConfig>); 7] = [
        ("all（默认全开）", Some(all)),
        ("all,-l5（关掉限定名层）", Some(without_l5)),
        ("l5（只开限定名层）", Some(only_l5)),
        ("l1（只开词法层）", ResolutionConfig::from_levels_spec("l1")),
        ("l7（只开全仓唯一层）", ResolutionConfig::from_levels_spec("l7")),
        ("p1（只开 P1 的 L1+L7）", ResolutionConfig::from_levels_spec("p1")),
        ("none（全关）", Some(none)),
    ];
    println!("══════════════════════════════════════════════════════════════════════════════");
    println!("┌──── 表 3｜逐层定位矩阵（场景 rust_qualified_path，调用点 qualifier.rs:9 `mod_a::pick(x)`）");
    println!("│ {:<26} {:>9} {:>7} {:<11} {:<24} {}", "--levels=", "层级", "候选", "状态", "最优候选", "备注");
    println!("│{}", "─".repeat(120));
    for (label, config) in specs {
        let Some(config) = config else {
            println!("│ {:<26} —— 配置串无法解析（应当报错，不得静默忽略）", label);
            continue;
        };
        let resolution = scenario.resolve("qualifier.rs", 9, &config);
        let level = resolution
            .primary()
            .map(|c| level_label(c.level))
            .unwrap_or("L0-Unresolved");
        println!(
            "│ {:<26} {:>9} {:>7} {:<11} {:<24} enabled_levels=0x{:03X}",
            label,
            level,
            resolution.total_candidate_count,
            status_name(&resolution.status),
            scenario.primary_text(&resolution),
            config.enabled_levels
        );
    }
    println!("└──── 表 3 结束\n");
}

// ============================================================================================
// caller 归属：必须由 innermost_scope_containing 得出（P0 基线靠产出顺序巧合）
// ============================================================================================

#[test]
fn p2a_nested_caller_attribution_uses_innermost_scope_not_line_first_wins() {
    let scenario = Scenario::build(
        "nested_function_caller",
        &format!("{RESOLUTION_ROOT}/nested_function_caller"),
    );
    let (file, site) = scenario.site("nested.rs", 12);

    // (1) `innermost_scope_containing(12)` 命中的作用域必须是 `inner`（第 11..13 行）。
    let scope_id = file
        .ctx
        .innermost_scope_containing(12)
        .expect("nested.rs:12 必须落在某个作用域里");
    let scope = file.ctx.scopes.scope(scope_id).expect("作用域必须存在");
    assert_eq!(scope.line_start, 11, "最内层作用域应是从第 11 行开始的 `inner`");

    // (2) 调用点构造时已经把它记进 `CallSite::caller_scope`（为 P2b 替换
    //     `_find_caller_function_by_line` 铺路；本阶段不改那条管线）。
    assert_eq!(
        site.caller_scope,
        Some(scope_id),
        "call_sites_from_symbols 必须用 innermost_scope_containing 填 caller_scope"
    );

    // (3) 可读名称也必须是 `inner`（而不是外层 `outer`）。
    assert_eq!(
        file.ctx.enclosing_function_at(12).as_deref(),
        Some("inner"),
        "第 12 行的最内层函数作用域名必须是 inner"
    );
    println!(
        "caller 归属：nested.rs:12 → 作用域 `{}:{}`（kind={:?}, depth={}），外层 = {:?}",
        scope.name,
        scope.line_start,
        scope.kind,
        scope.depth,
        file.ctx.scopes.scope(scope.parent.expect("inner 有父作用域")).map(|p| p.name.clone())
    );

    // (4) 闭包 `doubled(x)`（第 20 行）：AST 只产出 VariableDefinition ⇒ 本轮不修，
    //     layered 必须显式判 Unresolved（口径 A），同时图里的幽灵节点不许被动过。
    let closure = scenario.resolve("nested.rs", 20, &ResolutionConfig::all());
    assert_eq!(closure.status, ResolutionStatus::Unresolved);
    assert_eq!(closure.total_candidate_count, 0);
    let ghosts = scenario
        .graph
        .get_all_functions()
        .into_iter()
        .filter(|f| f.namespace == "unresolved")
        .count();
    assert_eq!(
        ghosts, 1,
        "图里的闭包幽灵节点必须原样保留（红线 1：P2a 不改任何建图产出）"
    );
}

// ============================================================================================
// 交付物 2(c)：索引**仅凭图节点**构建 —— 被 MD5 跳过的文件仍必须进候选集（口径 C）
// ============================================================================================
/// `tests/fixtures/resolution/cross_file_same_name` 走**两次**建图：
/// 第二次不 `force_reindex`，于是内容未变的文件被 MD5 跳过（`file_scope_ctx` 返回 `None`），
/// 但它们的函数节点仍在图里，`build_resolution_index_from_graph` 仍必须把它们当候选来源；
/// 而 `ensure_file_scope_ctx` 必须**按需补建**缺失的 ctx（不得降级成"只用不需要 ctx 的层"）。
#[test]
fn p2a_incremental_skipped_files_still_feed_index_and_ctx_can_be_rebuilt() {
    let scenario_name = "cross_file_same_name";
    let temp = tempfile::TempDir::new().expect("临时目录");
    let root = temp
        .path()
        .join(format!("p2a_skip_{scenario_name}_{}", std::process::id()));
    copy_tree(
        &manifest_dir().join(format!("{RESOLUTION_ROOT}/{scenario_name}")),
        &root,
    );

    // ---- 第一轮：全量建图（三个文件都被解析，ctx 齐全）----
    let mut parser = CodeParser::new();
    parser.set_force_reindex(true);
    let graph = parser
        .build_petgraph_code_graph(&root)
        .expect("第一轮建图失败");
    let gamma = root.join("gamma.rs");
    let alpha = root.join("alpha.rs");
    assert!(
        parser.file_scope_ctx(&alpha).is_some() && parser.file_scope_ctx(&gamma).is_some(),
        "全量建图后每个文件都应有 ctx"
    );

    // ============================================================================================
    // 第二轮：不置 `force_reindex` ⇒ 文件哈希未变 ⇒ 每个文件都必须被 `_should_skip_file` 跳过。
    //
    // ⚠ Phase P2b 收口对**本用例前置状态**的调整（红线 2 要求逐条论证；断言一条都没放宽）：
    // 改造前这里把新 parser 直接丢进"只有文件哈希、没有图缓存"的磁盘状态，然后断言
    // `file_scope_count() == 0`（以及下面那个 `nodes_skip == 0 && edges_skip == 0` 的分支）。
    // 那个状态是 **P0 遗留缺陷**造出来的非法增量态：`build_petgraph_code_graph` 只把哈希
    // 落盘（`_save_file_hashes`），图由另一条链路落盘（真实 CLI 是 `main.rs` 的
    // `persistence.save_graph(...)` + `codegraph::repository::save_state()`）。
    // P2b 口径 D（已批准，并由
    // `tests/test_p2b_wiring.rs::p2b_second_round_without_saved_graph_must_not_be_empty` 锁死）
    // 规定"图缓存缺失 ⇒ 哈希缓存一律作废 ⇒ 全量重解析"，于是这一轮会解析 3 个文件、
    // `file_scope_count()` 变成 3 —— 也就是说，原来的两条断言之所以成立，靠的是**缺陷**，
    // 而不是"哈希命中即跳过"这条契约本身。
    //
    // 处置：把磁盘状态摆成真实 CLI 跑完第一轮之后的样子（**哈希 + 图都在**），让跳过契约
    // 重新可达，于是 `file_scope_count() == 0` / `file_scope_ctx(alpha).is_none()` **原样保留**；
    // 另加一条 `last_round_file_counts() == (0, 3)` 把"确实一个文件都没多解析"钉死，
    // 不再依赖"空图"这个副作用。
    // ============================================================================================
    {
        let name_id = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .expect("临时项目目录必须有目录名");
        // 第一轮用的是 `CodeParser::new()` 的**默认档位**（Phase P3 起 = layered，
        // 候选上限 8），所以这里的戳必须一致，否则第二轮 `_load_existing_graph` 的
        // 档位门禁会把刚种下的图判成失效 ⇒ 本用例要钉的"哈希命中即跳过"就复现不出来。
        codeseek::storage::PersistenceManager::new()
            .save_graph(&name_id, &graph, "layered", 8)
            .expect("图缓存落盘失败（构造合法增量态）");
    }
    let mut incremental = CodeParser::new();
    let graph2 = incremental
        .build_petgraph_code_graph(&root)
        .expect("增量建图失败");
    assert_eq!(
        incremental.last_round_file_counts(),
        (0, 3),
        "口径 A：第二轮必须『重新解析 0 个文件 / 跳过 3 个文件』，\
         绝不允许为了喂分层索引而重新解析被哈希跳过的文件，实际 {:?}",
        incremental.last_round_file_counts()
    );
    assert_eq!(
        incremental.file_scope_count(),
        0,
        "被 MD5 跳过的文件不会重新解析 ⇒ 新 parser 的 `file_scopes` 必须是空的（P2a 已知边界）"
    );
    assert!(
        incremental.file_scope_ctx(&alpha).is_none(),
        "`file_scope_ctx` 对跳过的文件返回 None（正是 `ensure_file_scope_ctx` 要补的场景）"
    );
    let (nodes_full, edges_full) = (graph.get_all_functions().len(), graph.graph.edge_count());
    let (nodes_skip, edges_skip) = (graph2.get_all_functions().len(), graph2.graph.edge_count());
    println!(
        "第二轮（增量，哈希命中 + 图缓存命中）：file_scope_count={}，节点 {}→{}，边 {}→{}",
        incremental.file_scope_count(),
        nodes_full,
        nodes_skip,
        edges_full,
        edges_skip
    );
    // 改造前这里是 `if nodes_skip != nodes_full { assert_eq!((nodes_skip, edges_skip), (0, 0)) }`
    // —— 那条断言把"P0 空图缺陷"钉成了现状（原注释也自述"若不是空图请同步本断言"）。
    // 口径 D 修复 + 本用例的前置状态修正之后，合法增量态下第二轮读回图缓存，节点/边必须
    // **与非 force 第一轮完全相同**（既不丢节点、也不叠边），这才是"跳过 = 保留已有图"的
    // 真实契约，等价强度且不再依赖缺陷。
    assert_eq!(
        (nodes_skip, edges_skip),
        (nodes_full, edges_full),
        "哈希命中 + 图缓存命中时，增量轮不得丢节点/丢边（跳过 = 保留已有图）"
    );

    // **口径 C 的第一半**：索引**仅凭图节点**构建 —— 一个 ctx 都没有（等价于"文件被跳过"）时，
    // 图里已有的函数节点仍必须成为候选来源。这里用第一轮那份**有节点**的图，
    // 传空 ctx map，精确复现"被跳过文件仍要进候选集"的要求。
    let empty_ctx_map: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
    let index_without_any_ctx =
        build_resolution_index_from_graph(&graph, &empty_ctx_map, Some(root.as_path()));
    let helper_candidates = index_without_any_ctx.candidates_by_name("helper");
    assert_eq!(
        helper_candidates.len(),
        2,
        "被跳过文件（alpha.rs/beta.rs）里的 `helper` 必须照样进候选集（仅凭图节点构建）"
    );
    assert!(
        helper_candidates
            .iter()
            .all(|tag| tag.enclosing_type.is_none() && tag.param_count.is_none()),
        "没有任何 ctx 时，`param_count`/`enclosing_type` 必须留中性值（后果是**下沉**，绝不是误命中）"
    );
    // 中性值的后果必须真的是"下沉"，而不是把 L2/L3 变成假候选：同一个调用点在两种索引下
    // 的判定都必须是 Ambiguous（两条同名候选），层级都不许升到 L2/L3。
    let config = ResolutionConfig::all();
    let gamma_ctx = parser
        .ensure_file_scope_ctx(&gamma)
        .expect("gamma.rs 的 ctx 必须拿得到");
    let ts = TreeSitterParser::new();
    let ast = ts.parse_file_with_metadata(&gamma).expect("解析 gamma.rs");
    let source = std::fs::read_to_string(&gamma).unwrap_or_default();
    let sites = call_sites_from_symbols_with_source(&ast.symbols, &gamma_ctx, Some(&source));
    let helper_site = sites
        .iter()
        .find(|s| s.line == 6)
        .expect("gamma.rs:6 必须有一个调用点");
    for (label, probe_index) in [
        ("全 ctx", {
            let mut map: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
            for path in collect_source_files(&root) {
                if let Some(ctx) = parser.ensure_file_scope_ctx(&path) {
                    map.insert(path, ctx);
                }
            }
            build_resolution_index_from_graph(&graph, &map, Some(root.as_path()))
        }),
        ("零 ctx", index_without_any_ctx.clone()),
    ] {
        let resolution =
            resolve_call_with_config(helper_site, Some(gamma_ctx.as_ref()), &probe_index, &config);
        assert_eq!(
            resolution.status,
            ResolutionStatus::Ambiguous,
            "[{label}] gamma.rs:6 必须是 Ambiguous（缺 ctx 不得把歧义伪装成唯一命中）"
        );
        assert_eq!(
            resolution
                .primary()
                .map(|c| c.level)
                .unwrap_or(ResolutionLevel::Unresolved),
            ResolutionLevel::Heuristic,
            "[{label}] 缺 ctx 时唯一正确的层级是 L8-Heuristic，实测 = {:?}",
            resolution.primary().map(|c| c.level)
        );
    }

    // **口径 C 的第二半**：缺失的 ctx 用 `ensure_file_scope_ctx` **按需补建**，
    // 补出来的必须与全量建图时同口径（不许降级成"只用不需要 ctx 的层"）。
    let rebuilt = incremental
        .ensure_file_scope_ctx(&alpha)
        .expect("ensure_file_scope_ctx 必须能按需补建被 MD5 跳过文件的 ctx");
    assert_eq!(
        rebuilt.module_path,
        parser
            .file_scope_ctx(&alpha)
            .expect("全量 ctx")
            .module_path,
        "补建的 ctx 与全量建图时的 ctx 同口径（module path）"
    );
    assert!(
        !rebuilt.scopes.is_empty(),
        "补建的 ctx 必须有作用域树：{}",
        alpha.display()
    );
    assert!(
        rebuilt.callable_facts().iter().any(|fact| fact.name == "helper"),
        "补建的 ctx 必须带 AST 事实（helper 的 param_count/成员身份从这里回填）"
    );
    // 第二次调用必须命中缓存（同一份 Arc，绝不重复解析）。
    let again = incremental
        .ensure_file_scope_ctx(&alpha)
        .expect("缓存里应当已有 alpha.rs 的 ctx");
    assert!(
        Arc::ptr_eq(&rebuilt, &again),
        "`ensure_file_scope_ctx` 命中缓存时不得重新解析（同一 Arc）"
    );
    // 补建只写 `file_scopes`，绝不产生节点/边：全量那份图的规模必须不受影响。
    assert_eq!(
        (nodes_full, edges_full),
        (graph.get_all_functions().len(), graph.graph.edge_count()),
        "ensure_file_scope_ctx 不得改变图规模（红线 1）"
    );

    // 清理：删掉本次写进 ~/.codeseek 的项目目录。
    for id in [
        format!("{:x}", md5::compute(root.to_string_lossy().as_bytes())),
        root.file_name().unwrap().to_string_lossy().to_string(),
    ] {
        cleanup_codeseek_project(&id);
    }
}
