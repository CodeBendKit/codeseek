//! Phase P2b 收口交付物 3：**legacy vs layered 同口径精度对照**（实测输出，不是手抄数字）。
//!
//! ## 本文件提供什么
//! 同一份源码（`tests/fixtures/resolution/` 的聚合口径 + 7 个消歧场景，`tests/test_repos/`
//! 的 4 个整仓小项目），在 `--resolver=legacy`（默认）与 `--resolver=layered`
//! （`--max-candidates=8`、全层）两种配置下各建一次图，输出**同口径**的指标：
//!
//! ```text
//! 函数节点 / 调用边 / resolved / ambiguous边 / unresolved / 幽灵边 / 幽灵节点
//!   / 跨文件边 / 同名歧义调用点 / 有边调用点
//! ```
//!
//! 外加任务书要的最后一项"**被修正的错指条数**"，拆成两个可复现的子项：
//! - `fixed_ghost_to_real`：某调用点在 legacy 下**只有**未解析（连幽灵节点）的边，
//!   在 layered 下拿到**已解析**的边 ⇒ 真定义被找到了。与哈希种子无关，可精确断言。
//! - `redirected_off_legacy_pick`：某调用点 layered 的唯一解析目标**不在** legacy 连到的
//!   目标集合里 ⇒ legacy 那一枪打偏了。只断言下界 `>= 1`，精确值打印 ——
//!   因为 legacy 的 first-wins 取决于 `HashMap` 每进程随机种子（实测见
//!   `tests/fixtures/resolution/README.md`），把它当分母做精确断言会产出一条永不可能稳定的测试。
//!
//! ## 为什么这里还要再断言一次 legacy 侧数字
//! 红线 1 要求"默认 legacy 行为与 P2a（`8e62033`）逐字节等价"。
//! `tests/test_p1_edges_unchanged.rs` 的守卫表已覆盖 7 个场景与 4 个整仓的**聚合/逐仓**口径，
//! 本文件把同一批数字在**与 layered 并排**的位置再核一次，并额外核 `ambiguous边=0`
//! （legacy 从不写 `candidate_count`，若哪天 legacy 开始写分层标量，这里会先响）。
//! 核对比照表只覆盖"与哈希种子无关"的字段；`cross_file_edges` 与两个派生计数只打印。
//!
//! ## 怎么看表
//! `cargo test --test test_precision_report -- --nocapture` —— 输出即 markdown 表，
//! 可直接抄进 `tests/fixtures/resolution/README.md`。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use codeseek::cli::args::ResolverMode;
use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::types::{FunctionInfo, PetCodeGraph};

const RESOLUTION_ROOT: &str = "tests/fixtures/resolution";
const REPOS_ROOT: &str = "tests/test_repos";

/// 一行对照数据。
#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    function_nodes: usize,
    edges: usize,
    /// `is_resolved = true` 的边数
    resolved: usize,
    /// layered 口径：`candidate_count >= 2` 的边数（legacy 必须恒 0）
    ambiguous_edges: usize,
    /// `is_resolved = false` 的边数
    unresolved: usize,
    /// callee 是 `namespace == "unresolved"` 幽灵节点的边数（= 幽灵边）
    ghost_edges: usize,
    /// 图中幽灵节点个数
    ghost_nodes: usize,
    /// caller 与 callee 不同文件的边数（**种子相关，只打印**）
    cross_file_edges: usize,
    /// 守卫表口径：callee 裸名在图索引里有 >1 个候选的边数
    ambiguous_call_sites: usize,
    /// 有边的调用点数（`(caller 相对文件, 调用行)` 去重）—— 口径 B
    call_sites: usize,
}

struct Built {
    graph: PetCodeGraph,
    project_dir: PathBuf,
}

impl Built {
    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.project_dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn build(rel_src: &str, label: &str, layered: bool) -> Self {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_src);
        assert!(src.is_dir(), "fixture 目录不存在: {}", src.display());
        let temp = tempfile::TempDir::new().expect("创建临时目录失败");
        let project_dir = temp
            .path()
            .join(format!("p2bprecision_{}_{}", label, std::process::id()));
        copy_tree(&src, &project_dir);

        let mut parser = CodeParser::new();
        // 一律 force_reindex：本表要"从零解析"的纯解析结果，不接受任何缓存串味。
        parser.set_force_reindex(true);
        parser.set_resolver_mode(if layered {
            ResolverMode::Layered
        } else {
            ResolverMode::Legacy
        });
        parser.set_max_candidates(8);
        let graph = parser
            .build_petgraph_code_graph(&project_dir)
            .unwrap_or_else(|e| panic!("[{label}/{}] 建图失败: {e}", if layered { "layered" } else { "legacy" }));

        // 清掉本轮写进 ~/.codeseek/projects 的缓存（哈希 + 图），避免别的用例读到脏缓存。
        let id_by_md5 = format!("{:x}", md5::compute(project_dir.to_string_lossy().as_bytes()));
        cleanup_codeseek_project(&id_by_md5);
        if let Some(name) = project_dir.file_name() {
            cleanup_codeseek_project(&name.to_string_lossy());
        }
        // temp 正常回收（原 `p2b_probe.rs` 里的 `std::mem::forget(temp)` 是临时探针时代的泄漏）。
        drop(temp);

        Self { graph, project_dir }
    }

    /// Phase P3 交付物 2：图里所有幽灵节点（`namespace == "unresolved"`）的**裸名集合**。
    ///
    /// 用途：证明两档用的是**同一条幽灵身份规则**（同裸名复用一个节点）。
    /// 如果 layered 的幽灵名集合里出现 legacy 没有的名字，那多出来的节点
    /// 一定是"legacy 把这个名字**假解析**到了某个同名真定义上、根本没造幽灵"的场景 ——
    /// 下面 `resolved_edges_to_names` 就是用来核这件事的。
    fn ghost_names(&self) -> BTreeSet<String> {
        self.graph
            .get_all_functions()
            .into_iter()
            .filter(|f| f.namespace == "unresolved")
            .map(|f| f.name.clone())
            .collect()
    }

    /// Phase P3 交付物 2：`is_resolved == true` 且 callee 裸名落在 `names` 里的边数。
    ///
    /// 给 `names = layered 的幽灵名集合 - legacy 的幽灵名集合` 时，它就是
    /// "legacy 把这些本该未解析的调用点连到了同名真定义"的条数（假解析/误连）。
    fn resolved_edges_to_names(&self, names: &BTreeSet<String>) -> usize {
        self.graph
            .graph
            .edge_references()
            .filter(|e| {
                let r = e.weight();
                r.is_resolved && names.contains(&r.callee_name)
            })
            .count()
    }

    /// 指标口径与 `tests/test_p1_edges_unchanged.rs::Metrics` 对齐（另加 3 个本表专有字段）。
    fn metrics(&self) -> Metrics {
        let functions: Vec<&FunctionInfo> = self.graph.get_all_functions();
        let rels: Vec<_> = self
            .graph
            .graph
            .edge_references()
            .map(|e| e.weight().clone())
            .collect();
        let node = |id: &uuid::Uuid| self.graph.get_function_by_id(id);
        let ghost_ids: BTreeSet<uuid::Uuid> = functions
            .iter()
            .filter(|f| f.namespace == "unresolved")
            .map(|f| f.id)
            .collect();

        let mut sites: BTreeSet<(String, usize)> = BTreeSet::new();
        let mut cross = 0usize;
        let mut ghost_edges = 0usize;
        for r in &rels {
            if let (Some(a), Some(b)) = (node(&r.caller_id), node(&r.callee_id)) {
                if a.file_path != b.file_path {
                    cross += 1;
                }
                sites.insert((self.rel(&a.file_path), r.line_number));
            }
            if ghost_ids.contains(&r.callee_id) {
                ghost_edges += 1;
            }
        }
        Metrics {
            function_nodes: functions.len(),
            edges: rels.len(),
            resolved: rels.iter().filter(|r| r.is_resolved).count(),
            ambiguous_edges: rels.iter().filter(|r| r.candidate_count >= 2).count(),
            unresolved: rels.iter().filter(|r| !r.is_resolved).count(),
            ghost_edges,
            ghost_nodes: ghost_ids.len(),
            cross_file_edges: cross,
            ambiguous_call_sites: rels
                .iter()
                .filter(|r| {
                    self.graph
                        .function_names
                        .get(r.callee_name.as_str())
                        .map(|v| v.len())
                        .unwrap_or(0)
                        > 1
                })
                .count(),
            call_sites: sites.len(),
        }
    }

    /// `调用点 (caller 相对文件, 调用行) -> [(callee 相对文件, callee 名, callee 行, is_resolved)]`
    /// —— 跨模式对齐调用点的键**故意不含 callee**（两模式对 callee 名的填法不同，见
    /// `tests/test_p1_edges_unchanged.rs::Built::call_site_set` 的文档）。
    fn edges_by_site(&self) -> BTreeMap<(String, usize), Vec<(String, String, usize, bool)>> {
        let mut m: BTreeMap<(String, usize), Vec<(String, String, usize, bool)>> = BTreeMap::new();
        for e in self.graph.graph.edge_references() {
            let r = e.weight();
            let (Some(caller), Some(callee)) = (
                self.graph.get_function_by_id(&r.caller_id),
                self.graph.get_function_by_id(&r.callee_id),
            ) else {
                continue;
            };
            m.entry((self.rel(&caller.file_path), r.line_number))
                .or_default()
                .push((
                    self.rel(&callee.file_path),
                    callee.name.clone(),
                    callee.line_start,
                    r.is_resolved,
                ));
        }
        for v in m.values_mut() {
            v.sort();
        }
        m
    }

    /// 与 legacy 对照得出"被修正的错指"两项指标 + 逐条明细。
    fn misdirection(&self, legacy: &Built) -> (usize, usize, Vec<String>) {
        let l_sites = legacy.edges_by_site();
        let g_sites = self.edges_by_site();
        let mut fixed_ghost = 0usize;
        let mut redirected = 0usize;
        let mut rows = Vec::new();
        for (site, glayer) in g_sites {
            let lgraph = l_sites.get(&site).cloned().unwrap_or_default();
            let legacy_all_unresolved = !lgraph.is_empty() && lgraph.iter().all(|(_, _, _, r)| !*r);
            let layered_any_resolved = glayer.iter().any(|(_, _, _, r)| *r);
            if legacy_all_unresolved && layered_any_resolved {
                fixed_ghost += 1;
                rows.push(format!(
                    "{}:{} 幽灵→真定义 legacy={:?} layered={:?}",
                    site.0, site.1, lgraph, glayer
                ));
                continue;
            }
            let resolved_targets: Vec<_> = glayer
                .iter()
                .filter(|(_, _, _, r)| *r)
                .map(|(f, n, l, _)| (f.clone(), n.clone(), *l))
                .collect();
            if resolved_targets.len() == 1 {
                let legacy_set: BTreeSet<(String, String, usize)> = lgraph
                    .iter()
                    .map(|(f, n, l, _)| (f.clone(), n.clone(), *l))
                    .collect();
                if !legacy_set.contains(&resolved_targets[0]) {
                    redirected += 1;
                    rows.push(format!(
                        "{}:{} legacy={:?} layered唯一={:?} (level/conf/rank/n)",
                        site.0, site.1, lgraph, resolved_targets[0]
                    ));
                }
            }
        }
        (fixed_ghost, redirected, rows)
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

/// legacy 侧基线，抄自 `tests/test_p1_edges_unchanged.rs` 的 `EXPECTED_AGGREGATE` /
/// `PER_CASE_BASELINE`（P0/P1/P2a 实测值）。只列**与哈希种子无关**的字段；
/// `cross_file_edges` / `ghost_edges` / `call_sites` 不在基线里（前一项种子相关，
/// 后两项是本表新增的派生口径，无历史基线可抄）。
const LEGACY_BASELINE: &[(&str, [usize; 6])] = &[
    // (名称, [函数节点, 边, resolved, unresolved, 同名歧义调用点, 幽灵节点])
    ("resolution(聚合)", [32, 13, 11, 2, 9, 2]),
    ("cross_file_same_name", [5, 3, 3, 0, 3, 0]),
    ("python_import_alias", [4, 1, 0, 1, 0, 1]),
    ("rust_qualified_path", [4, 2, 2, 0, 2, 0]),
    ("python_dotted_qualified", [3, 1, 1, 0, 1, 0]),
    ("method_receiver", [4, 1, 1, 0, 1, 0]),
    ("nested_function_caller", [5, 3, 2, 1, 0, 1]),
    ("ambiguous_trait_impl", [7, 2, 2, 0, 2, 0]),
    ("simple_rust_project", [46, 44, 34, 10, 0, 10]),
    ("simple_python_project", [45, 48, 32, 16, 0, 16]),
    ("simple_ts_project", [92, 101, 70, 31, 4, 31]),
    ("simple_js_project", [1577, 746, 546, 200, 187, 200]),
];

fn legacy_baseline_diff(name: &str, m: &Metrics) -> Option<String> {
    let (_, expected) = LEGACY_BASELINE.iter().find(|(n, _)| *n == name)?;
    let actual = [
        m.function_nodes,
        m.edges,
        m.resolved,
        m.unresolved,
        m.ambiguous_call_sites,
        m.ghost_nodes,
    ];
    let fields = [
        "function_nodes",
        "edges",
        "resolved",
        "unresolved",
        "ambiguous_call_sites",
        "ghost_nodes",
    ];
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        if a != e {
            return Some(format!(
                "[{name}] legacy 字段 `{}` 变了：P2a 基线={e} 本次={a}（整表 expected={expected:?} actual={actual:?}）",
                fields[i]
            ));
        }
    }
    if m.ambiguous_edges != 0 {
        return Some(format!(
            "[{name}] legacy 写出了 `candidate_count>=2` 的边（{} 条）—— legacy 不消费分层引擎，这四个新标量必须恒 0",
            m.ambiguous_edges
        ));
    }
    None
}

/// 交付物 3：打印 legacy vs layered 同口径对照表，并钉死"legacy 侧不许变"。
#[test]
fn p2b_precision_report_legacy_vs_layered() {
    let cases: Vec<(&str, String)> = vec![
        ("resolution(聚合)", RESOLUTION_ROOT.to_string()),
        (
            "cross_file_same_name",
            format!("{RESOLUTION_ROOT}/cross_file_same_name"),
        ),
        (
            "python_import_alias",
            format!("{RESOLUTION_ROOT}/python_import_alias"),
        ),
        (
            "rust_qualified_path",
            format!("{RESOLUTION_ROOT}/rust_qualified_path"),
        ),
        (
            "python_dotted_qualified",
            format!("{RESOLUTION_ROOT}/python_dotted_qualified"),
        ),
        (
            "method_receiver",
            format!("{RESOLUTION_ROOT}/method_receiver"),
        ),
        (
            "nested_function_caller",
            format!("{RESOLUTION_ROOT}/nested_function_caller"),
        ),
        (
            "ambiguous_trait_impl",
            format!("{RESOLUTION_ROOT}/ambiguous_trait_impl"),
        ),
        (
            "simple_rust_project",
            format!("{REPOS_ROOT}/simple_rust_project"),
        ),
        (
            "simple_python_project",
            format!("{REPOS_ROOT}/simple_python_project"),
        ),
        (
            "simple_ts_project",
            format!("{REPOS_ROOT}/simple_ts_project"),
        ),
        (
            "simple_js_project",
            format!("{REPOS_ROOT}/simple_js_project"),
        ),
    ];

    println!("===== Phase P2b 精度对照（全部实测；layered = --max-candidates=8 全层）=====");
    println!("| case | mode | 函数节点 | 边 | resolved | ambiguous边 | unresolved | 幽灵边 | 幽灵节点 | 跨文件 | 同名歧义调用点 | 有边调用点 |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");

    let mut legacy_failures: Vec<String> = Vec::new();
    let mut coverage_breaks: Vec<String> = Vec::new();
    // Phase P3 交付物 2：layered 幽灵节点数 > legacy 的 case（必须全部能被"legacy 假解析"解释）。
    let mut ghost_amplification: Vec<String> = Vec::new();
    let mut ghost_unexplained: Vec<String> = Vec::new();
    let mut total_fixed_ghost = 0usize;
    let mut total_redirected = 0usize;
    let mut detail_rows: Vec<String> = Vec::new();

    for (name, rel) in &cases {
        let legacy = Built::build(rel, name, false);
        let layered = Built::build(rel, name, true);
        let (lm, gm) = (legacy.metrics(), layered.metrics());
        for (tag, m) in [("legacy", &lm), ("layered", &gm)] {
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                name,
                tag,
                m.function_nodes,
                m.edges,
                m.resolved,
                m.ambiguous_edges,
                m.unresolved,
                m.ghost_edges,
                m.ghost_nodes,
                m.cross_file_edges,
                m.ambiguous_call_sites,
                m.call_sites
            );
        }

        // 覆盖中性（口径 B）：两模式的"有边调用点"必须相等。
        if gm.call_sites != lm.call_sites {
            coverage_breaks.push(format!(
                "[{name}] layered 有边调用点 {} != legacy {}（边 {}→{}）",
                gm.call_sites, lm.call_sites, lm.edges, gm.edges
            ));
        }

        // ---- Phase P3 交付物 2：幽灵合并粒度必须与 legacy 一致（同裸名一个节点）----
        // (i) 硬不变量：两档的幽灵**节点数**都必须等于其幽灵**裸名集合大小**
        //     ⇒ 粒度就是"同裸名"，彻底没有"同文件+同调用行"那种按行放大的效应。
        // (ii) layered 相对 legacy 多出来的每一个幽灵名，都必须能被
        //     "legacy 把同名调用点**假解析**到某个真定义（因此它压根没造幽灵）"解释；
        //     解释不了的多出量就是残留的放大 ⇒ 直接失败。
        let l_ghost = legacy.ghost_names();
        let g_ghost = layered.ghost_names();
        assert_eq!(
            gm.ghost_nodes,
            g_ghost.len(),
            "[{name}] layered 幽灵合并粒度未统一：幽灵节点 {} != 不同幽灵裸名 {}（同一裸名被拆成多个节点）",
            gm.ghost_nodes,
            g_ghost.len()
        );
        assert_eq!(
            lm.ghost_nodes,
            l_ghost.len(),
            "[{name}] legacy 幽灵合并粒度口径变了：幽灵节点 {} != 不同幽灵裸名 {}",
            lm.ghost_nodes,
            l_ghost.len()
        );
        let only_layered: BTreeSet<String> = g_ghost.difference(&l_ghost).cloned().collect();
        let only_legacy: BTreeSet<String> = l_ghost.difference(&g_ghost).cloned().collect();
        let legacy_fake_on_extra = legacy.resolved_edges_to_names(&only_layered);
        // 集合恒等式：node 数 == 名字数（上面已各自 assert_eq），于是
        //   layered_nodes - legacy_nodes == |only_layered| - |only_legacy|。
        // 这条等式一旦不成立，就说明幽灵节点数与幽灵裸名数脱了钩（残留的按行放大源）。
        assert_eq!(
            gm.ghost_nodes as isize - lm.ghost_nodes as isize,
            only_layered.len() as isize - only_legacy.len() as isize,
            "[{name}] 幽灵节点差与名字集合差不一致 ⇒ 存在未归并的幽灵（合并粒度仍有放大源）",
        );
        println!(
            "  · [幽灵粒度] {name}: legacy 节点={} 名字={} | layered 节点={} 名字={} | 仅layered幽灵名={:?} 仅legacy幽灵名={:?} | legacy对这些名字假解析={}条",
            lm.ghost_nodes,
            l_ghost.len(),
            gm.ghost_nodes,
            g_ghost.len(),
            only_layered,
            only_legacy,
            legacy_fake_on_extra
        );
        if gm.ghost_nodes > lm.ghost_nodes {
            ghost_amplification.push(format!(
                "[{name}] layered 幽灵节点({}) 比 legacy({}) 多 {} 个：仅layered幽灵名 {:?}",
                gm.ghost_nodes,
                lm.ghost_nodes,
                gm.ghost_nodes - lm.ghost_nodes,
                only_layered
            ));
        }
        // **交付物 2 的硬不变量（合并粒度，而不是"谁更诚实"）**：
        //   layered 多出来的每一个幽灵裸名，legacy 侧必须**完全没有该裸名的任何边**
        //   （既没有幽灵边、也没有假解析边）—— 也就是说它多出来只能来自
        //   "layered 多提取了一个调用点"（`call_sites_from_symbols_with_source` 比
        //   legacy 的 `FunctionCall` 符号循环看得全），**绝不可能**来自合并粒度。
        //   反过来 legacy 独有的幽灵名也一样。这一条比 `<=` 更强：它把"多出来的每一个节点"
        //   的成因钉死，而不只是要求一个总数不等式。
        for (side, built, names) in [
            ("layered", &legacy, &only_layered),
            ("legacy", &layered, &only_legacy),
        ] {
            for n in names {
                let any_edge = built
                    .graph
                    .graph
                    .edge_references()
                    .any(|e| e.weight().callee_name == *n);
                if any_edge {
                    ghost_unexplained.push(format!(
                        "[{name}] {side} 独有幽灵名 `{n}` 在对方图里却有边 ⇒ 不是调用点覆盖差异，是合并粒度残留"
                    ));
                }
            }
        }
        assert!(
            gm.edges >= lm.edges,
            "[{name}] layered 边数 {} < legacy {}（候选展开只增不减）",
            gm.edges,
            lm.edges
        );

        if let Some(msg) = legacy_baseline_diff(name, &lm) {
            legacy_failures.push(msg);
        }

        let (fixed, redirected, rows) = layered.misdirection(&legacy);
        total_fixed_ghost += fixed;
        total_redirected += redirected;
        if !rows.is_empty() {
            for r in rows {
                println!("  · {name} | {r}");
                detail_rows.push(format!("{name}: {r}"));
            }
        }
    }

    println!("==== 被修正的错指条数（12 个口径合计）====");
    println!("  幽灵→真定义（可精确断言，与哈希种子无关）= {total_fixed_ghost}");
    println!("  layered 唯一解析且目标 ≠ legacy 的选择   = {total_redirected}");
    for r in &detail_rows {
        println!("  · {r}");
    }

    println!("==== Phase P3 交付物 2：幽灵合并粒度核对 ====");
    println!("  合并规则（两档同一条）：同一个裸名复用同一个幽灵节点 ⇒ 幽灵节点数 == 不同幽灵裸名数");
    println!(
        "  （上面已对 12 个 case 逐一 assert_eq(ghost_nodes, ghost_names.len())，\n     改造前 layered 的「同文件+同裸名+同调用行」按行放大已彻底消除）"
    );
    if ghost_amplification.is_empty() {
        println!("  ✅ 全部 case 满足 layered_ghost_nodes <= legacy_ghost_nodes");
    } else {
        for r in &ghost_amplification {
            println!("  · {r}");
        }
    }
    // Phase P3 交付物 2 的**显式硬断言**：同一 fixture/整仓，layered 幽灵节点数不得高于
    // legacy。改造前 layered 的「同文件+同裸名+同调用行」按行放大（rust 46→49 /
    // python 45→55 / ts 92→121 / js 1577→1607 节点）已合并为与 legacy 一致的「同裸名」，
    // 且文本还原不再为匿名函数表达式造 layered 独有的脏名幽灵 ⇒ 全部 case 现在满足 `<=`。
    assert!(
        ghost_amplification.is_empty(),
        "P3 交付物 2 未达成：仍有 case 的 layered 幽灵节点数 > legacy：\n{}",
        ghost_amplification.join("\n")
    );
    // 严格的"无残留放大"不变量：
    //  layered 的每一个幽灵名，legacy 侧要么同样造了幽灵，要么在该裸名上写出了
    //  `is_resolved=true` 的**假解析**边（这正是 legacy 压低幽灵数的唯一手段 = 误连）。
    // 换句话说：layered 绝不会因为"粒度"而比 legacy 多一个节点，只可能因为"更诚实"而多。
    assert!(
        ghost_unexplained.is_empty(),
        "P3 交付物 2 未达成：layered 仍有解释不了的幽灵放大：\n{}",
        ghost_unexplained.join("\n")
    );
    // legacy 侧逐字节不变（红线 1）里新增一项：legacy 的幽灵节点数本身就是它的假解析计数器，
    // 因此本用例还要求 legacy 的 ghost_nodes 仍等于 P2a 基线（已由 legacy_baseline_diff 覆盖）。
    assert!(
        legacy_failures.is_empty(),
        "红线 1 被破坏：legacy 侧的对照数字与 P2a（8e62033）基线不一致\n{}",
        legacy_failures.join("\n")
    );
    assert!(
        coverage_breaks.is_empty(),
        "口径 B（覆盖中性）被破坏：\n{}",
        coverage_breaks.join("\n")
    );
    assert!(
        total_fixed_ghost >= 1,
        "layered 至少要把一条 legacy 未解析（连幽灵）的调用点解析到真定义，否则接线没有任何精度收益"
    );
    assert!(
        total_redirected >= 1,
        "layered 至少要把一条 legacy 猜错的调用边改指到别的定义，否则消歧收益为零"
    );
}
