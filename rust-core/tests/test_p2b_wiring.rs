//! Phase P2b 交付物 4：接线之后的**全量 / 增量一致性**、**跨进程确定性**，
//! 以及**口径 D**（"只落哈希不落图 ⇒ 空图"）的回归锁。
//!
//! ## 为什么这三条要放在一起
//! P2b 把五处 first-wins 翻成分层解析器之后，最容易出的两类事故是：
//! 1. **同一份源码两次建图结果不同**（legacy 的老毛病：取值取决于 `HashMap` 的每进程
//!    随机种子，P1 实测 `simple_ts` 的跨文件边 7↔13、`simple_js` 31↔35 抖动）；
//! 2. **全量与增量不一致**（增量链路少建了 ctx / 少写了边 / 叠了边）。
//! 这两类事故都会让"影响面分析"给出随运行次数变化的答案，必须用测试钉死。
//!
//! ## 口径 D 的回归锁（本文件第一个用例）
//! `build_petgraph_code_graph` 的第 3 步读 `file_hashes.json`、第 1 步读 `graph.bin/json`，
//! 而**只有第 8 步会把哈希写回去**（图由 `codegraph::repository::save_state()` 另行落盘）。
//! 于是在"存了哈希、没存图"的状态下再跑一次：
//! 读不到图 → `has_existing_data = false` → 走全量分支，
//! 但所有文件都被 `_should_skip_file` 跳过 → `self.file_functions` 为空 → **空图**。
//! 该缺陷**先于本次改造存在**（P0 的 `851f7fb` 与 P2a 的 `8e62033` 都复现，
//! 见 P2b 报告里的 stash 证据），P2b 按任务口径 D 一并修掉：
//! **图缓存缺失时一律无视哈希缓存**（两者必须同生同死）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use codeseek::cli::args::ResolverMode;
use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::types::{CallRelation, PetCodeGraph};

const RESOLUTION_ROOT: &str = "tests/fixtures/resolution";
const REPOS_ROOT: &str = "tests/test_repos";

// ─── 基础设施 ───────────────────────────────────────────────────

/// 一次建图的产物。`temp` 必须比 `project_dir` 活得久，所以一起装在这里。
struct Build {
    _temp: tempfile::TempDir,
    project_dir: PathBuf,
    graph: PetCodeGraph,
}

impl Drop for Build {
    fn drop(&mut self) {
        // 本用例故意**不**带 `--force-reindex`（要的就是缓存行为），
        // 所以必须清掉写进 `~/.codeseek/projects/` 的目录，否则别的用例会看到脏缓存。
        let id_by_md5 = format!(
            "{:x}",
            md5::compute(self.project_dir.to_string_lossy().as_bytes())
        );
        cleanup_codeseek_project(&id_by_md5);
        if let Some(name) = self.project_dir.file_name() {
            cleanup_codeseek_project(&name.to_string_lossy());
        }
    }
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

/// 清掉某个项目目录的两份缓存（哈希 + 图），把"第二轮"的起点摆平。
fn clean_all_caches(project_dir: &Path) {
    let id_by_md5 = format!("{:x}", md5::compute(project_dir.to_string_lossy().as_bytes()));
    cleanup_codeseek_project(&id_by_md5);
    if let Some(name) = project_dir.file_name() {
        cleanup_codeseek_project(&name.to_string_lossy());
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

/// 把 fixture 目录复制进唯一命名的临时项目目录。
///
/// 目录名里带 `counter`：同一次 `cargo test` 里多个用例共用一个进程 id，
/// 不加区分就会互相看到对方的缓存（正是口径 D 那类"缓存串味"问题）。
fn staged(rel_src: &str, label: &str, counter: usize) -> (tempfile::TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_src);
    assert!(src.is_dir(), "fixture 目录不存在: {}", src.display());
    let temp = tempfile::TempDir::new().expect("创建临时目录失败");
    let project_dir = temp
        .path()
        .join(format!("p2b{}_{}_{}", label, counter, std::process::id()));
    copy_tree(&src, &project_dir);
    (temp, project_dir)
}

/// 一次建图（`force_reindex` 可控 —— 口径 D 的用例必须走 `false`）。
fn build_once(
    rel_src: &str,
    label: &str,
    counter: usize,
    layered: bool,
    force_reindex: bool,
    max_candidates: usize,
) -> Build {
    let (_temp, project_dir) = staged(rel_src, label, counter);
    let mut parser = CodeParser::new();
    parser.set_force_reindex(force_reindex);
    // Phase P3 交付物 3：`layered=false` 必须**显式**钉 Legacy，不得再依赖全局默认值
    // （P3 起默认已切 layered）；这样本文件所有"legacy 产边=32/13"的红线断言在默认翻转后
    // 依旧逐字节成立，而默认档本身的行为改由 [`p3_default_resolver_is_layered`] 单测。
    if layered {
        parser.set_resolver_mode(ResolverMode::Layered);
        parser.set_max_candidates(max_candidates);
    } else {
        parser.set_resolver_mode(ResolverMode::Legacy);
    }
    let graph = parser
        .build_petgraph_code_graph(&project_dir)
        .unwrap_or_else(|e| panic!("[{label}] 建图失败: {e}"));
    Build {
        _temp,
        project_dir,
        graph,
    }
}

/// 对**同一个**项目目录再建一次图（新 parser 实例 ⇒ 只可能通过磁盘缓存交流）。
fn rebuild_in_same_dir(previous: &Build, layered: bool, max_candidates: usize) -> PetCodeGraph {
    let mut parser = CodeParser::new();
    // 关键：不带 --force-reindex，走的就是"读缓存"的真实增量路径。
    parser.set_force_reindex(false);
    if layered {
        parser.set_resolver_mode(ResolverMode::Layered);
        parser.set_max_candidates(max_candidates);
    } else {
        // P3：显式钉 Legacy，不依赖已翻转的全局默认值。
        parser.set_resolver_mode(ResolverMode::Legacy);
    }
    parser
        .build_petgraph_code_graph(&previous.project_dir)
        .unwrap_or_else(|e| panic!("第二轮建图失败: {e}"))
}

// ─── 边的规范化多重集（含 4 个新字段，不含 uuid）────────────────

/// 一条边的"源码事实 + 解析元信息"投影：
/// `(caller 相对文件, caller 名, caller 起始行, callee 相对文件, callee 名, callee 起始行,
///   调用行, callee_name 字段, is_resolved, resolution_level, confidence, rank, candidate_count)`
///
/// 刻意不含 `Uuid`：节点 id 每次建图都是新的 `Uuid::new_v4()`，
/// 把它放进比较就等于断言"随机数可复现"。反过来，**4 个新标量必须在**：
/// 它们正是 P2b 加进图的一等信息，一致性/确定性断言不覆盖它们就等于没验收。
/// 用 struct 而不是元组：13 个字段超过 std 为元组实现 `Debug`/`PartialEq`/`Ord`
/// 的最大元数（12），而这三条正是断言与排序所必需的。
///
/// **字段声明顺序即比较顺序**，与上面注释里写的投影顺序一一对应。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    caller_file: String,
    caller_name: String,
    caller_line_start: usize,
    callee_file: String,
    callee_name: String,
    callee_line_start: usize,
    line_number: usize,
    edge_callee_name: String,
    is_resolved: bool,
    resolution_level: u8,
    confidence: u8,
    rank: u8,
    candidate_count: u8,
}

/// [`EdgeKey`] 去掉 **callee 身份维**（`callee_file` + `callee_line_start`）后的结构投影。
///
/// 用途：全量/增量一致性 harness 里，"这条边在不在、连不连得对、4 个新标量填了什么"
/// 必须逐条锁死；而"同名歧义最终连到哪一份定义"在 legacy 下由 `HashMap` 的每进程随机
/// 种子决定（实测 16 轮独立建图里 `gamma.rs:6 call_helper` 会落到四个同名候选中的任一个，
/// 而 edges/nodes 恒为 13/32）。把那一维从比较里拿掉，其余 11 维照旧全锁 ——
/// 重复边/漏边依然会被边条数与结构投影逐条抓到，约束强度不降。
/// 口径出处：`tests/test_p1_edges_unchanged.rs` 守卫 4 的 (c)/(c2)，
/// 以及 `tests/fixtures/resolution/README.md` 的 legacy 抖动记录。
type StructuralEdgeKey = (
    String, // caller_file
    String, // caller_name
    usize,  // caller_line_start
    String, // callee_name（节点名；同名候选之间这一维相同）
    usize,  // line_number（调用行 = 调用点身份）
    String, // edge_callee_name（边上记录的名字）
    bool,   // is_resolved
    u8,     // resolution_level
    u8,     // confidence
    u8,     // rank
    u8,     // candidate_count
);

impl EdgeKey {
    fn without_callee_identity(&self) -> StructuralEdgeKey {
        (
            self.caller_file.clone(),
            self.caller_name.clone(),
            self.caller_line_start,
            self.callee_name.clone(),
            self.line_number,
            self.edge_callee_name.clone(),
            self.is_resolved,
            self.resolution_level,
            self.confidence,
            self.rank,
            self.candidate_count,
        )
    }
}

fn edge_multiset(graph: &PetCodeGraph, root: &Path) -> Vec<EdgeKey> {
    let rel = |p: &Path| -> String {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut out: Vec<EdgeKey> = graph
        .graph
        .edge_references()
        .map(|edge| {
            let rel_edge: &CallRelation = edge.weight();
            let caller = graph.get_function_by_id(&rel_edge.caller_id);
            let callee = graph.get_function_by_id(&rel_edge.callee_id);
            EdgeKey {
                caller_file: caller.as_ref().map(|f| rel(&f.file_path)).unwrap_or_default(),
                caller_name: caller.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                caller_line_start: caller.as_ref().map(|f| f.line_start).unwrap_or(0),
                callee_file: callee.as_ref().map(|f| rel(&f.file_path)).unwrap_or_default(),
                callee_name: callee.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                callee_line_start: callee.as_ref().map(|f| f.line_start).unwrap_or(0),
                line_number: rel_edge.line_number,
                edge_callee_name: rel_edge.callee_name.clone(),
                is_resolved: rel_edge.is_resolved,
                resolution_level: rel_edge.resolution_level,
                confidence: rel_edge.confidence,
                rank: rel_edge.rank,
                candidate_count: rel_edge.candidate_count,
            }
        })
        .collect();
    out.sort();
    out
}

/// [`edge_multiset`] 的**结构投影**版本（去掉 callee 身份维，理由见 [`StructuralEdgeKey`]）。
///
/// 凡是在 **legacy** 模式下做"两次独立建图之间比较"的断言，都必须用这个版本：
/// 同名歧义连到哪一份定义由 `HashMap` 的每进程随机种子决定，锁它 = 写一条永不可能稳定的断言。
/// layered 是确定性链路，比较时请继续用全维的 [`edge_multiset`]。
fn structural_edge_multiset(graph: &PetCodeGraph, root: &Path) -> Vec<StructuralEdgeKey> {
    let mut v: Vec<StructuralEdgeKey> = edge_multiset(graph, root)
        .iter()
        .map(|k| k.without_callee_identity())
        .collect();
    v.sort();
    v
}

/// 节点投影（同样不含 uuid）。
fn node_multiset(graph: &PetCodeGraph, root: &Path) -> Vec<(String, String, usize, usize, String, String, bool)> {
    let rel = |p: &Path| -> String {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut out: Vec<(String, String, usize, usize, String, String, bool)> = graph
        .get_all_functions()
        .into_iter()
        .map(|f| {
            (
                rel(&f.file_path),
                f.name.clone(),
                f.line_start,
                f.line_end,
                f.namespace.clone(),
                // 新字段也纳入投影：layered 填 module_path/qualified_name，legacy 恒 None，
                // 所以这条同时是"节点身份字段只在 layered 被写"的证据。
                f.qualified_name.clone().unwrap_or_default(),
                f.module_path.is_some(),
            )
        })
        .collect();
    out.sort();
    out
}

/// 有边的调用点数（口径 B 的度量单位：`(caller 文件, 调用行, 被调裸名)`）。
fn call_sites_with_edges(graph: &PetCodeGraph, root: &Path) -> usize {
    let rel = |p: &Path| -> String {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    graph
        .graph
        .edge_references()
        .map(|edge| {
            let rel_edge = edge.weight();
            let caller = graph.get_function_by_id(&rel_edge.caller_id);
            (
                caller.as_ref().map(|f| rel(&f.file_path)).unwrap_or_default(),
                rel_edge.line_number,
                rel_edge.callee_name.clone(),
            )
        })
        .collect::<HashSet<(String, usize, String)>>()
        .len()
}

// ─── 口径 D：修掉"只落哈希不落图 ⇒ 空图" ────────────────────────

/// **口径 D 的回归锁**：第一轮建图之后（哈希已落盘、图未落盘），
/// 第二轮**必须**得到与第一轮同样的图，而不是一张空图。
///
/// 改造前实测（P0 `851f7fb` / P2a `8e62033` 皆可复现）：
/// `nested_function_caller` 第一轮 5 节点 / 3 边，第二轮 **0 节点 / 0 边**。
#[test]
fn p2b_second_round_without_saved_graph_must_not_be_empty() {
    let first = build_once(
        &format!("{RESOLUTION_ROOT}/nested_function_caller"),
        "cacheD",
        1,
        false,
        // 第一轮也用 force_reindex=false：真实 CLI 第二次跑就是这个值。
        false,
        8,
    );
    let first_nodes = first.graph.get_all_functions().len();
    let first_edges = first.graph.graph.edge_count();
    println!(
        "口径 D 第一轮：nodes={} edges={}",
        first_nodes, first_edges
    );
    assert!(
        first_nodes > 0 && first_edges > 0,
        "第一轮就必须有内容（否则这条回归锁是空的）"
    );

    let second = rebuild_in_same_dir(&first, false, 8);
    let second_nodes = second.get_all_functions().len();
    let second_edges = second.graph.edge_count();
    println!(
        "口径 D 第二轮（读缓存）：nodes={} edges={}",
        second_nodes, second_edges
    );

    // 这就是改造前会红的那两条。
    assert_eq!(
        second_nodes, first_nodes,
        "第二轮函数节点数不得归零（口径 D 缺陷：只落哈希不落图）"
    );
    assert_eq!(
        second_edges, first_edges,
        "第二轮调用边数不得归零（口径 D 缺陷：只落哈希不落图）"
    );
    assert_eq!(
        structural_edge_multiset(&second, &first.project_dir),
        structural_edge_multiset(&first.graph, &first.project_dir),
        "第二轮的边多重集必须与第一轮逐条相等（legacy 口径：结构投影，callee 身份维除外）。\
         `nested_function_caller` 场景里每个被调名都只有唯一候选，故此处结构投影与全维投影等价。"
    );
}

/// 口径 D 的对照组：清掉全部缓存之后重跑，结果与"读缓存"的第二轮一致。
/// 证明修复只是"让哈希与图同生同死"，没有改变任何解析结果。
#[test]
fn p2b_cache_d_fix_keeps_clean_rebuild_identical() {
    let first = build_once(
        &format!("{RESOLUTION_ROOT}/method_receiver"),
        "cacheDctrl",
        2,
        false,
        false,
        8,
    );
    // 判据用**结构投影**：`method_receiver` 恰好是 legacy 的抖动场景（`add` 有两份同名候选
    // `calc.rs:9` / `misc.rs:4`，first-wins 选哪一份取决于 HashMap 的每进程随机种子 ——
    // 实测 16 轮独立建图 9:7 分布）。本测试要证的是"清缓存重建没有改变任何解析结果"，
    // 与"随机选中哪一份"无关；被放开的这一维在下面用 layered 全维对照补回强度。
    let expected_edges = structural_edge_multiset(&first.graph, &first.project_dir);
    let expected_nodes = node_multiset(&first.graph, &first.project_dir);

    // 抹掉全部磁盘缓存 ⇒ 下一轮必然全量重解析。
    clean_all_caches(&first.project_dir);
    let from_scratch = rebuild_in_same_dir(&first, false, 8);
    assert_eq!(
        node_multiset(&from_scratch, &first.project_dir),
        expected_nodes,
        "清缓存后从零重建的节点投影必须不变"
    );
    assert_eq!(
        structural_edge_multiset(&from_scratch, &first.project_dir),
        expected_edges,
        "清缓存后从零重建的边投影（结构投影，含 is_resolved 与 4 个新标量）必须逐条不变"
    );

    // 补回被放开那一维的强度：**layered 是确定性链路**，同一场景下清缓存重建必须全维相等
    // （含 callee 身份）。⇒ 证明"结构投影"只是为了容忍 legacy 的既有随机面，
    // 而不是掩盖任何真实的不一致。
    let l_first = build_once(
        &format!("{RESOLUTION_ROOT}/method_receiver"),
        "cacheDctrlL",
        9,
        true,
        false,
        8,
    );
    let l_expected = edge_multiset(&l_first.graph, &l_first.project_dir);
    clean_all_caches(&l_first.project_dir);
    let l_scratch = rebuild_in_same_dir(&l_first, true, 8);
    assert_eq!(
        edge_multiset(&l_scratch, &l_first.project_dir),
        l_expected,
        "[layered] 清缓存后从零重建必须**全维**（含 callee 身份）逐条相等"
    );
}

// ─── 交付物 4：全量 vs 增量一致性 harness ───────────────────────

/// 改一个文件走增量刷新，断言"图的多重集 == 从零全量重建"。
///
/// 口径：layered 模式（P2b 的新链路）。legacy 也顺带跑一遍，
/// 证明这条 harness 对两种模式都成立、P3 可以直接沿用。
#[test]
fn p2b_full_vs_incremental_refresh_is_consistent() {
    for layered in [false, true] {
        let tag = if layered { "layered" } else { "legacy" };
        let case = format!("{RESOLUTION_ROOT}/cross_file_same_name");
        let (_temp, dir) = staged(&case, "inc", 3);

        // (1) 从零全量建图。
        let mut parser = CodeParser::new();
        parser.set_force_reindex(true);
        if layered {
            parser.set_resolver_mode(ResolverMode::Layered);
        }
        let full = parser
            .build_petgraph_code_graph(&dir)
            .unwrap_or_else(|e| panic!("[{tag}] 全量建图失败: {e}"));
        let before = edge_multiset(&full, &dir);

        // (2) 改动一个文件，走 refresh_file 增量链路（同一 parser 实例 ⇒ 与真实 CLI 一致）。
        let gamma = dir.join("gamma.rs");
        let mut text = std::fs::read_to_string(&gamma).expect("读 gamma.rs");
        let before_lines = text.lines().count();
        text.push_str("\nfn extra_helper_marker() -> i32 { 1 }\n");
        std::fs::write(&gamma, text.clone()).expect("写 gamma.rs");
        assert!(
            text.lines().count() > before_lines,
            "增量用例必须真的改了文件"
        );

        let mut entity_graph = codeseek::codegraph::types::EntityGraph::default();
        // `full` 的克隆 = "上一轮已经落盘、这一轮从缓存读回来的图"，
        // 与真实增量场景的起点一致（`refresh_file` 内部会自己按文件清理旧节点/旧边）。
        let mut inc_graph = full.clone();
        parser
            .refresh_file(&gamma, &mut entity_graph, &mut inc_graph)
            .unwrap_or_else(|e| panic!("[{tag}] refresh_file 失败: {e}"));
        let incremental = edge_multiset(&inc_graph, &dir);

        // (3) 对照：**清空缓存后从零全量重建**同一份（已改过的）源码。
        clean_all_caches(&dir);
        let mut parser2 = CodeParser::new();
        parser2.set_force_reindex(true);
        if layered {
            parser2.set_resolver_mode(ResolverMode::Layered);
        }
        let rebuilt = parser2
            .build_petgraph_code_graph(&dir)
            .unwrap_or_else(|e| panic!("[{tag}] 重建失败: {e}"));
        let fresh = edge_multiset(&rebuilt, &dir);

        // 打印差异（只在失败时有用，但 --nocapture 下也是有价值的对照数据）。
        println!(
            "[{tag}] 改动前全量 edges={} / 增量刷新后 edges={} / 从零重建 edges={}",
            before.len(),
            incremental.len(),
            fresh.len()
        );

        // 增量链路的已知边界（P3 处理，本阶段只报告不掩盖）：
        // `refresh_file` 只重算**被改文件内部**的调用点，其它文件里指向本文件新函数的
        // 反向边不会重算；因此这里比较的是"被改文件自己产出的边"必须一致。
        let mine = |set: &[EdgeKey]| -> Vec<EdgeKey> {
            let gamma_rel = "gamma.rs".to_string();
            set.iter()
                .filter(|e| e.caller_file == gamma_rel)
                .cloned()
                .collect()
        };
        assert!(
            !mine(&fresh).is_empty(),
            "[{tag}] 被改文件本身必须有边，否则这条 harness 是空的"
        );

        // **结构投影**（去掉 callee 的身份维）—— 两种模式都必须逐条相等。
        // 这一条正是"重复边 / 漏边"的精确检测器：同一个调用点若被增量叠出两条，
        // 投影多重集就会多一个元素而失败；callee 选到哪一份同名候选**不参与比较**
        // （legacy 的 first-wins 取值来自 HashMap 每进程随机种子，实测 16 轮独立建图里
        //  `gamma.rs:6 call_helper` 会落到 alpha/beta/mod_y/other_mod 四个候选中的任一个，
        //  而 edges/nodes 恒为 13/32 —— 详见 `tests/test_p1_edges_unchanged.rs` 守卫 4 的
        //  (c)/(c2) 与 `tests/fixtures/resolution/README.md`）。
        let structural = |set: &[EdgeKey]| -> Vec<StructuralEdgeKey> {
            let mut v: Vec<StructuralEdgeKey> =
                set.iter().map(|k| k.without_callee_identity()).collect();
            v.sort();
            v
        };
        let (si, sf) = (structural(&mine(&incremental)), structural(&mine(&fresh)));
        assert_eq!(
            si.len(),
            sf.len(),
            "[{tag}] 增量刷新与被改文件从零重建的**边条数**必须相等（多出来=重复边，少了=漏边）"
        );
        assert_eq!(
            si, sf,
            "[{tag}] 增量刷新与被改文件从零重建的边（结构投影，含 is_resolved 与 4 个新标量）必须逐条相等"
        );

        // **全维投影**（含 callee 身份）：layered 是确定性链路，必须逐条相等；
        // legacy 只在"合法候选域"内允许不同 —— 用每调用点边数 + callee 名族把它的偏差钉住。
        if layered {
            assert_eq!(
                mine(&incremental),
                mine(&fresh),
                "[layered] 全维（含 callee 身份）也必须逐条相等：layered 是确定性链路"
            );
        } else {
            let per_site = |set: &[EdgeKey]| -> std::collections::BTreeMap<(String, usize), usize> {
                let mut m = std::collections::BTreeMap::new();
                for k in set {
                    *m.entry((k.caller_name.clone(), k.line_number)).or_insert(0usize) += 1;
                }
                m
            };
            println!(
                "[legacy] 每调用点边数 增量={:?} 重建={:?} | callee 身份 增量={:?} 重建={:?}",
                per_site(&mine(&incremental)),
                per_site(&mine(&fresh)),
                mine(&incremental)
                    .iter()
                    .map(|k| (k.callee_name.clone(), k.callee_file.clone()))
                    .collect::<Vec<_>>(),
                mine(&fresh)
                    .iter()
                    .map(|k| (k.callee_name.clone(), k.callee_file.clone()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                per_site(&mine(&incremental)),
                per_site(&mine(&fresh)),
                "[legacy] 每个调用点上的边条数必须一致（重复边只会出现在增量一侧）"
            );
            assert_eq!(
                mine(&incremental)
                    .iter()
                    .map(|k| k.callee_name.clone())
                    .collect::<Vec<_>>(),
                mine(&fresh)
                    .iter()
                    .map(|k| k.callee_name.clone())
                    .collect::<Vec<_>>(),
                "[legacy] callee 名（同族）必须一致 —— 允许抖的是'哪一份同名定义'，不是'哪个函数'"
            );
        }
    }
}

// ─── 交付物 4：确定性 ──────────────────────────────────────────

/// 同一份源码、同一配置，**同进程内两次**全量建图 ⇒ 边多重集（含 4 个新字段）逐条相等。
///
/// 这条同时消灭 P1 记录的种子抖动（`cross_file_edges` 在 legacy 下 simple_ts 7↔13、
/// simple_js 31↔35）：legacy 侧不去抖（P3 删掉它），layered 侧必须完全确定。
#[test]
fn p2b_layered_is_deterministic_within_process() {
    for case in [
        "cross_file_same_name",
        "ambiguous_trait_impl",
        "nested_function_caller",
        "method_receiver",
        "rust_qualified_path",
    ] {
        let rel = format!("{RESOLUTION_ROOT}/{case}");
        let a = build_once(&rel, "det", 4, true, true, 8);
        let b = build_once(&rel, "det", 5, true, true, 8);
        let ea = edge_multiset(&a.graph, &a.project_dir);
        let eb = edge_multiset(&b.graph, &b.project_dir);
        assert_eq!(ea, eb, "[{case}] layered 两次建图的边（含新字段）必须逐条相等");
        let na = node_multiset(&a.graph, &a.project_dir);
        let nb = node_multiset(&b.graph, &b.project_dir);
        assert_eq!(na, nb, "[{case}] layered 两次建图的节点投影必须相等");
        println!("[{case}] 确定性：{} 条边两次全等", ea.len());
    }
}

/// 跨进程等价：整仓 fixture（`simple_ts_project` / `simple_js_project`）在 layered 下
/// 两次建图的边多重集必须相等 —— 这正是 P1 遗留的 `cross_file_edges` 抖动被消除的证据。
///
/// ⚠ 只比较 `caller 文件 + 调用行 + 被调裸名 + 4 个新标量` 之外的**聚合量**时，
/// legacy 会抖；这里比较完整多重集，所以 legacy 侧**不做**这条断言（P3 删掉 legacy 后
/// 本测试即可整仓锁死）。
#[test]
fn p2b_layered_is_deterministic_over_full_repos() {
    for case in ["simple_rust_project", "simple_ts_project", "simple_python_project"] {
        let rel = format!("{REPOS_ROOT}/{case}");
        let a = build_once(&rel, "detRepo", 6, true, true, 8);
        let b = build_once(&rel, "detRepo", 7, true, true, 8);
        let ea = edge_multiset(&a.graph, &a.project_dir);
        let eb = edge_multiset(&b.graph, &b.project_dir);
        let ca = call_sites_with_edges(&a.graph, &a.project_dir);
        let cb = call_sites_with_edges(&b.graph, &b.project_dir);
        println!(
            "[{case}] layered edges={} sites={} / edges={} sites={}",
            ea.len(),
            ca,
            eb.len(),
            cb
        );
        assert_eq!(ea, eb, "[{case}] 整仓 layered 两次建图的边必须逐条相等（消除种子抖动）");
        assert_eq!(ca, cb, "[{case}] 有边的调用点数必须相等");
    }
}

/// legacy 侧的抖动**只做记录、不做断言**（断言它会 flaky），
/// 并把抖动条数打印出来，作为"P2b 之后 legacy 仍是旧的、必须被删掉"的证据。
#[test]
fn p2b_legacy_seed_jitter_is_recorded_not_locked() {
    let rel = format!("{REPOS_ROOT}/simple_ts_project");
    let a = build_once(&rel, "jit", 8, false, true, 8);
    let b = build_once(&rel, "jit", 9, false, true, 8);
    let cross_a = edge_multiset(&a.graph, &a.project_dir)
        .iter()
        .filter(|e| e.caller_file != e.callee_file)
        .count();
    let cross_b = edge_multiset(&b.graph, &b.project_dir)
        .iter()
        .filter(|e| e.caller_file != e.callee_file)
        .count();
    println!(
        "legacy 跨文件边两次实测：{cross_a} / {cross_b}（不等即 P1 记录的种子抖动；layered 侧由上面两条测试锁死）"
    );
    // layered 同 fixture 的跨文件边必须两次相等（把上面的确定性具体到最容易抖的那一维）。
    let la = build_once(&rel, "jitL", 10, true, true, 8);
    let lb = build_once(&rel, "jitL", 11, true, true, 8);
    let lx_a = edge_multiset(&la.graph, &la.project_dir)
        .iter()
        .filter(|e| e.caller_file != e.callee_file)
        .count();
    let lx_b = edge_multiset(&lb.graph, &lb.project_dir)
        .iter()
        .filter(|e| e.caller_file != e.callee_file)
        .count();
    assert_eq!(lx_a, lx_b, "layered 的跨文件边数不得随进程种子变化");
    println!("layered 跨文件边两次实测：{lx_a} / {lx_b}（必须相等）");
}

// ─── 交付物 4：覆盖中性（口径 B） ──────────────────────────────

/// 同一份 fixture 上，layered 的"有边的调用点数"必须 **等于** legacy。
///
/// 这是口径 B 的硬断言：分层解析器**绝不许**让一个调用点从"有一条边"变成"没有边"。
/// 未解析的调用点必须拿到与 legacy 同形态的幽灵边（`level=0 / confidence=0 / n=0`）。
#[test]
fn p2b_coverage_is_neutral_between_modes() {
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
        let rel = format!("{RESOLUTION_ROOT}/{case}");
        let legacy = build_once(&rel, "cov", 12, false, true, 8);
        let layered = build_once(&rel, "cov", 13, true, true, 8);
        let lc = call_sites_with_edges(&legacy.graph, &legacy.project_dir);
        let fc = call_sites_with_edges(&layered.graph, &layered.project_dir);
        let le = legacy.graph.graph.edge_count();
        let fe = layered.graph.graph.edge_count();
        println!("覆盖中性 CASE {case}: legacy sites={lc} edges={le} | layered sites={fc} edges={fe}");
        assert_eq!(lc, fc, "[{case}] 有边的调用点数必须两种模式相等（口径 B）");
        assert!(
            fe >= le,
            "[{case}] layered 的边数只许持平或变多（每候选一条边），绝不许净减少"
        );
    }
}

/// 口径 B 的三个具体调用点：`doubled`（闭包）、`isinstance`/`cache.has`（内建/属性调用）
/// 在 layered 下必须**仍有边**，且 `resolution_level == 0 && confidence == 0`。
#[test]
fn p2b_unresolved_sites_keep_ghost_edges_with_zero_metadata() {
    let built = build_once(
        &format!("{RESOLUTION_ROOT}/nested_function_caller"),
        "ghost",
        14,
        true,
        true,
        8,
    );
    let ghosts: Vec<&CallRelation> = built
        .graph
        .graph
        .edge_references()
        .map(|e| e.weight())
        .filter(|r| r.resolution_level == 0)
        .collect();
    assert_eq!(
        ghosts.len(),
        1,
        "nested_function_caller 在 layered 下应当只剩 `doubled` 一个未解析调用点，实测 = {:?}",
        ghosts.iter().map(|r| (&r.callee_name, r.line_number)).collect::<Vec<_>>()
    );
    let ghost = ghosts[0];
    assert_eq!(ghost.callee_name, "doubled");
    assert_eq!(ghost.resolution_level, 0, "口径 B：未解析必须 level=0");
    assert_eq!(ghost.confidence, 0, "口径 B：未解析必须 confidence=0");
    assert_eq!(ghost.candidate_count, 0, "口径 B：未解析必须 candidate_count=0");
    assert!(!ghost.is_resolved, "未解析边不得声称已解析");

    // 幽灵节点的形态必须与 legacy 逐字段同形（namespace=unresolved、行区间压成 (n,n)）。
    let node = built
        .graph
        .get_function_by_id(&ghost.callee_id)
        .expect("幽灵边必须指向图里的幽灵节点");
    assert_eq!(node.namespace, "unresolved");
    assert_eq!(node.line_start, node.line_end);
    assert_eq!(node.line_start, ghost.line_number);
    assert_eq!(node.signature.as_deref(), Some("unresolved_call_doubled"));
    assert!(node.module_path.is_none() && node.qualified_name.is_none(),
            "幽灵节点不是源码里的定义，layered 也不许给它编造身份");

    // 内建/属性调用（`isinstance` / `cache.has`）在 Python 整仓里的表现：
    // 必须仍有边，且这些边的 level 恒为 0。
    let py = build_once(&format!("{REPOS_ROOT}/simple_python_project"), "ghostPy", 15, true, true, 8);
    let zero: Vec<&CallRelation> = py
        .graph
        .graph
        .edge_references()
        .map(|e| e.weight())
        .filter(|r| r.resolution_level == 0)
        .collect();
    println!(
        "simple_python_project layered：level=0 的边 {} 条（其中 isinstance 命中 {} 条）",
        zero.len(),
        zero.iter().filter(|r| r.callee_name == "isinstance").count()
    );
    assert!(
        !zero.is_empty(),
        "Python 整仓里必然有未解析调用点，它们必须都拿到幽灵边而不是消失"
    );
    for rel in &zero {
        assert_eq!(rel.confidence, 0, "level=0 的边必须 confidence=0");
        assert_eq!(rel.candidate_count, 0, "level=0 的边必须 candidate_count=0");
        assert!(!rel.is_resolved, "level=0 的边不得声称已解析");
    }
}

// ─── 交付物 4：--max-candidates=1 的语义（防再次静默塌缩）──────

/// `--max-candidates=1` 只**截断落盘条数**，绝不改变 `is_resolved` 的判定口径，
/// 也必须保留 `candidate_count` 的真实值。
///
/// 这是防"再次静默塌缩"的关键：如果 max-candidates=1 把歧义写成
/// `is_resolved=true / candidate_count=1`，那分层解析器就又退回 first-wins 了，
/// 只是换了个地方。
#[test]
fn p2b_max_candidates_truncates_edges_but_not_truth() {
    let rel = format!("{RESOLUTION_ROOT}/ambiguous_trait_impl");
    let full = build_once(&rel, "mc8", 16, true, true, 8);
    let one = build_once(&rel, "mc1", 17, true, true, 1);

    // 8 个候选时：drive:7 的 runner.run() 展开成 5 条边，candidate_count=5。
    let drive_edges: Vec<&CallRelation> = full
        .graph
        .graph
        .edge_references()
        .map(|e| e.weight())
        .filter(|r| r.callee_name == "run" && r.candidate_count >= 2)
        .collect();
    assert_eq!(
        drive_edges.len(),
        5,
        "layered 默认上限 8：歧义调用点必须每个候选一条边，实测 {} 条",
        drive_edges.len()
    );
    for rel_edge in &drive_edges {
        assert_eq!(rel_edge.candidate_count, 5);
        assert!(!rel_edge.is_resolved, "候选≥2 一律 is_resolved=false");
    }

    // max=1：同一个调用点只落 1 条边，但 candidate_count 仍是 5、is_resolved 仍是 false。
    let one_edges: Vec<&CallRelation> = one
        .graph
        .graph
        .edge_references()
        .map(|e| e.weight())
        .filter(|r| r.callee_name == "run" && r.candidate_count >= 2)
        .collect();
    println!(
        "--max-candidates=1：candidate_count>=2 的边 {} 条（默认 8 时 {} 条）",
        one_edges.len(),
        drive_edges.len()
    );
    assert!(
        one_edges.len() < drive_edges.len(),
        "上限 1 必须真的少写边，否则这个开关是假的"
    );
    for rel_edge in &one_edges {
        assert_eq!(
            rel_edge.candidate_count, 5,
            "candidate_count 必须仍是**截断前**的真实候选数（口径 A）"
        );
        assert!(!rel_edge.is_resolved, "max-candidates 不得把歧义伪装成唯一");
        assert_eq!(rel_edge.rank, 0, "只留一条时留下的必须是 rank=0 的最优候选");
    }
    assert!(
        one_edges.iter().all(|r| r.rank == 0),
        "上限截断只能保留最优候选"
    );
}

// ─── 交付物 4：默认值守卫 ───────────────────────────────────────

/// **Phase P3 交付物 3（反向更新自 P2b 的 `p2b_default_resolver_is_still_legacy`）**：
/// 不带 `--resolver` 时默认档**必须走 layered**，配置层 / CLI 层 / 产边层三重证明；
/// 同时保留"显式 legacy 仍等于 P0 基线 32/13"的红线（默认翻转不得让 legacy 断言静默失效）。
///
/// before（P2b）：`assert_eq!(CodeParser::new().resolver_mode(), Legacy, "--resolver 默认必须是 legacy")`。
/// after （P3） ：`assert_eq!(CodeParser::new().resolver_mode(), Layered, "默认已切 layered")`。
#[test]
fn p3_default_resolver_is_layered() {
    use codeseek::cli::args::Cli;
    use clap::Parser;

    // (1) 配置层：新建 CodeParser 的默认 mode == Layered（反向更新的那条断言）。
    let parser = CodeParser::new();
    assert_eq!(
        parser.resolver_mode(),
        ResolverMode::Layered,
        "Phase P3：`CodeParser::new()` 默认必须是 layered"
    );

    // (2) CLI 层：不带 `--resolver` 解析出的取值必须是 Layered（钉 clap `default_value`）。
    let cli = Cli::try_parse_from(["codeseek", "init"]).expect("解析 `codeseek init`（不带 --resolver）");
    assert_eq!(
        cli.resolver,
        ResolverMode::Layered,
        "Phase P3：`--resolver` 的 clap 默认值必须是 layered"
    );
    // 显式 `--resolver=legacy` 仍可用（回滚逃生口没被删）。
    let cli_legacy =
        Cli::try_parse_from(["codeseek", "--resolver", "legacy", "init"]).expect("解析 --resolver=legacy");
    assert_eq!(cli_legacy.resolver, ResolverMode::Legacy, "`--resolver=legacy` 逃生口必须仍在");

    // (3) 产边层：走**默认档**（完全不 set_resolver_mode）建图，结果必须是 layered、
    //     绝不等于 legacy 的 32/13 —— 证明默认翻转真的作用到了产边管线。
    let (_temp, project_dir) = staged(RESOLUTION_ROOT, "deflt_layered", 19);
    let mut p = CodeParser::new();
    p.set_force_reindex(true);
    // 不调 set_resolver_mode ⇒ 吃全局默认（= Layered）。
    let g = p.build_petgraph_code_graph(&project_dir).expect("默认档建图");
    let edges: Vec<&CallRelation> = g.graph.edge_references().map(|e| e.weight()).collect();
    assert!(
        edges.iter().any(|r| r.candidate_count > 0 || r.resolution_level > 0),
        "默认档产边必须带分层标量（candidate_count/resolution_level 至少一条非 0），否则默认没切到 layered"
    );
    assert_ne!(
        (g.get_all_functions().len(), edges.len()),
        (32, 13),
        "默认档不该再是 legacy 的 32 节点 / 13 边"
    );

    // (4) 红线 2：显式 legacy 建图仍逐字节等于 P0 基线 32/13/11/2 + 新标量全 0。
    let built = build_once(RESOLUTION_ROOT, "defgt", 18, false, true, 8);
    let ledges: Vec<&CallRelation> = built.graph.graph.edge_references().map(|e| e.weight()).collect();
    assert_eq!(built.graph.get_all_functions().len(), 32);
    assert_eq!(ledges.len(), 13);
    assert_eq!(ledges.iter().filter(|r| r.is_resolved).count(), 11);
    assert_eq!(ledges.iter().filter(|r| !r.is_resolved).count(), 2);
    assert_eq!(
        built
            .graph
            .get_all_functions()
            .iter()
            .filter(|f| f.namespace == "unresolved")
            .count(),
        2
    );
    for rel_edge in &ledges {
        assert_eq!(rel_edge.resolution_level, 0);
        assert_eq!(rel_edge.confidence, 0);
        assert_eq!(rel_edge.rank, 0);
        assert_eq!(rel_edge.candidate_count, 0);
    }
    println!("P3 默认值守卫：默认=layered（配置/CLI/产边三重），显式 legacy 仍 32/13/11/2 + 标量全 0 ✓");
}

// ─── 任务口径 A：「跳过」与「喂索引」必须解耦 ────────────────────

/// 把图落盘成"图缓存"，用于构造**合法增量态**（图缓存 + 文件哈希同时存在）。
///
/// `build_petgraph_code_graph` 自己只写 `file_hashes.json`，图的落盘在另一条链路
/// （真实 CLI：`main.rs` 的 `persistence.save_graph(...)` + `codegraph::repository::save_state()`）。
/// `_load_existing_graph` 认的 project_id 是**目录名**，所以这里必须用目录名存。
fn persist_graph_as_cache(project_dir: &Path, graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) {
    let name_id = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .expect("临时项目目录必须有目录名");
    codeseek::storage::PersistenceManager::new()
        .save_graph(&name_id, graph, resolver_mode, max_candidates)
        .expect("图缓存落盘失败");
}

/// **口径 A 的钉死用例**：哈希命中 ⇒ 不重新解析、不重建 `file_scopes`；
/// 但分层索引仍必须仅凭图里已有的函数节点产出候选（边一条不少、一条不多）。
///
/// 这里同时钉死任务书留给实现二选一的那半句 ——
/// **`ensure_file_scope_ctx` 的按需补建不计入"重新解析"计数**（`CodeParser::last_round_file_counts`
/// 只统计 `build_*` 主循环里真正解析过的文件；补建只往 `file_scopes` 写一份 ctx，
/// 既不产 `FunctionInfo` 也不产 `CallRelation`）。选中这个口径的理由写在
/// `src/codegraph/parser.rs` 的 `last_round_parsed` 字段文档里。
#[test]
fn p2b_hash_skip_contract_feeds_index_without_reparsing() {
    let case = format!("{}/cross_file_same_name", RESOLUTION_ROOT);
    let (_temp, dir) = staged(&case, "skipA", 7);

    // R1：layered 全量（force）⇒ 三个文件都解析、都建 ctx。
    let mut p1 = CodeParser::new();
    p1.set_force_reindex(true);
    p1.set_resolver_mode(ResolverMode::Layered);
    p1.set_max_candidates(8);
    let g1 = p1
        .build_petgraph_code_graph(&dir)
        .unwrap_or_else(|e| panic!("R1 建图失败: {e}"));
    let c1 = p1.last_round_file_counts();
    assert_eq!(c1, (3, 0), "R1（force）必须解析全部 3 个文件、跳过 0 个，实际 {c1:?}");
    assert_eq!(p1.file_scope_count(), 3, "被真正解析过的文件必须顺手建好 ctx");
    let e1 = edge_multiset(&g1, &dir);
    assert!(!e1.is_empty(), "R1 必须产出边，否则后面的对照是空的");

    // 把磁盘状态摆成真实 CLI 跑完第一轮之后的样子：图 + 哈希都在。
    // R1 是 layered/8 产出的图 ⇒ 戳必须写 layered/8，R2 才可能命中档位门禁（P3 收口）。
    persist_graph_as_cache(&dir, &g1, "layered", 8);

    // R2：同一套配置、非 force ⇒ 哈希全命中。
    let mut p2 = CodeParser::new();
    p2.set_force_reindex(false);
    p2.set_resolver_mode(ResolverMode::Layered);
    p2.set_max_candidates(8);
    let g2 = p2
        .build_petgraph_code_graph(&dir)
        .unwrap_or_else(|e| panic!("R2 建图失败: {e}"));
    let c2 = p2.last_round_file_counts();
    assert_eq!(
        c2,
        (0, 3),
        "口径 A：哈希命中即跳过 —— 必须『重新解析 0 个 / 跳过 3 个』，实际 {c2:?}"
    );
    assert_eq!(
        p2.file_scope_count(),
        0,
        "被跳过的文件不得为『喂索引』而重建 file_scopes（口径 A 的解耦要求）"
    );
    // 索引供给：跳过一轮之后，layered 的边与全量**逐条相同**（layered 是确定性链路 ⇒ 用全维比较）。
    assert_eq!(
        edge_multiset(&g2, &dir),
        e1,
        "跳过轮不得丢边 / 叠边 / 改候选（读回的图 + 仅凭图节点的索引必须等价于全量）"
    );

    // 按需补建：拿得到 ctx，但**不算**"重新解析"。
    let one = dir.join("alpha.rs");
    let ctx = p2
        .ensure_file_scope_ctx(&one)
        .expect("按需补建必须能为被跳过的文件重建出 ctx");
    assert!(
        !ctx.scopes.is_empty(),
        "补建出来的 ctx 必须有作用域树（说明它是真解析，不是空壳）"
    );
    assert_eq!(
        p2.last_round_file_counts(),
        c2,
        "`ensure_file_scope_ctx` 的按需补建不许计入『重新解析』计数（本用例钉死的口径选择）"
    );

    println!(
        "口径 A 钉死：R1 {c1:?} → R2 {c2:?}（3/3 跳过）| 边 {} 条逐条相等 | 补建 1 份 ctx 后计数仍 {c2:?}",
        e1.len()
    );

    clean_all_caches(&dir);
}
