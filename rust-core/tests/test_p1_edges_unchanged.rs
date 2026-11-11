//! Phase P1 的**边语义不变性守卫**（不是新期望，是"禁止变化"的护栏）。
//!
//! ## 这条守卫在保护什么
//! P1 的全部改动（`ImportDeclaration` 的 trait getter、Rust receiver、Java package/通配符、
//! 新建 `codegraph::resolution` 数据层、`CodeParser::file_scopes` 只建不消费的接线）
//! **一个都不许改变调用边**。本文件把 P0 实测出来的 8 项基线指标逐字段钉死：
//!
//! ```text
//! 函数节点总数            = 32
//! 调用边总数              = 13
//! is_resolved=true 边数   = 11
//! is_resolved=false 边数  = 2
//! 跨文件边数              = 4
//! 同名歧义调用点数        = 9
//! 幽灵(unresolved)节点数  = 2
//! 4 组同名歧义名的候选总数 = 13
//! ```
//!
//! 对照表出处：`rust-core/tests/fixtures/resolution/README.md`（7 个场景各自的
//! "当前解析结果 / 正确解析结果"），以及 `tests/test_resolution_baseline.rs`
//! 的 `test_baseline_aggregate_metrics_over_all_resolution_fixtures`
//! （那里只做了 `>=` 下界断言，本文件收紧成**精确相等**）。
//!
//! ## 为什么 P2/P3 也必须看这条测试
//! P2 把四处 first-wins 收敛成单一 resolver 时，这些数字**必然**改变（歧义候选被保留、
//! 误连边消失）。届时正确的做法是：改这张表并在 commit message 里逐项说明差异来源，
//! 而不是删掉本文件。测试名里的 `guard` 就是这个意思。
//!
//! ## 稳定性
//! 一律用 `CodeParser::set_force_reindex(true)` 从零全量解析（P0 交付物 2 的逃生口），
//! 因此不受 `~/.codeseek/projects/` 下的历史缓存影响；用例结束时清掉自己写进 home 的目录。

use std::path::{Path, PathBuf};

use codeseek::cli::args::ResolverMode;
use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::types::{FunctionInfo, PetCodeGraph};

/// 7 个消歧场景所在目录（整目录一次性建图 = P0 基线口径）
const RESOLUTION_ROOT: &str = "tests/fixtures/resolution";
/// 4 个整仓小项目（跨语言回归面，P0 之前就有）
const REPOS_ROOT: &str = "tests/test_repos";

/// 一次全量建图的可比较指标。**字段顺序即 P0 基线报告的顺序**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metrics {
    /// 图里的函数节点总数（含未解析调用造出的幽灵节点）
    function_nodes: usize,
    /// 调用边总数
    edges: usize,
    /// `is_resolved = true` 的边数
    resolved: usize,
    /// `is_resolved = false` 的边数
    unresolved: usize,
    /// caller 与 callee 落在不同文件里的边数
    cross_file_edges: usize,
    /// callee 名字在图里有 >1 个候选的边数（同名歧义调用点）
    ambiguous_call_sites: usize,
    /// `namespace == "unresolved"` 的幽灵节点数
    ghost_nodes: usize,
}

impl Metrics {
    /// 与 P0 完全一致（缺一项就把差异打全）
    fn assert_equals(&self, expected: &Metrics, label: &str) {
        let actual = format!("{:?}", self);
        let want = format!("{:?}", expected);
        assert_eq!(
            *self,
            *expected,
            "[{label}] 调用边语义发生变化！\n  P0/基线 = {want}\n  本次     = {actual}\n\
             若这是 P2/P3 的预期改动，请连同 tests/fixtures/resolution/README.md 一起更新本文件的表并说明原因。"
        );
    }

    /// 逐字段比较，但**跳过 `cross_file_edges`**，返回第一个不一致的字段名。
    ///
    /// 为什么不锁跨文件边数（仅限逐目录用例）：callee 命中哪一个同名候选取决于
    /// `_build_function_name_index` 的 `HashMap` 迭代顺序与 `or_insert` 的 first-wins
    /// （P0 基线文档里写明"取决于每进程随机哈希种子"），因此**同一份 fixture 在不同进程里
    /// 实测到的 cross_file_edges 会抖**（`simple_ts_project` 8↔13、`simple_js_project` 31↔35）。
    /// 锁它只会产出 flaky 测试。聚合口径（守卫 1）里的 4 之所以能锁：那 7 个场景的歧义名
    /// 候选无论选中哪一个都同样是跨文件边，计数不变 —— 这一点由守卫 1 的重复运行验证。
    fn first_difference_ignoring_cross_file(&self, expected: &Metrics) -> Option<&'static str> {
        if self.function_nodes != expected.function_nodes {
            return Some("function_nodes");
        }
        if self.edges != expected.edges {
            return Some("edges");
        }
        if self.resolved != expected.resolved {
            return Some("resolved");
        }
        if self.unresolved != expected.unresolved {
            return Some("unresolved");
        }
        if self.ambiguous_call_sites != expected.ambiguous_call_sites {
            return Some("ambiguous_call_sites");
        }
        if self.ghost_nodes != expected.ghost_nodes {
            return Some("ghost_nodes");
        }
        None
    }
}

/// 一次建图的产物：图 + 建图用的 parser（`file_scopes` 要从 parser 上读）。
struct Built {
    _temp: tempfile::TempDir,
    project_dir: PathBuf,
    graph: PetCodeGraph,
    parser: CodeParser,
}

impl Drop for Built {
    fn drop(&mut self) {
        // 清掉本次写进 ~/.codeseek/projects 的增量哈希目录（project_id 有两种形态，都试）。
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

impl Built {
    /// 把 `src` 整目录复制进唯一命名的临时项目目录，然后走**真实 parser 全量链路**建图。
    fn build(label: &str, rel_src: &str) -> Self {
        Self::build_with(label, rel_src, None)
    }

    /// Phase P2a 交付物 3 的验证入口：同一条全量建图链路，但把 `--resolver` /
    /// `--max-candidates` / `--levels` 三枚 flag 注入 `CodeParser`。
    ///
    /// ⚠ **P2a 里这三个开关不参与产边**（任务红线 2）。因此带 `flags` 与不带的两次建图，
    /// 守卫表的 8 项指标**必须逐字段相同**，全部调用边的多重集也必须相同。
    /// 见 [`p2a_guard_tables_unchanged_under_both_resolver_modes`]。
    fn build_with(label: &str, rel_src: &str, flags: Option<ResolverFlags>) -> Self {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_src);
        assert!(src.is_dir(), "fixture 目录不存在: {}", src.display());

        let temp = tempfile::TempDir::new().expect("创建临时目录失败");
        let project_dir = temp
            .path()
            .join(format!("p1guard_{}_{}", label, std::process::id()));
        copy_tree(&src, &project_dir);

        let mut parser = CodeParser::new();
        parser.set_force_reindex(true);
        match flags {
            Some(flags) => flags.apply(&mut parser),
            // Phase P3 交付物 3：生产默认档位已切到 layered，但本文件的守卫表全部是
            // **legacy 口径**（红线 2 的 32/13/... 基线）。因此"不带 flag"的建图必须
            // **显式**钉成 Legacy，绝不隐式依赖全局默认值 —— 否则默认一翻，legacy 基线
            // 会静默变成 layered 结果，红线名存实亡。
            None => parser.set_resolver_mode(ResolverMode::Legacy),
        }
        let graph = parser
            .build_petgraph_code_graph(&project_dir)
            .unwrap_or_else(|e| panic!("[{label}] 建图失败: {e}"));

        Self {
            _temp: temp,
            project_dir,
            graph,
            parser,
        }
    }

    fn all_functions(&self) -> Vec<FunctionInfo> {
        self.graph
            .get_all_functions()
            .into_iter()
            .cloned()
            .collect()
    }

    /// `function_names` 索引里某个名字的候选个数（P0 的"候选总数"口径就是这个索引）。
    fn candidate_count(&self, name: &str) -> usize {
        self.graph.function_names.get(name).map(|v| v.len()).unwrap_or(0)
    }

    fn edges(&self) -> Vec<(Option<FunctionInfo>, Option<FunctionInfo>, String, bool)> {
        self.graph
            .graph
            .edge_references()
            .map(|edge| {
                let rel = edge.weight();
                (
                    self.graph.get_function_by_id(&rel.caller_id).cloned(),
                    self.graph.get_function_by_id(&rel.callee_id).cloned(),
                    rel.callee_name.clone(),
                    rel.is_resolved,
                )
            })
            .collect()
    }

    /// 算出全部守卫指标（口径与 `test_resolution_baseline.rs` 的汇总测试**逐行一致**）。
    fn metrics(&self) -> Metrics {
        let functions = self.all_functions();
        let edges = self.edges();
        let resolved = edges.iter().filter(|(_, _, _, is_resolved)| *is_resolved).count();
        Metrics {
            function_nodes: functions.len(),
            edges: edges.len(),
            resolved,
            unresolved: edges.len() - resolved,
            cross_file_edges: edges
                .iter()
                .filter(|(caller, callee, _, _)| match (caller, callee) {
                    (Some(caller), Some(callee)) => caller.file_path != callee.file_path,
                    _ => false,
                })
                .count(),
            ambiguous_call_sites: edges
                .iter()
                .filter(|(_, _, callee_name, _)| self.candidate_count(callee_name) > 1)
                .count(),
            ghost_nodes: functions.iter().filter(|f| f.namespace == "unresolved").count(),
        }
    }

    /// 4 组同名歧义名（helper/pick/add/run）的候选总数。
    fn ambiguous_name_candidates(&self) -> usize {
        ["helper", "pick", "add", "run"]
            .iter()
            .map(|name| self.candidate_count(name))
            .sum()
    }

    /// 全部调用边的**规范化多重集**（不含 uuid，只留源码事实）：
    /// `(caller 相对文件, caller 名, caller 定义行, callee 相对文件, callee 名, callee 定义行, 调用行, is_resolved)`。
    ///
    /// 用途：证明 `--resolver=layered` 在 P2a 对图**零影响** —— 比 8 项聚合指标强得多
    /// （聚合指标相同仍可能"换了一条边"）。
    fn normalized_edge_multiset(&self) -> Vec<(String, String, usize, String, String, usize, usize, bool)> {
        let root = self.project_dir.clone();
        let rel = |p: &Path| -> String {
            p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/")
        };
        let mut out: Vec<(String, String, usize, String, String, usize, usize, bool)> = self
            .graph
            .graph
            .edge_references()
            .map(|edge| {
                let rel_edge = edge.weight();
                let caller = self.graph.get_function_by_id(&rel_edge.caller_id);
                let callee = self.graph.get_function_by_id(&rel_edge.callee_id);
                (
                    caller.as_ref().map(|f| rel(&f.file_path)).unwrap_or_default(),
                    caller.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                    caller.as_ref().map(|f| f.line_start).unwrap_or(0),
                    callee.as_ref().map(|f| rel(&f.file_path)).unwrap_or_default(),
                    callee.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
                    callee.as_ref().map(|f| f.line_start).unwrap_or(0),
                    rel_edge.line_number,
                    rel_edge.is_resolved,
                )
            })
            .collect();
        out.sort();
        out
    }

    /// [`Built::normalized_edge_multiset`] 去掉 **callee 文件** 那一维的版本。
    ///
    /// 只保留"caller 是谁、在第几行调用了哪个同名函数、以及那条边是否被 legacy 声称已解析"，
    /// 因为"两份同名候选里选哪一份"正是 legacy first-wins 的随机面（见守卫 4(b) 的注释）。
    fn normalized_edge_multiset_without_callee_file(
        &self,
    ) -> Vec<(String, String, usize, String, usize, bool)> {
        self.normalized_edge_multiset()
            .into_iter()
            .map(|(cf, cn, cl, _callee_file, cn2, _cl2, line, resolved)| {
                (cf, cn, cl, cn2, line, resolved)
            })
            .collect()
    }

    /// **调用点**的身份集合：`(caller 相对文件, caller 名, 调用行)`。
    ///
    /// ⚠ 判据里**故意不含 callee 名**（P2b 收口实测教训）：`CallRelation.callee_name` 记的是
    /// **解析结果名**而不是调用点裸名，两种模式在同一条边上填的东西本就不同 ——
    /// legacy 对 `alias(value)` 解析失败 ⇒ 填裸名 `alias`（连本文件幽灵节点）；
    /// layered 用 L4-ImportBinding 命中 `mod_x.py:4 def func` ⇒ 填 `func`。
    /// 拿 callee 名当调用点身份会把"这次解析得更对了"误报成"这个调用点丢了"。
    /// 该口径差异已写进 `tests/fixtures/resolution/README.md` 的 legacy/layered 分叉表。
    fn call_site_set(&self) -> std::collections::HashSet<(String, String, usize)> {
        self.call_site_edge_counts().into_keys().collect()
    }

    /// `调用点身份 -> 该调用点上的边条数`。用于证明"边数上升"来自同一调用点的
    /// 候选展开（口径 A），而不是凭空多出调用点（口径 B 的覆盖中性）。
    fn call_site_edge_counts(&self) -> std::collections::BTreeMap<(String, String, usize), usize> {
        let mut m = std::collections::BTreeMap::new();
        for edge in self.graph.graph.edge_references() {
            let rel = edge.weight();
            let caller = self.graph.get_function_by_id(&rel.caller_id);
            let key = (
                caller
                    .map(|f| {
                        f.file_path
                            .strip_prefix(&self.project_dir)
                            .unwrap_or(f.file_path.as_path())
                            .to_string_lossy()
                            .replace('\\', "/")
                    })
                    .unwrap_or_default(),
                caller.map(|f| f.name.clone()).unwrap_or_default(),
                rel.line_number,
            );
            *m.entry(key).or_insert(0usize) += 1;
        }
        m
    }

    /// 每条边的 callee 身份投影：`(裸名, callee 相对文件, callee 定义行, is_resolved)`，
    /// 供差集取证打印用（不参与断言 —— 该维在 legacy 下与哈希种子有关）。
    fn callee_identities(&self) -> Vec<(String, String, usize, bool)> {
        let mut out: Vec<(String, String, usize, bool)> = self
            .graph
            .graph
            .edge_references()
            .map(|edge| {
                let rel = edge.weight();
                let callee = self.graph.get_function_by_id(&rel.callee_id);
                (
                    rel.callee_name.clone(),
                    callee
                        .map(|f| {
                            f.file_path
                                .strip_prefix(&self.project_dir)
                                .unwrap_or(f.file_path.as_path())
                                .to_string_lossy()
                                .replace('\\', "/")
                        })
                        .unwrap_or_default(),
                    callee.map(|f| f.line_start).unwrap_or(0),
                    rel.is_resolved,
                )
            })
            .collect();
        out.sort();
        out
    }
}

/// Phase P2a 交付物 3：CLI 三枚 flag 的测试侧载体（`--resolver` / `--max-candidates` / `--levels`）。
#[derive(Debug, Clone, Copy)]
struct ResolverFlags {
    resolver: ResolverMode,
    max_candidates: usize,
    levels: Option<&'static str>,
}

impl ResolverFlags {
    /// 与 `services/analyzer.rs::configure_resolver`（`--force-reindex` 的同一条注入链）同口径。
    fn apply(&self, parser: &mut CodeParser) {
        parser.set_resolver_mode(self.resolver);
        parser.set_max_candidates(self.max_candidates);
        parser.set_levels_spec(self.levels.map(|s| s.to_string()));
    }

    /// `--resolver=layered --max-candidates=8`（即"不带任何 legacy 之外的行为，但把开关拨过去"）。
    const LAYERED: ResolverFlags = ResolverFlags {
        resolver: ResolverMode::Layered,
        max_candidates: 8,
        levels: None,
    };

    /// 最激进的一组：layered + 上限 1 + 只开 L5 —— 若哪天有人把它接进产边，这条守卫会立刻响。
    const LAYERED_STRICT: ResolverFlags = ResolverFlags {
        resolver: ResolverMode::Layered,
        max_candidates: 1,
        levels: Some("l5"),
    };
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

// ============================================================================================
// 守卫 1：P0 汇总基线 —— 8 项指标逐字段精确相等（32/13/11/2/4/9/2/13）
// ============================================================================================

/// P0 实测的**汇总**基线（空 case = 以 `tests/fixtures/resolution` 整目录为项目根，
/// `README.md` 会被 `scan_directory` 过滤掉）。
const EXPECTED_AGGREGATE: Metrics = Metrics {
    function_nodes: 32,
    edges: 13,
    resolved: 11,
    unresolved: 2,
    cross_file_edges: 4,
    ambiguous_call_sites: 9,
    ghost_nodes: 2,
};
const EXPECTED_AGGREGATE_CANDIDATES: usize = 13;

#[test]
fn p1_guard_p0_aggregate_baseline_is_byte_for_byte_unchanged() {
    let built = Built::build("aggregate", RESOLUTION_ROOT);
    let actual = built.metrics();
    let candidates = built.ambiguous_name_candidates();

    println!("===== Phase P1 边语义不变性守卫：tests/fixtures/resolution 全量建图 =====");
    println!("函数节点总数            = {}", actual.function_nodes);
    println!("调用边总数              = {}", actual.edges);
    println!("is_resolved=true 边数   = {}", actual.resolved);
    println!("is_resolved=false 边数  = {}", actual.unresolved);
    println!("跨文件边数              = {}", actual.cross_file_edges);
    println!("同名歧义调用点数        = {}", actual.ambiguous_call_sites);
    println!("幽灵(unresolved)节点数  = {}", actual.ghost_nodes);
    println!("4 组同名歧义名的候选总数 = {candidates}");
    println!(
        "stats.resolved/unresolved = {}/{}",
        built.graph.get_stats().resolved_calls,
        built.graph.get_stats().unresolved_calls
    );
    println!("P0 基线口径（必须完全一致）= 32/13/11/2/4/9/2/13");
    println!("==========================================================================");

    actual.assert_equals(&EXPECTED_AGGREGATE, "tests/fixtures/resolution 全量");
    assert_eq!(
        candidates, EXPECTED_AGGREGATE_CANDIDATES,
        "4 组同名歧义名（helper/pick/add/run）的候选总数必须仍是 {EXPECTED_AGGREGATE_CANDIDATES}"
    );
}

// ============================================================================================
// 守卫 2：逐目录基线 —— 7 个消歧场景 + 4 个整仓小项目，一个数字都不许动
// ============================================================================================

/// `(目录, 期望指标, 同名候选总数)`。
/// 期望值 = HEAD(`851f7fb`, P0 之后 / P1 之前) 的实测值，由本文件的
/// `p1_guard_per_case_metrics_are_unchanged` 用 `--nocapture` 打印后逐条抄录；
/// P1 的任何改动都不应该让任何一格发生变化。
///
/// ⚠ 表里 `cross_file_edges` 这一格**只记录、不断言**（原因见
/// [`Metrics::first_difference_ignoring_cross_file`]）；其余 6 项与候选总数逐字段断言。
const PER_CASE_BASELINE: &[(&str, Metrics, usize)] = &[
    // ---- 7 个消歧场景（tests/fixtures/resolution/）----
    // 与 `tests/test_resolution_baseline.rs` 的逐场景断言互相印证：
    //   cross_file_same_name 3 条边全 is_resolved=true（同名歧义被谎报为已解析）
    ("cross_file_same_name", Metrics { function_nodes: 5, edges: 3, resolved: 3, unresolved: 0, cross_file_edges: 1, ambiguous_call_sites: 3, ghost_nodes: 0 }, 2),
    //   python_import_alias 唯一那条边 is_resolved=false，callee 是本文件幽灵节点 `alias`
    ("python_import_alias", Metrics { function_nodes: 4, edges: 1, resolved: 0, unresolved: 1, cross_file_edges: 0, ambiguous_call_sites: 0, ghost_nodes: 1 }, 0),
    ("rust_qualified_path", Metrics { function_nodes: 4, edges: 2, resolved: 2, unresolved: 0, cross_file_edges: 1, ambiguous_call_sites: 2, ghost_nodes: 0 }, 2),
    ("python_dotted_qualified", Metrics { function_nodes: 3, edges: 1, resolved: 1, unresolved: 0, cross_file_edges: 1, ambiguous_call_sites: 1, ghost_nodes: 0 }, 2),
    ("method_receiver", Metrics { function_nodes: 4, edges: 1, resolved: 1, unresolved: 0, cross_file_edges: 1, ambiguous_call_sites: 1, ghost_nodes: 0 }, 2),
    //   nested_function_caller = 4 个真实函数 + 1 个闭包幽灵节点 `doubled`，3 条边（2 解析 / 1 未解析）
    ("nested_function_caller", Metrics { function_nodes: 5, edges: 3, resolved: 2, unresolved: 1, cross_file_edges: 0, ambiguous_call_sites: 0, ghost_nodes: 1 }, 0),
    //   ambiguous_trait_impl 的 5 个同名 `run` 候选被压成 2 条边
    ("ambiguous_trait_impl", Metrics { function_nodes: 7, edges: 2, resolved: 2, unresolved: 0, cross_file_edges: 0, ambiguous_call_sites: 2, ghost_nodes: 0 }, 5),
    // ---- 4 个整仓小项目（tests/test_repos/）----
    ("simple_rust_project", Metrics { function_nodes: 46, edges: 44, resolved: 34, unresolved: 10, cross_file_edges: 6, ambiguous_call_sites: 0, ghost_nodes: 10 }, 1),
    ("simple_python_project", Metrics { function_nodes: 45, edges: 48, resolved: 32, unresolved: 16, cross_file_edges: 10, ambiguous_call_sites: 0, ghost_nodes: 16 }, 0),
    ("simple_ts_project", Metrics { function_nodes: 92, edges: 101, resolved: 70, unresolved: 31, cross_file_edges: 8, ambiguous_call_sites: 4, ghost_nodes: 31 }, 1),
    ("simple_js_project", Metrics { function_nodes: 1577, edges: 746, resolved: 546, unresolved: 200, cross_file_edges: 31, ambiguous_call_sites: 187, ghost_nodes: 200 }, 3),
];

#[test]
fn p1_guard_per_case_metrics_are_unchanged() {
    let mut cases: Vec<(&str, &'static str)> = Vec::new();
    for name in [
        "cross_file_same_name",
        "python_import_alias",
        "rust_qualified_path",
        "python_dotted_qualified",
        "method_receiver",
        "nested_function_caller",
        "ambiguous_trait_impl",
    ] {
        cases.push((name, RESOLUTION_ROOT));
    }
    for name in [
        "simple_rust_project",
        "simple_python_project",
        "simple_ts_project",
        "simple_js_project",
    ] {
        cases.push((name, REPOS_ROOT));
    }

    let mut failures: Vec<String> = Vec::new();
    for (case, root) in cases {
        let rel = format!("{root}/{case}");
        let built = Built::build(case, &rel);
        let actual = built.metrics();
        let candidates = built.ambiguous_name_candidates();
        println!("CASE {case}\t{actual:?}\tcandidates={candidates}");

        match PER_CASE_BASELINE.iter().find(|(name, _, _)| *name == case) {
            Some((_, expected, expected_candidates)) => {
                // 逐目录口径只锁"与 HashMap 顺序无关"的 6 项 + 同名候选总数；
                // cross_file_edges 仍然打印出来供人工比对，但不参与断言（见上面的说明）。
                if let Some(field) = actual.first_difference_ignoring_cross_file(expected) {
                    failures.push(format!(
                        "[{case}] 字段 `{field}` 变化：基线 = {expected:?}，本次 = {actual:?}"
                    ));
                }
                if candidates != *expected_candidates {
                    failures.push(format!(
                        "[{case}] 同名候选总数变化：基线 = {expected_candidates}，本次 = {candidates}"
                    ));
                }
            }
            None => failures.push(format!("[{case}] 基线表里缺这一条用例（不许静默漏掉）")),
        }
    }

    assert!(
        failures.is_empty(),
        "P1 改动破坏了调用边语义：\n{}",
        failures.join("\n")
    );
}

// ============================================================================================
// 守卫 3：只建不消费的自检 —— file_scopes 必须存在，且不得影响任何边
// ============================================================================================

#[test]
fn p1_guard_resolution_context_is_built_but_consumes_nothing() {
    let built = Built::build("ctx", &format!("{}/method_receiver", RESOLUTION_ROOT));
    // 建了：每个被解析的文件都应当有 ctx（P1 交付物 3 的接线证据）。
    assert!(
        built.parser.file_scope_count() > 0,
        "`CodeParser::file_scopes` 必须在全量建图后被填上（P1 交付物 3 的接线证据）"
    );

    // 找到了：`receiver_user.rs` 的 ctx 里作用域树与 import 表都已就位（交付物 2/3 的证据）。
    let path = built.project_dir.join("receiver_user.rs");
    let ctx = built
        .parser
        .file_scope_ctx(&path)
        .unwrap_or_else(|| panic!("{} 的 FileScopeCtx 应存在", path.display()));
    assert!(
        !ctx.scopes.is_empty(),
        "method_receiver/receiver_user.rs 应有作用域树，module_path = {}",
        ctx.module_path
    );
    assert_eq!(
        ctx.module_path, "receiver_user",
        "module path 必须由 `CodeParser` 记录的仓库根推导出来"
    );
    // P2 需要的原料现在拿得到：调用点行号 → 最内层作用域。
    assert!(
        ctx.innermost_scope_containing(1).is_some(),
        "文件根必须覆盖第 1 行（innermost 判定不依赖符号产出顺序）"
    );
}

// ============================================================================================
// 辅助：打印逐条基线（只在需要重新抄表时读输出，本身不做任何断言）
// ============================================================================================

#[test]
fn p1_guard_print_every_case_table_for_transcription() {
    // `-- --nocapture` 查看；输出即 PER_CASE_BASELINE 的抄录来源。
    println!("(见 p1_guard_per_case_metrics_are_unchanged 的 CASE 行)");
}

// ============================================================================================
// 守卫 4（Phase P2b 收口，任务口径 C）：**分叉**基线 —— legacy 段沿用 P0 表，layered 段用新表
//
// ## 为什么这里从"三段相等"变成"两段分叉"
// P2a 时 `--resolver` 三枚 flag 只存不用，所以本守卫的形态是
// "legacy == layered == strict == 32/13/11/2/4/9/2/13"。
// P2b 把引擎接进产边链路之后，"相等"这件事**必然**不成立（口径 A：每个候选一条边）。
// 任务口径 C 要求：**分叉而不是覆盖** —— legacy 段一个数字都不许改（红线 1），
// layered 段用实测值钉死成新基线，差值来源逐项写进 `tests/fixtures/resolution/README.md`。
//
// ## 两段表
// ```text
// legacy （= P0/P1/P2a 基线，必须逐字节不变）
//        函数节点 32 / 边 13 / resolved 11 / unresolved 2 / 跨文件 4 / 同名歧义 9 / 幽灵 2 / 候选 13
// layered（P2b 接线后的新基线，`--max-candidates=8`、全层开启）
//        函数节点 31 / 边 20 / resolved 10 / unresolved 10 / 跨文件 12 / 同名歧义 16 / 幽灵 1 / 候选 13
// ```
//
// ## `function_nodes 32 → 31` 的定性（任务口径 D）
// 见 [`p2b_layered_guard_table_is_the_forked_baseline`] 里的差集打印与断言：
// 少掉的那一个是 `python_import_alias/consumer.py` 上由 legacy 的未解析调用临时造的
// **幽灵节点** `alias`（`namespace == "unresolved"`），layered 用 L4-ImportBinding 把
// `alias(a)` 正确解析到 `mod_x.py:4` 的真定义，于是**不再需要**造幽灵节点。
// ⇒ 结论是**口径差异（②）而不是节点丢失（①）**：真实定义节点在两种模式下恒等 30 个
// （本文件用 `ghost == false` 的节点子集逐条断言相等来钉死这一点）。
// ============================================================================================

/// Phase P2b 分叉基线的 **layered 段**（`--resolver=layered --max-candidates=8`，全层开启）。
///
/// 抄录来源：`cargo test --test test_p1_edges_unchanged p2b_layered_guard_table -- --nocapture`
/// 的 `P2B LAYERED BASELINE` 行（同一份 fixture、同一条真实建图链路，只是 `--resolver` 拨到 layered）。
const EXPECTED_LAYERED_AGGREGATE: Metrics = Metrics {
    function_nodes: 31,
    edges: 20,
    resolved: 10,
    unresolved: 10,
    cross_file_edges: 12,
    ambiguous_call_sites: 16,
    ghost_nodes: 1,
};
/// layered 段候选总数与 legacy 段**相同**（13）：候选来自图中的同名定义节点，
/// `--resolver` 不增删定义，只改变"保留几个候选"。
const EXPECTED_LAYERED_AGGREGATE_CANDIDATES: usize = 13;

/// **legacy 段（红线 1）**：默认路径必须与 P2a（`8e62033`）逐字节等价。
///
/// 三条子断言合起来才是"默认行为没被 P2b 收口改动碰过"的证据：
///  (a) 不带任何 flag 的聚合守卫表 == P0 的 `32/13/11/2/4/9/2/13`（含候选总数 13）；
///  (b) 显式 `--resolver=legacy` 与"不带 flag"必须完全一致（默认值没有隐藏分支）；
///  (c) 边的规范化多重集（含 callee 身份）在两种 legacy 配置之间逐条相等。
#[test]
fn p2b_legacy_guard_table_is_byte_for_byte_unchanged() {
    let implicit = Built::build("p2b_legacy_implicit", RESOLUTION_ROOT);
    let explicit = Built::build_with(
        "p2b_legacy_explicit",
        RESOLUTION_ROOT,
        Some(ResolverFlags {
            resolver: ResolverMode::Legacy,
            max_candidates: 1,
            levels: Some("l5"),
        }),
    );

    // ⚠ Phase P3 交付物 3：全局默认档位已由 legacy 切到 layered。本 `implicit` 走
    // `Built::build`（= `build_with(None)`），而 `build_with(None)` 现在**显式**钉 Legacy
    // （见其注释），所以这里比的是"本文件 legacy 建图入口" 与 "显式 --resolver=legacy"
    // 两条 legacy 路径是否逐字节一致 —— 不再依赖、也不验证全局默认值。
    // "全局默认 == Layered" 改由 [`p3_global_default_resolver_is_layered`] 单独钉。
    for (label, built) in [("本文件 legacy 入口（build→显式钉 Legacy）", &implicit), ("显式 --resolver=legacy", &explicit)] {
        let actual = built.metrics();
        let candidates = built.ambiguous_name_candidates();
        println!("P2B LEGACY {label}\t{actual:?}\tcandidates={candidates}");
        actual.assert_equals(&EXPECTED_AGGREGATE, label);
        assert_eq!(
            candidates, EXPECTED_AGGREGATE_CANDIDATES,
            "[{label}] 4 组同名歧义名的候选总数必须仍是 {EXPECTED_AGGREGATE_CANDIDATES}"
        );
    }

    // (c) legacy 的 `--max-candidates` / `--levels` 在 legacy 模式下**必须仍是死参数**
    // （它们只配置分层引擎；legacy 不消费引擎），所以多重集必须逐条相等。
    //
    // ⚠ 判据取的是 [`Built::normalized_edge_multiset_without_callee_file`] —— 即
    // **只放开 callee 的身份维（文件 + 定义行）**，其余 6 维
    // `(caller 文件, caller 名, caller 定义行, callee 名, 调用行, is_resolved)` 逐条锁死。
    // 这不是放宽，而是因为"含 callee 文件"这一条**在任何实现下都不可能稳定成立**：
    // legacy 的同名 first-wins 取值来自 `_build_function_name_index` 的 `HashMap`
    // 迭代顺序，其种子是每进程（甚至每多次建图）随机的。实测（同一份
    // `tests/fixtures/resolution`、16 轮独立全量建图）：
    // ```text
    // edges / nodes 每轮恒为 13 / 32（与种子无关的量），但
    //   gamma.rs:6            call_helper  -> alpha.rs:4 x2 | beta.rs:4 x3 | mod_y.py:4 x4 | other_mod.py:4 x7
    //   dotted_consumer.py:10 call_dotted  -> alpha.rs:4 x2 | beta.rs:4 x3 | mod_y.py:4 x4 | other_mod.py:4 x7
    //   receiver_user.rs:11   use_method   -> calc.rs:9 x9  | misc.rs:4   x7
    //   qualifier.rs:9        call_qualified -> mod_a.rs:3 x8 | mod_b.rs:3 x8
    // ```
    // P0/P1 的守卫正是基于同一事实不敢锁 `cross_file_edges`（见
    // [`Metrics::first_difference_ignoring_cross_file`]）。
    // 被放开的那一维由 (c2) 重新约束回"必须是该裸名的图内候选之一"，
    // 因此 (c) + (c2) 的总约束强度**高于**原先那条永不可能稳定的断言。
    assert_eq!(
        implicit.normalized_edge_multiset_without_callee_file(),
        explicit.normalized_edge_multiset_without_callee_file(),
        "legacy 下 `--max-candidates` / `--levels` 不得影响任何一条边（callee 身份维除外，理由见上方注释与 (c2)）"
    );

    // (c2) callee 身份维的合法域：每条边的 callee 必须是"该边记录的裸名在图索引里的候选之一"，
    // 或者是该调用点自己造的未解析幽灵节点。⇒ 随机面被约束在合法候选集合内，
    // 绝不允许 first-wins 指到名字无关的函数。
    let mut illegal_callee: Vec<String> = Vec::new();
    for (label, built) in [("implicit", &implicit), ("explicit", &explicit)] {
        for edge in built.graph.graph.edge_references() {
            let rel = edge.weight();
            let Some(callee) = built.graph.get_function_by_id(&rel.callee_id) else {
                illegal_callee.push(format!("[{label}] {}:{} 的 callee 节点根本不在图里", rel.caller_name, rel.line_number));
                continue;
            };
            let is_ghost = callee.namespace == "unresolved";
            let in_candidate_set = built
                .graph
                .function_names
                .get(rel.callee_name.as_str())
                .map(|ids| ids.contains(&rel.callee_id))
                .unwrap_or(false);
            if !is_ghost && !in_candidate_set {
                illegal_callee.push(format!(
                    "[{label}] {}:{} `{}` -> `{}:{}` 既不是裸名候选也不在幽灵索引里",
                    rel.caller_name,
                    rel.line_number,
                    rel.callee_name,
                    callee
                        .file_path
                        .strip_prefix(&built.project_dir)
                        .unwrap_or(callee.file_path.as_path())
                        .to_string_lossy(),
                    callee.line_start
                ));
            }
        }
    }
    assert!(
        illegal_callee.is_empty(),
        "legacy 的 callee 必须在合法候选域内（这是 (c) 放开 callee 身份维后补的约束）：\n{}",
        illegal_callee.join("\n")
    );

    // (d) 四个新标量在 legacy 产出的每一条边上都必须恒为 0（P2b 的落盘默认值口径）。
    let zeros: Vec<(u8, u8, u8, u8)> = implicit
        .graph
        .graph
        .edge_references()
        .map(|e| {
            let r = e.weight();
            (r.resolution_level, r.confidence, r.rank, r.candidate_count)
        })
        .collect();
    assert!(
        zeros.iter().all(|t| *t == (0, 0, 0, 0)),
        "legacy 的边必须四标量全 0，实际非零的有 {:?}",
        zeros.iter().filter(|t| **t != (0, 0, 0, 0)).count()
    );

    // (e) legacy 一律不给节点补写身份字段（`module_path` / `qualified_name` 恒 None）。
    let with_identity = implicit
        .all_functions()
        .into_iter()
        .filter(|f| f.module_path.is_some() || f.qualified_name.is_some())
        .count();
    assert_eq!(
        with_identity, 0,
        "legacy 不得写节点身份字段（layered 专属），实际 {} 个节点被写了",
        with_identity
    );
}

/// **layered 段（任务口径 C/D）**：接线之后的新基线 + 与 legacy 的差集取证。
#[test]
fn p2b_layered_guard_table_is_the_forked_baseline() {
    let legacy = Built::build("p2b_fork_legacy", RESOLUTION_ROOT);
    let layered = Built::build_with("p2b_fork_layered", RESOLUTION_ROOT, Some(ResolverFlags::LAYERED));

    // ---- (0) flag 真的被读进来了（否则比的是同一份配置，断言是空的）----
    // 注：`Built::build(None)` 现在显式钉 Legacy（P3 起全局默认已切 layered），
    // 所以这里核的是"legacy 入口确实拿到 Legacy"，而非"全局默认是 legacy"。
    assert_eq!(legacy.parser.resolver_mode(), ResolverMode::Legacy, "legacy 入口必须被显式钉成 Legacy");
    assert_eq!(layered.parser.resolver_mode(), ResolverMode::Layered);
    assert_eq!(layered.parser.max_candidates(), 8);
    assert!(layered.parser.levels_spec().is_none(), "不带 `--levels` 必须是全层");

    // ---- (1) 两段各自的精确基线 ----
    let legacy_metrics = legacy.metrics();
    let layered_metrics = layered.metrics();
    println!(
        "P2B LEGACY BASELINE   {} 候选={}",
        format!(
            "节点={}/边={}/resolved={}/unresolved={}/跨文件={}/同名歧义={}/幽灵={}",
            legacy_metrics.function_nodes,
            legacy_metrics.edges,
            legacy_metrics.resolved,
            legacy_metrics.unresolved,
            legacy_metrics.cross_file_edges,
            legacy_metrics.ambiguous_call_sites,
            legacy_metrics.ghost_nodes
        ),
        legacy.ambiguous_name_candidates()
    );
    println!(
        "P2B LAYERED BASELINE  {} 候选={}",
        format!(
            "节点={}/边={}/resolved={}/unresolved={}/跨文件={}/同名歧义={}/幽灵={}",
            layered_metrics.function_nodes,
            layered_metrics.edges,
            layered_metrics.resolved,
            layered_metrics.unresolved,
            layered_metrics.cross_file_edges,
            layered_metrics.ambiguous_call_sites,
            layered_metrics.ghost_nodes
        ),
        layered.ambiguous_name_candidates()
    );
    // legacy 段：与 P0/P1/P2a 完全一致（同 [`p2b_legacy_guard_table_is_byte_for_byte_unchanged`]，
    // 这里再对一次是为了让"分叉"两段在同一份输出里可比）。
    legacy_metrics.assert_equals(&EXPECTED_AGGREGATE, "分叉守卫 legacy 段");
    // layered 段：新基线逐字段精确相等。
    layered_metrics.assert_equals(&EXPECTED_LAYERED_AGGREGATE, "分叉守卫 layered 段");
    assert_eq!(
        layered.ambiguous_name_candidates(),
        EXPECTED_LAYERED_AGGREGATE_CANDIDATES,
        "layered 段的同名候选总数必须与 legacy 段相同（`--resolver` 不增删定义节点）"
    );

    // ---- (2) 口径 D 取证：函数节点集合按 `(相对文件, 行号, 名字, 是否幽灵)` 做差集 ----
    let key = |b: &Built| std::collections::HashSet::<(String, String, usize, bool)>::from_iter(
        b.all_functions().into_iter().map(|f| {
            let rel = f
                .file_path
                .strip_prefix(&b.project_dir)
                .unwrap_or(f.file_path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            (rel, f.name.clone(), f.line_start, f.namespace == "unresolved")
        }),
    );
    let legacy_nodes = key(&legacy);
    let layered_nodes = key(&layered);
    let only_in_legacy: Vec<String> = legacy_nodes
        .difference(&layered_nodes)
        .map(|(file, name, line, ghost)| format!("{file}:{line}#{name}(ghost={ghost})"))
        .collect();
    let only_in_layered: Vec<String> = layered_nodes
        .difference(&legacy_nodes)
        .map(|(file, name, line, ghost)| format!("{file}:{line}#{name}(ghost={ghost})"))
        .collect();
    println!("P2B 函数节点差集（口径 D 取证，layered 相对 legacy）：");
    println!("  legacy minus layered = {:?}", {
        let mut v = only_in_legacy.clone();
        v.sort();
        v
    });
    println!("  layered minus legacy = {:?}", {
        let mut v = only_in_layered.clone();
        v.sort();
        v
    });

    // **少掉的那一个必须是幽灵节点**，而且是 `python_import_alias/consumer.py` 的 `alias`：
    // legacy 把 `alias(a)` 判成未解析 ⇒ 造 `namespace="unresolved"` 的临时节点；
    // layered 用 L4-ImportBinding 命中 `mod_x.py:4 def alias(...)` ⇒ 不需要造幽灵。
    assert!(
        only_in_layered.is_empty(),
        "layered 不得凭空多出任何函数节点（口径 D ①：节点丢失的反向情形同样不允许）：{:?}",
        only_in_layered
    );
    assert_eq!(
        only_in_legacy.len(),
        1,
        "32→31 必须恰好少一个节点，实际少 {:?}（若差集变化请连同 README 的分叉表一起更新）",
        only_in_legacy
    );
    let gone = &only_in_legacy[0];
    assert!(
        gone.contains("#alias(ghost=true)"),
        "少掉的那一个必须是 legacy 造的未解析幽灵 `alias`，实际 = {gone}"
    );
    assert_eq!(legacy_nodes.len() - layered_nodes.len(), 1, "节点差集必须恰好为 1");
    // 真实定义（非幽灵）节点集合两模式**逐元素相等** ⇒ 证明 32→31 是统计口径差异，
    // 不是"layered 建图少写了一个函数节点"。
    let real = |set: &std::collections::HashSet<(String, String, usize, bool)>| {
        set.iter()
            .filter(|(_, _, _, ghost)| !*ghost)
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut real_legacy = real(&legacy_nodes);
    let mut real_layered = real(&layered_nodes);
    real_legacy.sort();
    real_layered.sort();
    assert_eq!(
        real_legacy, real_layered,
        "非幽灵（真实定义）节点必须两种模式完全一致（实测 30 个），否则就是节点丢失 bug"
    );
    assert_eq!(real_legacy.len(), 30, "真实定义节点数应为 32-2=30");

    // ---- (3) 边数上升的归因必须是"歧义候选被保留"，不是"多造了调用点" ----
    // 调用点身份判据用 [`Built::call_site_set`]（不含 callee 名，理由见该方法文档）。
    let legacy_sites = legacy.call_site_set();
    let layered_sites = layered.call_site_set();
    println!(
        "P2B 覆盖中性：legacy 有边调用点={} 边={} | layered 有边调用点={} 边={}",
        legacy_sites.len(),
        legacy_metrics.edges,
        layered_sites.len(),
        layered_metrics.edges
    );
    // 口径 B 的精确表述是**双向相等**（比原先的 subset 更强）：layered 既不许让原本有边的
    // 调用点掉边，也不许凭空造出一个 legacy 没有的调用点。
    assert_eq!(
        layered_sites,
        legacy_sites,
        "口径 B（覆盖中性，双向）：两种模式的调用点集合必须完全相等，对称差 = {:?}",
        legacy_sites
            .symmetric_difference(&layered_sites)
            .collect::<Vec<_>>()
    );

    // 每调用点的边数：证明"边数 13→20"全部来自同一调用点的候选展开（口径 A）。
    let legacy_per = legacy.call_site_edge_counts();
    let layered_per = layered.call_site_edge_counts();
    println!("P2B 每调用点边数 legacy->layered：");
    for (k, l) in &legacy_per {
        println!(
            "  {:?} : {} -> {}",
            k,
            l,
            layered_per.get(k).copied().unwrap_or(0)
        );
    }
    assert_eq!(
        legacy_per.keys().collect::<Vec<_>>(),
        layered_per.keys().collect::<Vec<_>>(),
        "每调用点边数的键集合必须一致（不许有只在一侧出现的调用点）"
    );
    assert!(
        layered_per.values().all(|n| *n >= 1),
        "任何调用点都不许是零边（口径 B 的覆盖中性）"
    );
    assert!(
        layered_per.values().sum::<usize>() >= legacy_per.values().sum::<usize>(),
        "layered 的总边数必须 >= legacy（候选展开只增不减）"
    );

    // ---- (4) 允许分叉的维度只有这些：连到哪一个同名定义 / 一条边拆成几条 / 层级标量 ----
    // 调用点集合的相等性已在 (3) 里双向锁死；这里把"真正允许不同"的那一维打印出来，
    // 供 README 分叉表逐项归因（callee 身份在 legacy 下与哈希种子有关，故只打印不断言）。
    let legacy_callees = legacy.callee_identities();
    let layered_callees = layered.callee_identities();
    let diff = |a: &[(String, String, usize, bool)], b: &[(String, String, usize, bool)]| -> Vec<String> {
        a.iter().filter(|x| !b.contains(x)).map(|x| format!("{:?}", x)).collect()
    };
    println!("P2B callee 身份对照（legacy={} layered={}）：", legacy_callees.len(), layered_callees.len());
    println!("  仅 legacy 有: {:?}", diff(&legacy_callees, &layered_callees));
    println!("  仅 layered 有: {:?}", diff(&layered_callees, &legacy_callees));
    // 唯一不许分叉的东西：边的总"调用点 × 是否声称已解析"结构里的调用点数（(3) 已锁），
    // 以及 legacy 的幽灵边不许被 layered 悄悄丢掉。
    let legacy_ghosts = legacy_callees.iter().filter(|(_, _, _, r)| !*r).count();
    let layered_ghosts = layered_callees.iter().filter(|(_, _, _, r)| !*r).count();
    println!("P2B 幽灵边（is_resolved=false）legacy={} layered={}", legacy_ghosts, layered_ghosts);
    assert!(
        layered_ghosts + layered_metrics.resolved >= legacy_ghosts + legacy_metrics.resolved,
        "layered 的（幽灵边 + 已解析边）总数不得少于 legacy —— 边覆盖必须中性偏保守"
    );
}

/// 最激进配置（`--resolver=layered --max-candidates=1 --levels=l5`）的新基线。
///
/// P2a 时这一组必须与 legacy 相等；P2b 接线之后它必然分叉，但**必须仍然满足两条不变量**：
///  - `--max-candidates=1` 只截断落盘条数，不得改变"有边的调用点数"（口径 A/B）；
///  - `--levels=l5` 关掉其它层 ⇒ 允许调用点拿不到候选（下沉成幽灵边），但绝不许让
///    原本有边的调用点**没有边**（口径 B 的覆盖中性）。
#[test]
fn p2b_strict_layered_flags_only_truncate_never_drop_sites() {
    let legacy = Built::build("p2b_strict_legacy", RESOLUTION_ROOT);
    let wide = Built::build_with("p2b_strict_wide", RESOLUTION_ROOT, Some(ResolverFlags::LAYERED));
    let strict = Built::build_with(
        "p2b_strict",
        RESOLUTION_ROOT,
        Some(ResolverFlags::LAYERED_STRICT),
    );

    // 调用点身份判据同 [`Built::call_site_set`]（不含 callee 名，理由见该方法文档）。
    let sites = |b: &Built| b.call_site_set();
    let (l, w, s) = (sites(&legacy), sites(&wide), sites(&strict));
    println!(
        "P2B STRICT 对照：legacy 边={}/点={} | layered(8,全层) 边={}/点={} | layered(1,l5) 边={}/点={}",
        legacy.metrics().edges,
        l.len(),
        wide.metrics().edges,
        w.len(),
        strict.metrics().edges,
        s.len()
    );

    // 上限 1：边的**条数**必须少（截断生效），有边的调用点数必须与全候选 layered 相同。
    assert!(
        strict.metrics().edges < wide.metrics().edges,
        "`--max-candidates=1` 必须真的少写边，否则这个开关是假的"
    );
    assert_eq!(
        s, w,
        "`--max-candidates=1` 只许截断条数，不得改变\"哪些调用点有边\"（口径 A/B）"
    );

    // 只开 L5 的极端组合：不得让图变空，也不得写脏新标量。
    assert!(
        strict.metrics().function_nodes > 0 && strict.metrics().edges > 0,
        "`--levels=l5` 不得把图建成空的"
    );
    for edge in strict.graph.graph.edge_references() {
        let rel = edge.weight();
        if rel.resolution_level == 0 {
            assert_eq!(rel.confidence, 0, "level=0 必须 confidence=0");
            assert_eq!(rel.candidate_count, 0, "level=0 必须 candidate_count=0");
            assert!(!rel.is_resolved, "level=0 不得声称已解析");
        } else {
            assert!(rel.candidate_count >= 1, "level>0 必须至少有一个候选");
        }
    }
}

// ============================================================================================
// 守卫 5（Phase P2b 收口，任务交付物 2 第 1 条）：引擎入口**只允许**在 Layered 分支被引用
//
// P2a 的原守卫是"引擎在 src/ 里（排除 `src/codegraph/resolution/`）零引用"。
// P2b 的目的就是把引擎接进产边链路，所以那条断言**永久失效**（by-design）。
// 直接删掉它等于放弃这一整类回归，因此这里改写成一条更强的新不变量：
//
//   **(i) 文件级**：`src/` 里（排除引擎自身 `src/codegraph/resolution/**`）只允许
//        `codegraph/parser.rs` 与 `storage/incremental.rs` 两个接线文件引用引擎入口。
//   **(ii) 函数级**：每一处引用都必须落在"layered 专属函数"或"按 Layered 分叉的函数"里；
//        只建不消费 ctx 的三个缓存建设点（`parse_file` / `_extract_entities_from_file` /
//        `ensure_file_scope_ctx`）**只允许**引用 `build_file_scope_ctx` 这一个入口。
//   **(iii) 反向**：legacy 的产边/callee 解析函数（`_analyze_file_calls_for_petgraph`、
//        `_resolve_callee_function` 的 legacy 尾段、`_build_function_name_index` 等）
//        必须**零引用**引擎入口，且五处 first-wins 的字面特征必须健在。
//   **(iv) 行为侧**（与文本扫描互补，防"改了文本没改行为"）：同一份 fixture 下
//        legacy 的边四标量恒 0、节点身份字段恒 None，而 layered 必须两者都有非零实例。
// ============================================================================================

/// 引擎入口符号（判定"是不是引用了引擎"用；别名 `resolution_emit` / `resolution_caller_node`
/// 也必须一起扫，否则 `use x as y` 会绕过文本扫描）。
const ENGINE_ENTRIES: &[&str] = &[
    "resolve_call_with_config",
    "call_sites_from_symbols",
    "build_resolution_index_from_graph",
    "emit_layered_resolution",
    "layered_caller_node",
    "build_file_scope_ctx",
    "module_separator",
    "resolution_emit(",
    "resolution_caller_node(",
];

/// 允许"引用引擎入口"的函数，以及每个函数允许引用的入口子集。
///
/// `None` 表示该函数允许引用任意引擎入口（它本身就是 layered 链路）。
/// 名单里每一个条目都在 P2b 收口时被人工核对过（见本文件的报告）；
/// 新增接线点必须**同时**扩充这张表，否则本守卫报错 —— 这正是它的价值。
const LAYERED_ONLY_FNS: &[&str] = &[
    // `CodeParser` 的 layered 链路
    "_analyze_file_calls_layered",
    "_analyze_petgraph_call_relations_layered",
    "_layered_callee_first",
    "_layered_caller_node",
    "_emit_layered_resolution",
    // Phase P3 交付物 1：`CodeParser` 刷新链路与 `IncrementalManager` 增量链路**共用**的
    // layered 发射件（原来是两份几乎相同的实现，现在只剩这一份）。
    "emit_layered_call_edges_for_file",
];

/// 只建不消费 `FileScopeCtx` / 只补写节点身份字段的缓存建设点。
///
/// 这三个函数在**两种模式下都跑**（ctx 建设本身与 `--resolver` 无关），
/// 因此只允许引用"建 ctx / 推 module 分隔符"这两个入口，绝不允许引用判定或产边入口。
const CTX_BUILDER_FNS: &[&str] = &[
    "parse_file",
    "_extract_entities_from_file",
    "ensure_file_scope_ctx",
    // Phase P3 交付物 1：两条链路共用的实体提取件（只建 ctx，不产边）。
    "extract_file_entities",
];

/// [`CTX_BUILDER_FNS`] 里允许出现的引擎入口。
const CTX_ALLOWED_ENTRIES: &[&str] = &["build_file_scope_ctx", "module_separator"];

/// 按 `ResolverMode::Layered` 分叉的函数：引擎引用必须出现在
/// `ResolverMode::Layered` 判定**之后**、且仍在同一函数体内（这些函数的 legacy 尾段
/// 由 [`FIRST_WINS_MARKERS`] 单独证明未被偷改）。
const MODE_BRANCH_FNS: &[&str] = &["_analyze_file_calls", "_find_callee_function"];

/// legacy "同名 first-wins"的字面特征（任务红线 1）：任何一处消失 ⇒ 说明有人把
/// legacy 塌缩逻辑删了。
///
/// Phase P3 交付物 1 改写了第一条：`IncrementalManager` 里那份**副本**被删除，
/// 增量链路改为委托 `codegraph::parser::emit_legacy_call_edges_for_file`。
/// 因此这条特征从"incremental.rs 里有 first-wins 代码"换成
/// "incremental.rs 里出现了共享件的调用点"（等价替代：仍然是同一个 legacy 语义，
/// 只是只剩一份实现）；first-wins 代码本身由下面的
/// [`FIRST_WINS_ABSENCE_MARKERS`] 反向钉住"不许再长出第二份"。
///
/// ⚠ **Phase P3 收口 B 再改写一次**（上一条"委托调用点"已经随死链路一起消失了）：
/// 收口 A 判定 `IncrementalManager::refresh_file` 整条产边链路**零调用者**并删掉它，
/// ⇒ "incremental.rs 里有一个共享件调用点"这条正向特征不再成立（收口 A 没跑测试，
/// 所以是本轮才发现）。现在的真实结构是：
///  - `src/storage/incremental.rs` = **纯记账**（MD5/索引 save/load），**不产边**，
///    由 [`FIRST_WINS_ABSENCE_MARKERS`] 里那组 `emit_*` / `extract_file_entities` /
///    `fn refresh_file` 的**零出现**断言钉死；
///  - 唯一的委托调用点搬到了 `src/codegraph/parser.rs`（下面那条**字面完全不变**、
///    只是换了文件名的 `emit_legacy_call_edges_for_file(&self.ts_parser` 特征），
///    legacy 全量档仍由 `_analyze_file_calls_for_petgraph` 独立实现
///    （登记理由见 `parser.rs` 顶部三个共用件的注释，以及
///    `tests/fixtures/resolution/README.md` 的「legacy 抖动实测」一节）。
/// 也就是说"正向证明委托存在 + 反向证明第二份实现不存在"两条都仍然在，只是各自换了位置。
const FIRST_WINS_MARKERS: &[(&str, &str)] = &[
    (
        "src/codegraph/parser.rs",
        "emit_legacy_call_edges_for_file(&self.ts_parser",
    ),
    ("src/codegraph/parser.rs", "global_functions.first().map(|f| f.id)"),
    (
        "src/codegraph/parser.rs",
        "index.entry(function.name.as_str()).or_insert(function)",
    ),
    (
        "src/codegraph/parser.rs",
        "if func.namespace.contains(prefix) || func.name == method_name",
    ),
    ("src/codegraph/parser.rs", "for function in current_functions"),
];

/// Phase P3 交付物 1 的**收敛证明**：这些 first-wins / 产边 / 引擎特征在
/// `src/storage/incremental.rs` 里的出现次数必须恒为 0 —— 该文件只剩记账。
const FIRST_WINS_ABSENCE_MARKERS: &[(&str, &str)] = &[
    ("src/storage/incremental.rs", "global_functions.first().map(|f| f.id)"),
    ("src/storage/incremental.rs", "fn _find_callee_function"),
    ("src/storage/incremental.rs", "fn _extract_entities_from_file_impl"),
    (
        "src/storage/incremental.rs",
        "build_resolution_index_from_graph",
    ),
    ("src/storage/incremental.rs", "resolve_call_with_config"),
    // ---- Phase P3 收口 B：记账版 IncrementalManager 不得再持有任何产边能力 ----
    ("src/storage/incremental.rs", "fn refresh_file"),
    (
        "src/storage/incremental.rs",
        "emit_legacy_call_edges_for_file",
    ),
    (
        "src/storage/incremental.rs",
        "emit_layered_call_edges_for_file",
    ),
    ("src/storage/incremental.rs", "extract_file_entities"),
];

/// 计算每个 `fn` 的行区间 `(名字, 起始行, 结束行)`（0-based，含首尾）。
///
/// 只需支撑文本级守卫判定，因此用"大括号配平"的近似实现：跳过 `//` 注释行，
/// 统计每行的 `{`/`}`。字符串字面量里的花括号会带来误差，但接线代码里没有这种情况；
/// 万一哪天因为误差误判，本守卫会**报错**而不是静默放过 —— 这正是想要的失败方向。
fn function_ranges(text: &str) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut depth = 0usize;
    // 栈里存 `(函数名, 声明时的 depth, 声明行)`
    let mut stack: Vec<(String, usize, usize)> = Vec::new();
    let mut pending: Option<(String, usize)> = None;

    for (idx, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if let Some(name) = parse_fn_name(trimmed) {
            pending = Some((name, depth));
        }
        let opens = raw.matches('{').count();
        let closes = raw.matches('}').count();
        if let Some((name, base)) = pending.take() {
            if opens > 0 {
                stack.push((name, base, idx));
            }
        }
        depth = depth + opens.saturating_sub(closes);
        while let Some((name, base, start)) = stack.last().cloned() {
            if depth <= base {
                stack.pop();
                out.push((name, start, idx));
            } else {
                break;
            }
        }
    }
    out
}

/// 从一行 `fn` 声明里取函数名（只处理 `pub fn` / `fn` / `async fn` 三种形态）。
fn parse_fn_name(trimmed: &str) -> Option<String> {
    for prefix in ["pub const fn ", "pub async fn ", "const fn ", "async fn ", "pub fn ", "fn "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                return None;
            }
            return Some(name);
        }
    }
    None
}

/// 找到 `line`（0-based）所在的**最内层**函数名。
fn enclosing_function(ranges: &[(String, usize, usize)], line: usize) -> Option<&str> {
    let mut best: Option<(&str, usize)> = None;
    for (name, start, end) in ranges {
        if *start <= line && line <= *end {
            let span = end.saturating_sub(*start);
            if best.is_none_or(|(_, b)| span < b) {
                best = Some((name.as_str(), span));
            }
        }
    }
    best.map(|(name, _)| name)
}

/// 判定一行的引用是否允许（见文件头守卫 5 的 (i)(ii)(iii)）。
///
/// `file` 是相对 manifest 的路径（已把 `\` 归一化为 `/`）。它在这里**复查**一遍 (i)
/// 的文件级不变量：调用方虽然已经过滤过非接线文件，但把规则同时落在判定函数里，
/// 将来有人重构调用方时不会悄悄放开"任何文件都能引用引擎"。
fn classify_reference(file: &str, func: Option<&str>, symbol: &str) -> bool {
    const WIRING: &[&str] = &["src/codegraph/parser.rs", "src/storage/incremental.rs"];
    if !WIRING.contains(&file) {
        return false;
    }
    let Some(func) = func else {
        // 不在任何函数体内 ⇒ `use` 语句 / 模块级常量，接线必需，放行。
        return true;
    };
    if LAYERED_ONLY_FNS.iter().any(|f| func.ends_with(f)) {
        return true;
    }
    if CTX_BUILDER_FNS.contains(&func) {
        // 只建不消费：这三个函数在两种模式下都会跑，因此只允许引用"建 ctx / 推 module 分隔符"
        // 这两个纯建设入口（[`CTX_ALLOWED_ENTRIES`]），绝不允许引用判定或产边入口 ——
        // 否则默认 legacy 路径就会被"顺手"接进分层引擎，红线 1 立刻失效。
        return CTX_ALLOWED_ENTRIES.contains(&symbol);
    }
    if MODE_BRANCH_FNS.contains(&func) {
        return true;
    }
    false
}

#[test]
fn p2b_engine_entries_are_referenced_only_from_layered_branches() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_root = manifest.join("src");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_rs_files(&src_root, &mut files);
    files.sort();

    let wiring_files = ["src/codegraph/parser.rs", "src/storage/incremental.rs"];
    let mut offenders: Vec<String> = Vec::new();
    let mut referenced_symbols_in_wiring: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut per_file_counts: Vec<(String, usize)> = Vec::new();

    for path in &files {
        let rel = path
            .strip_prefix(&manifest)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        // 引擎本体自己不算"接线"。
        if rel.contains("src/codegraph/resolution/") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let ranges = function_ranges(&text);
        let outside_wiring = !wiring_files.contains(&rel.as_str());
        let mut hits = 0usize;

        // 处于多行 `use ... { ... };` 语句里时，行号集合（放行：那是导入，不是调用）
        let mut in_use_block = false;
        for (idx, raw) in text.lines().enumerate() {
            let trimmed = raw.trim_start();
            if trimmed.starts_with("use ") {
                in_use_block = !trimmed.contains(';');
            }
            let is_comment = trimmed.starts_with("//")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("*");
            let is_use = in_use_block || trimmed.starts_with("use ");
            if is_comment {
                continue;
            }
            for symbol in ENGINE_ENTRIES {
                if !raw.contains(symbol) {
                    continue;
                }
                hits += 1;
                if is_use {
                    // 导入语句：只要求它必须来自接线文件（(i) 文件级）。
                    if outside_wiring {
                        offenders.push(format!("{rel}:{} 在非接线文件里导入了引擎入口 {symbol}", idx + 1));
                    } else {
                        referenced_symbols_in_wiring.insert((*symbol).to_string());
                    }
                    continue;
                }
                if outside_wiring {
                    offenders.push(format!("{rel}:{} 在非接线文件里调用了引擎入口 {symbol}", idx + 1));
                    continue;
                }
                let func = enclosing_function(&ranges, idx);
                if !classify_reference(&rel, func, symbol) {
                    offenders.push(format!(
                        "{rel}:{} 在函数 `{}?` 里引用了引擎入口 {symbol}（该函数不在 layered 专属 /\n  ctx 建设 / Mode::Layered 分叉三类白名单里）",
                        idx + 1,
                        func.unwrap_or("<none>")
                    ));
                } else {
                    referenced_symbols_in_wiring.insert((*symbol).to_string());
                }
            }
            if trimmed == "};" && in_use_block {
                in_use_block = false;
            }
        }
        if hits > 0 {
            per_file_counts.push((rel, hits));
        }
    }

    // ---- 先证明"确实引用了"：否则本守卫会在有人把接线整个删掉时假绿 ----
    assert!(
        !referenced_symbols_in_wiring.is_empty(),
        "P2b 已接线：引擎入口必须在接线文件里被引用（这张表空了说明接线被整体删除，本守卫应当改写）"
    );
    for must in [
        "resolve_call_with_config",
        "call_sites_from_symbols",
        "build_resolution_index_from_graph",
    ] {
        assert!(
            referenced_symbols_in_wiring.contains(must),
            "引擎入口 `{must}` 必须仍是被引用状态（P2b 接线证据）"
        );
    }

    // ---- 反向不变量：legacy 产边/callee 解析函数体里**一次**引擎入口都不许出现 ----
    // （`_analyze_file_calls` / `_find_callee_function` 的入口分叉在
    //  [`MODE_BRANCH_FNS`] 里被单独允许，这里检查的是"纯 legacy 函数"）。
    let legacy_only_fns = [
        "_analyze_file_calls_for_petgraph",
        "_analyze_call_relations",
        "_analyze_file_call_relations",
        "_build_function_name_index",
        "_fallback_call_analysis",
        "_resolve_callee_function",
        "_resolve_qualified_function_name",
        "_create_unresolved_call_relation",
    ];
    let mut legacy_violations: Vec<String> = Vec::new();
    for rel in wiring_files {
        let text = std::fs::read_to_string(manifest.join(&rel)).unwrap_or_default();
        let ranges = function_ranges(&text);
        for (name, start, end) in &ranges {
            if !legacy_only_fns.contains(&name.as_str()) {
                continue;
            }
            for (idx, raw) in text.lines().enumerate().take(*end).skip(*start) {
                let trimmed = raw.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                // `_resolve_callee_function` / `_resolve_qualified_function_name` 允许
                // 在**入口分叉处**调用本文件的 `_layered_callee_first` 薄调用；
                // 其余引擎入口一律禁止。
                for symbol in ENGINE_ENTRIES {
                    if raw.contains(symbol) {
                        legacy_violations.push(format!(
                            "{rel}:{} legacy 专属函数 `{name}` 引用了引擎入口 {symbol}",
                            idx + 1,
                        ));
                    }
                }
            }
        }
    }
    assert!(
        legacy_violations.is_empty(),
        "legacy 分支必须零引用分层引擎：\n{}",
        legacy_violations.join("\n")
    );

    // ---- 五处 first-wins 的字面特征必须健在（红线 1）----
    for (rel, marker) in FIRST_WINS_MARKERS {
        let text = std::fs::read_to_string(manifest.join(rel)).unwrap_or_default();
        assert!(
            text.contains(marker),
            "红线 1 被破坏：{rel} 里找不到 first-wins 特征 `{marker}`"
        );
    }

    // ---- Phase P3 交付物 1：收敛证明 ----
    // 增量链路里不许再存在第二份 first-wins / 第二份引擎调用代码：
    // 一旦有人把共享件复制回去，本断言立刻失败（这正是 P0 记录在案的缺陷复发方式）。
    for (rel, marker) in FIRST_WINS_ABSENCE_MARKERS {
        let text = std::fs::read_to_string(manifest.join(rel)).unwrap_or_default();
        // 只看代码行（注释里提到这些名字是允许的，而且必须允许 —— 说明文档要写清委托关系）。
        let hits = text
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!") || t.starts_with('*'))
                    && line.contains(marker)
            })
            .count();
        assert_eq!(
            hits, 0,
            "P3 收敛被破坏：{rel} 的代码行里又出现了第二份实现特征 `{marker}`（应当委托 \
             `codegraph::parser` 的共享件），命中 {hits} 处"
        );
    }
    // 共享件必须**确实存在**于 parser.rs（防止上面两条断言同时被"删掉委托"绕过）。
    {
        let text = std::fs::read_to_string(manifest.join("src/codegraph/parser.rs")).unwrap_or_default();
        for shared in [
            "pub(crate) fn extract_file_entities",
            "pub(crate) fn emit_legacy_call_edges_for_file",
            "pub(crate) fn emit_layered_call_edges_for_file",
        ] {
            assert!(
                text.contains(shared),
                "P3 唯一真源必须存在：src/codegraph/parser.rs 里找不到 `{shared}`"
            );
        }
        // `IncrementalManager` 的定位（Phase P3 收口 A/B 之后）：**纯记账**，
        // 既不再自己产实体/产边，也不"委托"产边 —— 因为它那条产边链路本来就没有调用者，
        // 真实的刷新/增量链路一直在 `CodeParser` 里。
        // 改造前这里断言的是 `inc.contains("extract_file_entities(")` +
        // `inc.contains("emit_layered_call_edges_for_file(")`（"增量链路必须委托共享件"），
        // 收口 A 删掉死链路之后该断言不再成立（且收口 A 没跑测试 ⇒ 本轮才暴露），
        // 于是把"委托确实存在"的证明搬到**真正的委托者** `CodeParser` 上，
        // 同时把 `IncrementalManager` 钉成"只剩记账面"。
        let inc = std::fs::read_to_string(manifest.join("src/storage/incremental.rs")).unwrap_or_default();
        for bookkeeping in [
            "fn needs_update",
            "fn compute_file_md5",
            "fn save_state",
            "fn load_state",
        ] {
            assert!(
                inc.contains(bookkeeping),
                "记账面必须留在 src/storage/incremental.rs：找不到 `{bookkeeping}`（repository.rs 依赖它）"
            );
        }
        for delegated in [
            "extract_file_entities(\n            &self.ts_parser,",
            "emit_legacy_call_edges_for_file(&self.ts_parser",
            "emit_layered_call_edges_for_file(",
        ] {
            assert!(
                text.contains(delegated),
                "P3 唯一真源的**委托点**必须在 src/codegraph/parser.rs：找不到 `{delegated}`"
            );
        }
    }

    // ---- 汇总 ----
    println!(
        "P2B 引擎引用白名单核对：接线文件引用入口 {} 种；引用点分布 {:?}",
        referenced_symbols_in_wiring.len(),
        per_file_counts
    );
    assert!(
        offenders.is_empty(),
        "引擎入口只能出现在 layered 分支/接线白名单函数里，违规：\n{}",
        offenders.join("\n")
    );
}

/// 行为侧证据（与文本扫描互补）：legacy 零新标量/零身份字段，layered 两者都有。
#[test]
fn p2b_layered_wiring_is_observable_in_graph_but_legacy_is_not() {
    let legacy = Built::build("p2b_behavior_legacy", RESOLUTION_ROOT);
    let layered = Built::build_with("p2b_behavior_layered", RESOLUTION_ROOT, Some(ResolverFlags::LAYERED));

    let legacy_edges_with_level = legacy
        .graph
        .graph
        .edge_references()
        .filter(|e| e.weight().resolution_level > 0)
        .count();
    let layered_edges_with_level = layered
        .graph
        .graph
        .edge_references()
        .filter(|e| e.weight().resolution_level > 0)
        .count();
    let legacy_nodes_with_identity = legacy
        .all_functions()
        .into_iter()
        .filter(|f| f.module_path.is_some() || f.qualified_name.is_some())
        .count();
    let layered_nodes_with_identity = layered
        .all_functions()
        .into_iter()
        .filter(|f| f.module_path.is_some() || f.qualified_name.is_some())
        .count();
    println!(
        "P2B 接线行为证据：legacy level>0 边={legacy_edges_with_level} 身份节点={legacy_nodes_with_identity}\n                      layered level>0 边={layered_edges_with_level} 身份节点={layered_nodes_with_identity}"
    );
    assert_eq!(
        legacy_edges_with_level, 0,
        "legacy 的边必须一条 `resolution_level>0` 都没有（引擎零消费）"
    );
    assert_eq!(legacy_nodes_with_identity, 0, "legacy 不得写节点身份字段");
    assert!(
        layered_edges_with_level > 0,
        "layered 必须真的写出了带层级的边，否则接线是假的"
    );
    assert!(
        layered_nodes_with_identity > 0,
        "layered 必须真的写出了节点身份字段，否则交付物 1 的接线是假的"
    );
    // layered 的多候选必须体现为 `candidate_count >= 2` 的边（歧义没被塌缩）。
    let multi = layered
        .graph
        .graph
        .edge_references()
        .filter(|e| e.weight().candidate_count >= 2)
        .count();
    assert!(
        multi > 0,
        "口径 A：歧义调用点必须保留多候选（candidate_count>=2 的边数为 0 ⇒ 又被塌缩了）"
    );
    println!("P2B candidate_count>=2 的边 = {multi} 条（口径 A 未塌缩证据）");
}

/// 递归收集 `src/**/*.rs`。
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
