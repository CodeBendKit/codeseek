//! Phase P3 收口 B（交付物 2）：**终验测试集**。
//!
//! ## 本文件补的是哪四类"此前偏薄/缺失"的验收
//! 1. **全量 vs 增量等价 × 两种档位 × 4 个整仓**
//!    （[`p3b_full_vs_incremental_are_equivalent_over_four_repos_both_modes`]）：
//!    既有 `tests/test_p2b_wiring.rs::p2b_full_vs_incremental_refresh_is_consistent` 只有
//!    **一个 fixture**（`cross_file_same_name`），而且只比较**被改文件自己**的边；
//!    这里把它推广到 4 个整仓、并比较**整图**（含 `resolution_level` / `confidence` /
//!    `rank` / `candidate_count` 四个元信息字段）。
//! 2. **layered 确定性 × 4 个整仓**（[`p3b_layered_is_deterministic_over_all_four_repos`]）：
//!    既有的 `p2b_layered_is_deterministic_over_full_repos` 覆盖 rust/ts/python，**缺
//!    `simple_js_project`**（最大的那个仓）；这里补齐 4 仓，并同时核节点投影。
//! 3. **增量轮次绝不产出空图**（[`p3b_incremental_rounds_never_yield_empty_graph_over_four_repos`]）：
//!    既有的口径 D 回归锁 `p2b_second_round_without_saved_graph_must_not_be_empty` 只有
//!    一个 fixture、只有 legacy 一档；这里在 4 仓 × 两档上跑"第二轮（图+哈希都在）/
//!    第三轮（**只剩哈希、图被删**）"，并额外用 `last_round_file_counts()` 把
//!    "命中"与"重解析"两种状态区分开，防止"空图"被"部分命中"掩盖。
//! 4. **解析档位戳端到端**（[`p3b_resolver_stamp_gate_invalidates_and_rebuilds_end_to_end`]）：
//!    `tests/test_graph_cache_gate.rs` 只覆盖 **schema 版本**不符（v2 载荷）⇒
//!    `resolver_mode` / `max_candidates` 这两个**档位戳**在真实建图链路上的失效判定
//!    此前**零覆盖**。这里补上，并同时钉住"同档必须命中"的反面（否则"判失效"这条断言
//!    会因为"永远重建"而假绿）。
//!
//! 另加交付物 1 的守卫：[`p3b_levels_flag_fail_fast_requires_force_reindex`]。
//!
//! ## 口径（与既有约定一致，不新造规则）
//! * **legacy 档比较时丢掉 callee 的身份维**（`callee_file` + `callee_line_start`）：
//!   legacy 的同名 first-wins 取值来自 `HashMap` 每进程随机种子，锁它 = 写一条永不可能
//!   稳定的测试。出处：`tests/fixtures/resolution/README.md` 的「legacy 抖动实测」与
//!   `tests/test_p1_edges_unchanged.rs` 守卫 4 的 (c)/(c2)。
//!   被放开那一维由"其余 11 维（含 4 个元信息标量）逐条锁死 + 节点投影"补回强度。
//! * **Phase P3 收口 C：legacy 档另外再放开两样东西，理由同上，不是新造规则**
//!   1. `is_resolved`：**legacy 档它是 first-wins 命中结果的函数，不是独立语义维**。
//!      `CallRelation::legacy`（`src/codegraph/types.rs:108` 起）的 `is_resolved` 由调用方
//!      按旧语义直接传入（四个分层标量恒为 0），取值含义就是"这次有没有在索引里撞到同名
//!      节点"。同一个 `trim()` 调用点，本轮若撞到**别的文件**先造好的同名幽灵 ⇒ `true`；
//!      若按另一顺序先解析自己 ⇒ 在**本文件**新造幽灵 ⇒ `false`。这两种产出的 callee 身份
//!      本来就不同（正是被丢掉的那两维），所以只丢身份维、留着 `is_resolved`，等于把同一个
//!      随机量的另一个投影又锁回去。出处：`tests/fixtures/resolution/README.md`
//!      「legacy 抖动实测」—— 该节明确"甚至同进程内每轮建图"都会翻。
//!      ⚠ 只在 **legacy 投影**里放开；`layered` 仍走 `EdgeKey` **全 13 维**（含
//!      `is_resolved`），因为 `is_resolved = (resolution_level > 0 && candidate_count == 1)`
//!      在 layered 侧是按派生式重算的语义真值（`src/codegraph/types.rs:145` 起）。
//!   2. **节点投影里的幽灵节点**：幽灵的**存在与归属文件**由同一个 first-wins 随机选择
//!      决定 ⇒ 整文件重解析必然带来节点/边集合的条数差。出处：
//!      `tests/fixtures/resolution/README.md:106-107`「幽灵合并粒度」一节已明文定性：
//!      "只要任一条边/节点的落点发生变化，整文件重解析必然带来边与节点集合的差值 ——
//!      它不是可比较的性质"。因此 legacy 档的节点投影只锁**真定义**
//!      （`namespace != "unresolved"`），逐字段与 layered 一样严。
//! * **layered 档用全维度**（含 callee 身份）逐条比较。
//! * 判据一律**不依赖** `~/.codeseek` 的历史状态：每个用例把仓复制到唯一命名的临时目录，
//!   建图后按"目录名的 MD5"与"目录名"两个 project_id 清掉自己写进 home 的缓存
//!   （`CodeParser::_load_existing_graph` 认目录名、`_should_skip_file` 认 MD5），
//!   清理挂在 `Drop` 上 ⇒ panic 也会扫干净，用例之间互不依赖、与执行顺序无关。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use clap::Parser;

use codeseek::cli::args::{Cli, ResolverMode};
use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::types::{CallRelation, EntityGraph, FunctionInfo, PetCodeGraph};

const REPOS_ROOT: &str = "tests/test_repos";

/// 四个整仓 + 每个仓里**被增量改动**的那个文件（相对仓根）+ 追加代码片段。
///
/// 选文件的原则：该语言下的普通源文件（不选 `simple_js_project` 里那个
/// `main.9d1c33d4.js` —— 它可能被判为混淆/编译产物而走跳过分支），
/// 追加的函数名带 `p3b` 前缀 ⇒ **全仓唯一**，绝不新增同名候选，
/// 因此"改动其它文件指向本文件的边"这种增量链路边界（`refresh_file` 只重算被改文件
/// 内部的调用点）不会被触发，整图等价断言才有成立的正当理由。
const REPOS: &[(&str, &str, &str)] = &[
    (
        "simple_rust_project",
        "src/math.rs",
        "\nfn p3b_marker_rust_unique() -> i32 { 7 }\n",
    ),
    (
        "simple_python_project",
        "math_utils.py",
        "\ndef p3b_marker_python_unique():\n    return 7\n",
    ),
    (
        "simple_ts_project",
        "src/mathUtils.ts",
        "\nexport function p3bMarkerTsUnique(): number { return 7; }\n",
    ),
    (
        "simple_js_project",
        "mathUtils.js",
        "\nfunction p3bMarkerJsUnique() { return 7; }\n",
    ),
];

// ─── 沙箱（复制到临时目录 + 自动清理 home 里的缓存）──────────────

struct Sandbox {
    _temp: tempfile::TempDir,
    project_dir: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        clean_all_caches(&self.project_dir);
    }
}

/// 把 `REPOS` 里的某个仓复制进唯一命名的临时项目目录。
///
/// 目录名同时是 `_load_existing_graph` 认的 project_id，因此必须**每个用例唯一**
/// （label + 计数器 + 进程号三重），否则两个用例会互相看到对方的图缓存。
fn stage(repo: &str, label: &str, counter: usize) -> Sandbox {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("{REPOS_ROOT}/{repo}"));
    assert!(src.is_dir(), "整仓 fixture 不存在: {}", src.display());
    let temp = tempfile::TempDir::new().expect("创建临时目录失败");
    let project_dir = temp
        .path()
        .join(format!("p3b_{repo}_{label}_{counter}_{}", std::process::id()));
    copy_tree(&src, &project_dir);
    Sandbox {
        _temp: temp,
        project_dir,
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("创建目标目录");
    for entry in std::fs::read_dir(src).expect("读取目录") {
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

fn cleanup_codeseek_project(project_id: &str) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let dir = home.join(".codeseek").join("projects").join(project_id);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 清掉某个项目目录的两份缓存（按 MD5 命名的哈希目录 + 按目录名命名的图目录）。
fn clean_all_caches(project_dir: &Path) {
    let id_by_md5 = format!(
        "{:x}",
        md5::compute(project_dir.to_string_lossy().as_bytes())
    );
    cleanup_codeseek_project(&id_by_md5);
    if let Some(name) = project_dir.file_name() {
        cleanup_codeseek_project(&name.to_string_lossy());
    }
}

/// 只删**图缓存**（`graph.bin` / `graph.json`），保留文件 MD5 哈希。
///
/// 用来复现口径 D 的"有哈希、无图"状态 —— 修好之前这一轮会得到一张空图。
fn drop_graph_cache_only(project_dir: &Path) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let Some(name) = project_dir.file_name() else {
        return;
    };
    let dir = home.join(".codeseek").join("projects").join(name);
    for f in ["graph.bin", "graph.json"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// 把图落盘成"图缓存"，并**带上与本轮建图一致的解析档位戳**（P3 收口 A 的强制参数）。
///
/// `build_petgraph_code_graph` 自己只写 `file_hashes.json`，图的落盘在 CLI 那条链路
/// （`main.rs` 的 `persistence.save_graph(...)`）；测试要构造"合法增量态"必须手工补上，
/// 且戳必须与下一轮读取方的档位一致，否则会被 `validate_resolution_stamp` 判失效。
fn persist_graph_as_cache(project_dir: &Path, graph: &PetCodeGraph, mode: &str, max: u16) {
    let name_id = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .expect("临时项目目录必须有目录名");
    codeseek::storage::PersistenceManager::new()
        .save_graph(&name_id, graph, mode, max)
        .expect("图缓存落盘失败");
}

/// 档位字符串（与 `ResolverMode::as_str()` 同一真源，测试里不另写一套）。
fn mode_str(layered: bool) -> &'static str {
    if layered {
        ResolverMode::Layered.as_str()
    } else {
        ResolverMode::Legacy.as_str()
    }
}

fn new_parser(project_dir: &Path, layered: bool, force: bool) -> CodeParser {
    let mut parser = CodeParser::new();
    parser.set_force_reindex(force);
    // ⚠ 两档都必须**显式**钉：Phase P3 起默认已是 layered，测试不许依赖默认值
    //（`tests/test_p2b_wiring.rs::p3_default_resolver_is_layered` 另有一条钉默认本身）。
    parser.set_resolver_mode(if layered {
        ResolverMode::Layered
    } else {
        ResolverMode::Legacy
    });
    parser.set_max_candidates(8);
    let _ = project_dir;
    parser
}

fn full_build(project_dir: &Path, layered: bool) -> PetCodeGraph {
    let mut parser = new_parser(project_dir, layered, true);
    parser
        .build_petgraph_code_graph(project_dir)
        .unwrap_or_else(|e| panic!("[{}/{}] 全量建图失败: {e}", tag(layered), project_dir.display()))
}

fn tag(layered: bool) -> &'static str {
    if layered {
        "layered"
    } else {
        "legacy"
    }
}

/// 本仓**实际被解析**的文件数（R1 的 `last_round_file_counts().0`）与
/// `scan_directory` 的文件数可能不同（混淆 JS 会被跳过），所以命中/重解析的期望值
/// 一律取自 R1 的真实计数，而不是 `scan_directory` 的粗数。
fn source_file_count(project_dir: &Path) -> usize {
    let mut parser = CodeParser::new();
    parser.scan_directory(project_dir).len()
}

// ─── 边/节点投影（与 `tests/test_p2b_wiring.rs` 同一口径，本文件自带一份以免耦合）───

/// `(caller 相对文件, caller 名, caller 起始行, callee 相对文件, callee 名, callee 起始行,
///   调用行, 边上的 callee_name, is_resolved, resolution_level, confidence, rank, candidate_count)`
///
/// 刻意不含 `Uuid`（每次建图都是新的 v4）；**四个元信息标量必须在**，
/// 否则"增量与全量等价"就核不到 P2b 加进图的一等信息。
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
    /// callee 节点是不是**幽灵**（`namespace == "unresolved"`）。
    ///
    /// 不参与 `layered` 的全维度比较（那里连 callee 身份一起锁），只给 legacy 投影做
    /// 过滤用：命中"哪个同名幽灵/要不要新造一个幽灵"与 `callee_file`/`callee_line_start`
    /// 是同一个 first-wins 随机量的投影，依据见文件头「口径」第 2 条。
    /// 因此它**不进** `StructuralEdgeKey`，只用于"这条边是否属于不可比较的那一批"。
    callee_is_ghost: bool,
}

/// 去掉 **callee 身份维**与 **`is_resolved`** 之后的结构投影（legacy 专用，理由见文件头）。
///
/// Phase P3 收口 C 从 11 维减到 10 维：`is_resolved` 在 legacy 档就是"这次 first-wins
/// 撞没撞到同名节点"，与被丢掉的 `callee_file`/`callee_line_start` 是**同一个随机量的两个
/// 投影**（依据见文件头「口径」第 2 条与 `tests/fixtures/resolution/README.md`）。
/// 其余 10 维照旧逐条锁死：caller 的身份 3 维 + callee 裸名 + 调用行 + 边上的 callee_name
/// + `resolution_level`/`confidence`/`rank`/`candidate_count` 4 个元信息标量。
type StructuralEdgeKey = (
    String, // caller_file
    String, // caller_name
    usize,  // caller_line_start
    String, // callee_name
    usize,  // line_number
    String, // edge_callee_name
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
            self.resolution_level,
            self.confidence,
            self.rank,
            self.candidate_count,
        )
    }
}

fn rel_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn edge_multiset(graph: &PetCodeGraph, root: &Path) -> Vec<EdgeKey> {
    let mut out: Vec<EdgeKey> = graph
        .graph
        .edge_references()
        .map(|edge| {
            let rel_edge: &CallRelation = edge.weight();
            let caller = graph.get_function_by_id(&rel_edge.caller_id);
            let callee = graph.get_function_by_id(&rel_edge.callee_id);
            EdgeKey {
                caller_file: caller.as_ref().map(|f| rel_of(root, &f.file_path)).unwrap_or_default(),
                caller_name: caller.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                caller_line_start: caller.as_ref().map(|f| f.line_start).unwrap_or(0),
                callee_file: callee.as_ref().map(|f| rel_of(root, &f.file_path)).unwrap_or_default(),
                callee_name: callee.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                callee_line_start: callee.as_ref().map(|f| f.line_start).unwrap_or(0),
                line_number: rel_edge.line_number,
                edge_callee_name: rel_edge.callee_name.clone(),
                is_resolved: rel_edge.is_resolved,
                resolution_level: rel_edge.resolution_level,
                confidence: rel_edge.confidence,
                rank: rel_edge.rank,
                candidate_count: rel_edge.candidate_count,
                callee_is_ghost: callee
                    .as_ref()
                    .map(|f| f.namespace == "unresolved")
                    .unwrap_or(false),
            }
        })
        .collect();
    out.sort();
    out
}

fn structural_multiset(edges: &[EdgeKey]) -> Vec<StructuralEdgeKey> {
    // legacy 档**只比较"打到真定义"的边**：callee 是幽灵的边，其存在性取决于
    // "本轮有没有先撞到同名幽灵 / 要不要新造一个"，与被丢掉的 callee 身份维
    // （以及 legacy 的 `is_resolved`）是同一个 first-wins 随机量的投影。
    // 依据：`tests/fixtures/resolution/README.md`「legacy 抖动实测」与「幽灵合并粒度」两节
    // —— 后者已明文"整文件重解析必然带来边与节点集合的差值，它不是可比较的性质"。
    // 补回强度的做法：打到**真定义**的边仍然 10 维逐条锁死；ghost 边的 caller 侧 4 个
    // 元信息标量在 legacy 恒为 0，本来也不携带信息（`legacy_ghost` 构造器写死）。
    let mut v: Vec<StructuralEdgeKey> = edges
        .iter()
        .filter(|k| !k.callee_is_ghost)
        .map(|k| k.without_callee_identity())
        .collect();
    v.sort();
    v
}

/// 节点投影（不含 uuid；含 P2b 新增的身份字段是否被填）。
fn node_multiset(
    graph: &PetCodeGraph,
    root: &Path,
) -> Vec<(String, String, usize, usize, String, String, bool)> {
    node_multiset_inner(graph, root, true)
}

/// legacy 档用的节点投影：**只锁真定义**（`namespace != "unresolved"`）。
///
/// 依据见文件头「口径」第 2 条第 2 点：legacy 的幽灵节点在哪个文件、有几条，
/// 与 callee 身份维是同一个 first-wins 随机量的投影；
/// `tests/fixtures/resolution/README.md`「幽灵合并粒度」一节已把"整文件重解析带来的
/// 节点/边集合差值"定性为**不是可比较的性质**。真定义的 7 个字段照旧逐条锁死。
fn real_node_multiset(
    graph: &PetCodeGraph,
    root: &Path,
) -> Vec<(String, String, usize, usize, String, String, bool)> {
    node_multiset_inner(graph, root, false)
}

fn node_multiset_inner(
    graph: &PetCodeGraph,
    root: &Path,
    include_ghosts: bool,
) -> Vec<(String, String, usize, usize, String, String, bool)> {
    let mut out: Vec<(String, String, usize, usize, String, String, bool)> = graph
        .get_all_functions()
        .into_iter()
        .filter(|f: &&FunctionInfo| include_ghosts || f.namespace != "unresolved")
        .map(|f: &FunctionInfo| {
            (
                rel_of(root, &f.file_path),
                f.name.clone(),
                f.line_start,
                f.line_end,
                f.namespace.clone(),
                f.qualified_name.clone().unwrap_or_default(),
                f.module_path.is_some(),
            )
        })
        .collect();
    out.sort();
    out
}

/// 多重集差异摘要：只打印"仅左 / 仅右"的条目（各最多 6 条）与总条数。
///
/// 断言失败信息必须**一眼看出是哪个仓 / 哪个档位 / 哪个维度**不一致，
/// 因此比较函数一律把 `repo`/`mode`/`维度` 写进 `context`。
fn diff_report<T: Ord + Clone + std::fmt::Debug>(left: &[T], right: &[T], context: &str) -> String {
    fn tally<'a, T: Ord>(v: &'a [T]) -> BTreeMap<&'a T, usize> {
        let mut m: BTreeMap<&'a T, usize> = BTreeMap::new();
        for item in v {
            *m.entry(item).or_insert(0) += 1;
        }
        m
    }
    fn only_left<'a, T: Ord + std::fmt::Debug>(
        a: &BTreeMap<&'a T, usize>,
        b: &BTreeMap<&'a T, usize>,
    ) -> Vec<String> {
        a.iter()
            .filter(|(k, _)| !b.contains_key(*k))
            .take(6)
            .map(|(k, n)| format!("×{n} {k:?}"))
            .collect()
    }
    let (ml, mr) = (tally(left), tally(right));
    format!(
        "[{context}] 左 {} 条 / 右 {} 条\n  仅左: {:?}\n  仅右: {:?}",
        left.len(),
        right.len(),
        only_left(&ml, &mr),
        only_left(&mr, &ml)
    )
}

// ─── 交付物 2①：全量 vs 增量等价 × 两档 × 4 仓 ─────────────────

#[test]
fn p3b_full_vs_incremental_are_equivalent_over_four_repos_both_modes() {
    // 8 个组合（4 仓 × 2 档）全部跑完再一次性断言：这样一条失败信息就能看清
    // "到底是哪几个仓/哪一档在哪个维度上不等价"，而不是第一个红灯遮住后面的事实。
    let mut failures: Vec<String> = Vec::new();
    for (repo, rel_file, snippet) in REPOS {
        for layered in [true, false] {
            let mode = tag(layered);
            let sb = stage(repo, "incEQ", 1);
            let dir = sb.project_dir.clone();

            // (1) 改动前：全量建图（同一个 parser 实例继续做刷新，与真实 CLI 一致）。
            let mut parser = new_parser(&dir, layered, true);
            let before = parser
                .build_petgraph_code_graph(&dir)
                .unwrap_or_else(|e| panic!("[{repo}/{mode}] 改动前全量建图失败: {e}"));
            let before_edges = edge_multiset(&before, &dir);
            assert!(
                !before_edges.is_empty(),
                "[{repo}/{mode}] 改动前就无边 ⇒ 本等价断言是空的"
            );

            // (2) 改一个文件（追加一个全仓唯一的函数）⇒ 走 `refresh_file` 增量链路。
            let target = dir.join(rel_file);
            assert!(target.is_file(), "[{repo}/{mode}] 要改的文件不存在: {}", target.display());
            let text = std::fs::read_to_string(&target).expect("读被改文件");
            std::fs::write(&target, format!("{text}{snippet}")).expect("写被改文件");

            let mut entity_graph = EntityGraph::default();
            let mut refreshed = before.clone();
            parser
                .refresh_file(&target, &mut entity_graph, &mut refreshed)
                .unwrap_or_else(|e| panic!("[{repo}/{mode}] refresh_file 失败: {e}"));

            // (3) 对照：**同一份（已改过的）源码从零全量重建**。
            let rebuilt = full_build(&dir, layered);

            let inc_edges = edge_multiset(&refreshed, &dir);
            let fresh_edges = edge_multiset(&rebuilt, &dir);

            // 改动必须真的生效（新函数进了图），否则"等价"可能只是因为什么都没变。
            let has_marker = |names: &BTreeSet<String>| names.contains("p3b_marker_rust_unique")
                || names.contains("p3b_marker_python_unique")
                || names.contains("p3bMarkerTsUnique")
                || names.contains("p3bMarkerJsUnique");
            let fresh_names: BTreeSet<String> = rebuilt
                .get_all_functions()
                .into_iter()
                .map(|f| f.name.clone())
                .collect();
            let inc_names: BTreeSet<String> = refreshed
                .get_all_functions()
                .into_iter()
                .map(|f| f.name.clone())
                .collect();
            assert!(
                has_marker(&fresh_names),
                "[{repo}/{mode}] 从零重建没看到新加的 marker 函数 ⇒ 改动未被解析：{fresh_names:?}"
            );
            assert!(
                has_marker(&inc_names),
                "[{repo}/{mode}] 增量刷新没看到新加的 marker 函数 ⇒ refresh_file 漏了节点：{inc_names:?}"
            );

            println!(
                "[{repo}/{mode}] 全量(改动前)={} 边 | 增量刷新={} 边 | 从零重建={} 边",
                before_edges.len(),
                inc_edges.len(),
                fresh_edges.len()
            );

            if layered {
                // layered 是确定性链路 ⇒ **全维度**（含 callee 身份）逐条相等。
                if inc_edges != fresh_edges {
                    failures.push(diff_report(
                        &inc_edges,
                        &fresh_edges,
                        &format!("{repo}/layered 整图边·全维度"),
                    ));
                }
                let (na, nb) = (node_multiset(&refreshed, &dir), node_multiset(&rebuilt, &dir));
                if na != nb {
                    failures.push(format!(
                        "{}",
                        diff_report(&na, &nb, &format!("{repo}/layered 整图节点·投影"))
                    ));
                }
            } else {
                // legacy：丢掉 callee 身份维（`callee_file` + `callee_line_start`）、
                // `is_resolved` 以及"打到幽灵"的那批边（三条依据都在文件头「口径」第 2 条），
                // 其余 10 维（含 4 个元信息标量）照旧逐条锁死 + 真定义节点投影全锁。
                let si = structural_multiset(&inc_edges);
                let sf = structural_multiset(&fresh_edges);
                let ghost_i = inc_edges.iter().filter(|k| k.callee_is_ghost).count();
                let ghost_f = fresh_edges.iter().filter(|k| k.callee_is_ghost).count();
                println!(
                    "[{repo}/legacy] 真定义边 {} vs {}（另有 {} vs {} 条打到幽灵的边不参与比较）",
                    si.len(),
                    sf.len(),
                    ghost_i,
                    ghost_f
                );
                // 防止"过滤把断言过滤空了"：真定义边必须非空，否则本比较是假的。
                assert!(
                    !si.is_empty() && !sf.is_empty(),
                    "[{repo}/legacy] 真定义边投影为空 ⇒ 本用例的比较是空的（{si:?} vs {sf:?}）"
                );
                if si != sf {
                    failures.push(format!(
                        "{}",
                        diff_report(
                            &si,
                            &sf,
                            &format!(
                                "{repo}/legacy 整图边·结构投影（callee 身份维除外，{} vs {} 条）",
                                si.len(),
                                sf.len()
                            )
                        )
                    ));
                }
                let (na, nb) = (real_node_multiset(&refreshed, &dir), real_node_multiset(&rebuilt, &dir));
                if na != nb {
                    failures.push(format!(
                        "{}",
                        diff_report(&na, &nb, &format!("{repo}/legacy 整图节点·投影（真定义）"))
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "全量 vs 增量不等价的组合共 {} 个（增量=左 / 从零重建=右）：\n{}",
        failures.len(),
        failures.join("\n────────\n")
    );
}

// ─── 交付物 2②：layered 确定性 × 4 仓（补齐既有测试缺的 js 仓）───

#[test]
fn p3b_layered_is_deterministic_over_all_four_repos() {
    for (repo, _, _) in REPOS {
        // 两次**互相独立**的全量建图（不同沙箱目录 ⇒ 不共享任何内存状态，也不读磁盘缓存）。
        let a = stage(repo, "det", 2);
        let b = stage(repo, "det", 3);
        let ga = full_build(&a.project_dir, true);
        let gb = full_build(&b.project_dir, true);

        let ea = edge_multiset(&ga, &a.project_dir);
        let eb = edge_multiset(&gb, &b.project_dir);
        println!("[{repo}/layered] 两次全量建图边数 = {} / {}", ea.len(), eb.len());
        assert_eq!(
            ea,
            eb,
            "{}",
            diff_report(&ea, &eb, &format!("{repo}/layered 确定性·边全维度"))
        );

        let na = node_multiset(&ga, &a.project_dir);
        let nb = node_multiset(&gb, &b.project_dir);
        assert_eq!(
            na,
            nb,
            "[{repo}/layered] 两次全量建图的节点投影必须逐条相等（{} vs {}）",
            na.len(),
            nb.len()
        );

        // 确定性还要覆盖"层级分布"这个精度维度：两次的 resolution_level 直方图必须一致。
        let hist = |g: &PetCodeGraph, root: &Path| -> BTreeMap<u8, usize> {
            let mut m: BTreeMap<u8, usize> = BTreeMap::new();
            for k in edge_multiset(g, root) {
                *m.entry(k.resolution_level).or_insert(0) += 1;
            }
            m
        };
        assert_eq!(
            hist(&ga, &a.project_dir),
            hist(&gb, &b.project_dir),
            "[{repo}/layered] resolution_level 直方图必须一致，实际 {:?} / {:?}",
            hist(&ga, &a.project_dir),
            hist(&gb, &b.project_dir)
        );
    }
}

// ─── 交付物 2③：增量轮次绝不产出空图（口径 D，4 仓 × 两档）─────

#[test]
fn p3b_layered_reparse_is_deterministic_over_all_four_repos() {
    // 为什么要单独一条守卫：`p3b_layered_is_deterministic_over_all_four_repos` 只比较
    // **两次 force 全量建图**（同一条代码路径），而实测抖动发生在"有文件哈希、无图 ⇒
    // 走 `_should_skip_file` 全量重解析"这条**非 force** 路径上（`p3b_incremental_rounds…`
    // 的 R3 轮，约 14% 翻红：`simple_js_project/layered` 的 1126 条边多重集里
    // `main.9d1c33d4.js` 若干条**幽灵边**的 `caller_name` 在 `onClick` 与 `Va` 之间翻转）。
    // 该翻转依赖"本线程此前创建了多少个 HashMap"（`RandomState` 种子轨迹），所以守卫必须
    // **在同一线程内重复多轮 reparse**、并跨轮交叉比较，才能在修复前稳定抓红。
    //
    // 断言口径：对每个仓做 N 次相互独立的"全量(force) + 只留哈希删图后的重解析(非force)"，
    // 把这 2N 张图的全维度边多重集**全部**要求相等（force 之间、重解析之间、force vs 重解析）。
    // 修复后全链路确定性 ⇒ 恒绿；修复前 ⇒ 只要任一对不等即红（实测能抓到）。
    const REPS: usize = 4;
    for (repo, _, _) in REPOS {
        let mut builds: Vec<Vec<EdgeKey>> = Vec::new();
        for rep in 0..REPS {
            let sb = stage(repo, "reparseDet", 300 + rep);
            let dir = sb.project_dir.clone();

            // (a) force 全量建图 + 落盘图缓存 + 写哈希
            let g_force = full_build(&dir, true);
            persist_graph_as_cache(&dir, &g_force, "layered", 8);
            builds.push(edge_multiset(&g_force, &dir));

            // (b) 只留文件哈希、删掉图缓存 ⇒ 非 force 的全量重解析（正是 R3 的那条路径）
            drop_graph_cache_only(&dir);
            let mut p = new_parser(&dir, true, false);
            let g_reparse = p
                .build_petgraph_code_graph(&dir)
                .unwrap_or_else(|e| panic!("[{repo}] 重解析建图失败: {e}"));
            let counts = p.last_round_file_counts();
            assert!(
                counts.0 > 0 && counts.1 == 0,
                "[{repo}] 重解析轮必须真的全量重扫（实得 {counts:?}）—— 否则本守卫走错了分支"
            );
            builds.push(edge_multiset(&g_reparse, &dir));
        }
        let base = &builds[0];
        for (i, got) in builds.iter().enumerate().skip(1) {
            assert_eq!(
                base.len(),
                got.len(),
                "[{repo}] 第 {i} 次建图边数 {} != 基准 {}",
                got.len(),
                base.len()
            );
            assert_eq!(
                base,
                got,
                "{}",
                diff_report(base, got, &format!("{repo}/layered reparse 确定性·边全维度"))
            );
        }
    }
}

#[test]
fn p3b_incremental_rounds_never_yield_empty_graph_over_four_repos() {
    for (repo, _, _) in REPOS {
        for layered in [true, false] {
            let mode = tag(layered);
            let sb = stage(repo, "notEmpty", 4);
            let dir = sb.project_dir.clone();
            let scanned = source_file_count(&dir);

            // ---- 第一轮：全量（force），并把图落盘（构造真实 CLI 的合法增量态）----
            let mut p1 = new_parser(&dir, layered, true);
            let g1 = p1
                .build_petgraph_code_graph(&dir)
                .unwrap_or_else(|e| panic!("[{repo}/{mode}] 第一轮全量失败: {e}"));
            let r1 = p1.last_round_file_counts();
            assert!(
                r1.0 > 0 && r1.0 <= scanned,
                "[{repo}/{mode}] 第一轮必须真的解析文件（实得 {r1:?}，扫描到 {scanned} 个）"
            );
            // 期望值取自 R1 的真实解析数（混淆 JS 之类被跳过的文件不在此列）。
            let n_files = r1.0;
            persist_graph_as_cache(&dir, &g1, mode_str(layered), 8);
            let e1 = edge_multiset(&g1, &dir);
            let n1 = node_multiset(&g1, &dir);
            println!(
                "[{repo}/{mode}] R1 全量：{} 文件 / {} 节点 / {} 边",
                n_files,
                n1.len(),
                e1.len()
            );

            // ---- 第二轮：非 force，图缓存 + 哈希缓存**都在** ⇒ 必须命中且非空 ----
            let mut p2 = new_parser(&dir, layered, false);
            let g2 = p2
                .build_petgraph_code_graph(&dir)
                .unwrap_or_else(|e| panic!("[{repo}/{mode}] 第二轮失败: {e}"));
            let c2 = p2.last_round_file_counts();
            assert!(
                g2.get_all_functions().len() > 0 && g2.graph.edge_count() > 0,
                "[{repo}/{mode}] 第二轮（图+哈希都在）产出空图：{:?}",
                (g2.get_all_functions().len(), g2.graph.edge_count())
            );
            assert_eq!(
                c2,
                (0, n_files),
                "[{repo}/{mode}] 第二轮必须命中缓存并跳过全部 {n_files} 个文件（实得 {c2:?}）"
            );
            compare_round(&dir, &g2, &g1, layered, repo, mode, "R2");

            // ---- 第三轮：只留哈希、删掉图缓存 ⇒ 口径 D 的"有哈希无图"状态 ----
            drop_graph_cache_only(&dir);
            let mut p3 = new_parser(&dir, layered, false);
            let g3 = p3
                .build_petgraph_code_graph(&dir)
                .unwrap_or_else(|e| panic!("[{repo}/{mode}] 第三轮失败: {e}"));
            let c3 = p3.last_round_file_counts();
            assert!(
                g3.get_all_functions().len() > 0 && g3.graph.edge_count() > 0,
                "[{repo}/{mode}] 第三轮（只剩哈希）产出空图 ⇒ 口径 D 回归：nodes={} edges={}",
                g3.get_all_functions().len(),
                g3.graph.edge_count()
            );
            assert_eq!(
                c3,
                (n_files, 0),
                "[{repo}/{mode}] 图缓存缺失 ⇒ 哈希必须一律作废、全量重解析 {n_files} 个文件（实得 {c3:?}）"
            );
            compare_round(&dir, &g3, &g1, layered, repo, mode, "R3");
        }
    }
}

/// 第二轮/第三轮与第一轮全量的对照（档位不同判据不同，口径见文件头）。
fn compare_round(
    dir: &Path,
    got: &PetCodeGraph,
    full: &PetCodeGraph,
    layered: bool,
    repo: &str,
    mode: &str,
    round: &str,
) {
    let (got_edges, full_edges) = (edge_multiset(got, dir), edge_multiset(full, dir));
    // 节点投影按档位选口径：layered 含幽灵、全 7 维逐条锁；legacy 只锁真定义
    // （依据见文件头「口径」第 2 条第 2 点与 `tests/fixtures/resolution/README.md`
    // 「幽灵合并粒度」一节 —— 幽灵的条数与归属在 legacy 档不是可比较的性质）。
    let (got_nodes, full_nodes) = if layered {
        (node_multiset(got, dir), node_multiset(full, dir))
    } else {
        (real_node_multiset(got, dir), real_node_multiset(full, dir))
    };
    assert_eq!(
        got_nodes.len(),
        full_nodes.len(),
        "[{repo}/{mode}] {round} 节点数 {got} != 全量 {want}",
        got = got_nodes.len(),
        want = full_nodes.len()
    );
    if layered {
        assert_eq!(
            got_edges,
            full_edges,
            "{}",
            diff_report(&got_edges, &full_edges, &format!("{repo}/{mode} {round}·边全维度"))
        );
        assert_eq!(
            got_nodes,
            full_nodes,
            "{}",
            diff_report(&got_nodes, &full_nodes, &format!("{repo}/{mode} {round}·节点投影"))
        );
    } else {
        let (sg, sf) = (structural_multiset(&got_edges), structural_multiset(&full_edges));
        assert_eq!(
            sg,
            sf,
            "{}",
            diff_report(
                &sg,
                &sf,
                &format!("{repo}/{mode} {round}·边结构投影（callee 身份维除外）")
            )
        );
    }
}

// ─── 交付物 2④：解析档位戳的端到端失效判定 ────────────────────

#[test]
fn p3b_resolver_stamp_gate_invalidates_and_rebuilds_end_to_end() {
    // 取一个"两档产出确实不同"的小仓（rust：legacy 无元信息、layered 带层级）。
    let repo = "simple_rust_project";
    let sb = stage(repo, "stamp", 5);
    let dir = sb.project_dir.clone();

    // ---- R1：用 **legacy** 档建图并按 legacy 戳落盘 ----
    let mut p1 = new_parser(&dir, false, true);
    let legacy_graph = p1
        .build_petgraph_code_graph(&dir)
        .unwrap_or_else(|e| panic!("[{repo}/legacy] 建图失败: {e}"));
    // 期望的"重解析/跳过"条数以 R1 实际解析的文件数为准。
    let n_files = p1.last_round_file_counts().0;
    assert!(n_files > 0, "[{repo}] R1 一个文件都没解析，本用例是空的");
    persist_graph_as_cache(&dir, &legacy_graph, "legacy", 8);
    let legacy_edges = edge_multiset(&legacy_graph, &dir);
    let legacy_with_level = legacy_edges.iter().filter(|e| e.resolution_level > 0).count();
    println!(
        "[{repo}] legacy 档：{} 边，其中 resolution_level>0 的 {} 条（legacy 恒 0）",
        legacy_edges.len(),
        legacy_with_level
    );
    assert_eq!(
        legacy_with_level, 0,
        "[{repo}] legacy 写出了带层级的边 ⇒ 红线（legacy 不消费分层引擎）已被破坏"
    );

    // ---- R2：**默认档位（layered）**、非 force 加载 ⇒ 必须判档位失效并全量重建 ----
    // 读取方用 `CodeParser::new()` 的默认配置（P3 起 = layered / max=8），
    // 这正是"`--resolver` 不置 flag"时真实 CLI 走的那条路。
    let mut p2 = CodeParser::new();
    p2.set_force_reindex(false);
    assert_eq!(p2.resolver_mode(), ResolverMode::Layered);
    let rebuilt = p2
        .build_petgraph_code_graph(&dir)
        .unwrap_or_else(|e| panic!("[{repo}/默认(layered)] 加载失败: {e}"));
    let c2 = p2.last_round_file_counts();
    assert_eq!(
        c2,
        (n_files, 0),
        "[{repo}] 档位戳不符（缓存=legacy，当前=layered）⇒ 必须作废缓存并全量重解析 {n_files} 个文件，实得 {c2:?}"
    );
    let rebuilt_edges = edge_multiset(&rebuilt, &dir);
    let rebuilt_with_level = rebuilt_edges.iter().filter(|e| e.resolution_level > 0).count();
    assert!(
        rebuilt_with_level > 0,
        "[{repo}] 重建之后应当带上 layered 的解析层级标注（{} 条带层级），实得 0 条 ⇒ 缓存被复用了",
        rebuilt_edges.len()
    );
    // 重建结果必须等于"从零 layered 全量建图"（而不是 legacy 图的任何一部分）。
    let fresh_layered = full_build(&dir, true);
    assert_eq!(
        rebuilt_edges,
        edge_multiset(&fresh_layered, &dir),
        "{}",
        diff_report(
            &rebuilt_edges,
            &edge_multiset(&fresh_layered, &dir),
            &format!("{repo} 档位失效重建 vs 从零 layered 全量")
        )
    );
    assert!(
        !rebuilt_edges.is_empty(),
        "[{repo}] 档位失效之后的图不许是空的"
    );

    // ---- 对照（防"永远重建"假绿）：**同档同上限**必须命中缓存 ----
    persist_graph_as_cache(&dir, &rebuilt, "layered", 8);
    let mut p3 = CodeParser::new();
    p3.set_force_reindex(false);
    let hit = p3
        .build_petgraph_code_graph(&dir)
        .unwrap_or_else(|e| panic!("[{repo}/layered] 命中轮失败: {e}"));
    let c3 = p3.last_round_file_counts();
    assert_eq!(
        c3,
        (0, n_files),
        "[{repo}] 同档（layered/8）必须命中图缓存并跳过全部文件，否则上面那条『判失效』\
         是『永远重建』的假绿（实得 {c3:?}）"
    );
    assert_eq!(
        edge_multiset(&hit, &dir),
        rebuilt_edges,
        "[{repo}] 命中缓存读回的图必须与写入时逐条相等（含 4 个元信息字段）"
    );

    // ---- 第二道戳：只改 `--max-candidates`（8→3）也必须判失效 ----
    let mut p4 = CodeParser::new();
    p4.set_force_reindex(false);
    p4.set_resolver_mode(ResolverMode::Layered);
    p4.set_max_candidates(3);
    let _ = p4
        .build_petgraph_code_graph(&dir)
        .expect("候选上限不符的那一轮也必须能建出图");
    let c4 = p4.last_round_file_counts();
    assert_eq!(
        c4,
        (n_files, 0),
        "[{repo}] 档位戳里的 max_candidates 也参与门禁：缓存=8、本轮=3 ⇒ 必须全量重解析（实得 {c4:?}）"
    );

    println!(
        "[{repo}] 档位戳端到端：legacy 落盘 → layered 加载判失效({c2:?}) → 同档命中({c3:?}) → 改 max=3 再判失效({c4:?})"
    );
}

// ─── 交付物 1：`--levels` 非默认 ⇒ 必须配 `--force-reindex`（fail-fast）───

#[test]
fn p3b_levels_flag_fail_fast_requires_force_reindex() {
    let parse = |args: &[&str]| Cli::try_parse_from(args).unwrap_or_else(|e| panic!("clap 解析失败 {args:?}: {e}"));

    // (a) 非默认 `--levels` + `init` + 没有 `--force-reindex` ⇒ 可读的 Err（不是 panic）。
    let cli = parse(&["codeseek", "--levels=l1,l7", "init"]);
    let err = cli
        .validate_levels_against_cache()
        .expect_err("非默认 --levels 且未传 --force-reindex 必须被 fail-fast 拦住");
    println!("==== --levels 的 fail-fast 文案（原文）====\n{err}\n====");
    assert!(
        err.contains("--force-reindex"),
        "错误文案必须给出补救开关，实际：{err}"
    );
    assert!(
        err.contains("codeseek --levels=l1,l7 --force-reindex init"),
        "错误文案必须含**可直接照抄的正确用法示例**，实际：{err}"
    );
    assert!(
        err.contains("l1,l7"),
        "错误文案必须回显用户的取值，实际：{err}"
    );

    // (b) 同一个取值 + `--force-reindex` ⇒ 放行。
    let ok_forced = parse(&["codeseek", "--levels=l1,l7", "--force-reindex", "init"]);
    assert!(
        ok_forced.validate_levels_against_cache().is_ok(),
        "带了 --force-reindex 就必须放行"
    );

    // (c) 默认取值（空）⇒ 放行；等价于全开的写法（`all` / `l1-l8`）⇒ 也放行。
    for args in [
        vec!["codeseek", "init"],
        vec!["codeseek", "--levels=all", "init"],
        vec!["codeseek", "--levels=l1-l8", "init"],
        vec!["codeseek", "--levels=all,max=8", "init"],
    ] {
        let cli = parse(&args);
        assert!(
            cli.validate_levels_against_cache().is_ok(),
            "{args:?} 等价于默认（全层开启），不该触发 fail-fast"
        );
    }

    // (d) 真正关掉某一层（诊断用法）⇒ 必须触发。
    for spec in ["none", "p1", "l2-l6", "all,-l6", "l1"] {
        let cli = parse(&[
            "codeseek",
            &format!("--levels={spec}"),
            "init",
        ]);
        assert!(
            cli.levels_deviates_from_default(),
            "`--levels={spec}` 偏离默认（全层开启），必须被判定为非默认"
        );
        assert!(
            cli.validate_levels_against_cache().is_err(),
            "`--levels={spec}` 未配 --force-reindex 必须报错"
        );
    }

    // (e) 不建图的子命令不受这条守卫影响（`--levels` 对它们没有任何后果）。
    let not_init = parse(&["codeseek", "--levels=l1", "status"]);
    assert!(
        not_init.validate_levels_against_cache().is_ok(),
        "`--levels` 的守卫只管会读写图缓存的 `init`"
    );

    // (f) 语法非法的 spec **不**由本条守卫报错（两条错误不许互相盖掉）。
    let illegal = parse(&["codeseek", "--levels=l99", "init"]);
    assert!(
        illegal.levels_spec_is_invalid(),
        "`l99` 必须是语法非法（由既有 spec 校验报错）"
    );
    assert!(
        !illegal.levels_deviates_from_default(),
        "非法 spec 不该被本条守卫判成『非默认取值』"
    );
}
