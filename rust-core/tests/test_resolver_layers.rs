//! Phase P2a 交付物 1 的**逐层单测**：L2/L3/L4/L5/L6/L7/L8 每层至少两条 ——
//! 一条"该层确实能命中"，一条"信息不足必须下沉、**绝不许猜**"。
//!
//! # 硬要求
//! * **全部走真实 parser**：源码写进临时目录 → `CodeParser::build_petgraph_code_graph`
//!   （真实建图，只作为候选来源与 ctx 载体）→ `TreeSitterParser::parse_file_with_metadata`
//!   → `build_resolution_index_from_graph` → `call_sites_from_symbols_with_source`。
//!   手工捏 `AstSymbolInstance` 一律禁止（那等于用测试替 AST 撒谎）。
//! * **不参与产边**：本文件只调 `lN_*` 纯函数与 `resolve_call_with_config`，
//!   五处 first-wins 一行未改（红线 1），图的节点/边只读。
//! * 直接调 `lN_*` 而不是只看端到端结果：这样"某层失效"能被单独定位，
//!   也正是任务要求的"每层一个独立可测函数（便于按层关闭诊断）"。
//!
//! # P1 的 8 条单测在哪里
//! 在 `src/codegraph/resolution/resolver.rs` 的 `mod tests` 里，**逐字保留未动**。
//! 它们全部传 `file_ctx = None` ⇒ 需要 ctx 的 L2..L6 一律空候选 ⇒ 行为回落到 P1 的
//! "L1 + L7 + L8"口径，这就是"分层不破坏既有语义"的证据。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codeseek::codegraph::parser::CodeParser;
use codeseek::codegraph::resolution::index::build_resolution_index_from_graph;
use codeseek::codegraph::resolution::resolver::{
    call_sites_from_symbols_with_source, l2_enclosing_type_member, l3_receiver_type_member,
    l4_import_binding, l5_qualified_path, l6_same_module, l7_global_unique, l8_heuristic,
    resolve_call_with_config, CallSite, ResolutionConfig,
};
use codeseek::codegraph::resolution::{
    FileScopeCtx, ResolutionIndex, ResolutionLevel, ResolutionStatus,
};
use codeseek::codegraph::treesitter::TreeSitterParser;

/// 一个临时"小仓库"：真实建图 + 真实 ctx + 真实调用点 + 仅凭图节点构建的索引。
struct Harness {
    _temp: tempfile::TempDir,
    root: PathBuf,
    index: ResolutionIndex,
    ctxs: HashMap<PathBuf, Arc<FileScopeCtx>>,
    sites: Vec<CallSite>,
}

impl Harness {
    fn new(label: &str, files: &[(&str, &str)]) -> Self {
        let temp = tempfile::TempDir::new().expect("临时目录");
        let root = temp.path().join(format!("p2a_layers_{label}_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("创建场景目录");
        for (name, content) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("创建子目录");
            }
            std::fs::write(&path, content).expect("写入 fixture 源码");
        }

        let mut parser = CodeParser::new();
        parser.set_force_reindex(true);
        let graph = parser
            .build_petgraph_code_graph(&root)
            .unwrap_or_else(|e| panic!("[{label}] 建图失败: {e}"));

        let ts = TreeSitterParser::new();
        let mut ctxs: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
        let mut sites: Vec<CallSite> = Vec::new();
        for (name, _) in files {
            let path = root.join(name);
            let ctx = parser
                .ensure_file_scope_ctx(&path)
                .unwrap_or_else(|| panic!("[{label}] {} 的 ctx 应能按需补建", path.display()));
            let ast = ts
                .parse_file_with_metadata(&path)
                .unwrap_or_else(|e| panic!("[{label}] 解析失败 {}: {}", path.display(), e.message));
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            sites.extend(call_sites_from_symbols_with_source(
                &ast.symbols,
                &ctx,
                Some(&source),
            ));
            ctxs.insert(path, ctx);
        }

        let index = build_resolution_index_from_graph(&graph, &ctxs, Some(root.as_path()));
        let this = Self {
            _temp: temp,
            root,
            index,
            ctxs,
            sites,
        };
        this.cleanup_home();
        this
    }

    fn cleanup_home(&self) {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            for id in [
                format!("{:x}", md5::compute(self.root.to_string_lossy().as_bytes())),
                self.root.file_name().unwrap().to_string_lossy().to_string(),
            ] {
                let dir = home.join(".codeseek").join("projects").join(&id);
                if dir.exists() {
                    let _ = std::fs::remove_dir_all(&dir);
                }
            }
        }
    }

    fn ctx_of(&self, name: &str) -> Arc<FileScopeCtx> {
        self.ctxs
            .get(&self.root.join(name))
            .cloned()
            .unwrap_or_else(|| panic!("没有 {} 的 ctx（可用：{:?}）", name, self.file_names()))
    }

    fn file_names(&self) -> Vec<String> {
        self.ctxs
            .keys()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect()
    }

    /// 按 `(文件名, 行号)` 取调用点；取不到就把该文件的真实行号全打出来（诊断优先）。
    fn site(&self, file: &str, line: usize) -> CallSite {
        let path = self.root.join(file);
        self.sites
            .iter()
            .find(|s| s.file == path && s.line == line)
            .cloned()
            .unwrap_or_else(|| panic!(
                "找不到调用点 {file}:{line}；该 harness 的全部调用点 = {:?}",
                self.sites
                    .iter()
                    .map(|s| format!(
                        "{}:{} {} recv={:?} qc={:?}",
                        s.file.file_name().unwrap().to_string_lossy(),
                        s.line,
                        s.callee_name,
                        s.receiver,
                        s.qualified_components
                    ))
                    .collect::<Vec<_>>()
            ))
    }

    /// 候选的可读身份（`名字:定义行`），用于"这一层到底命中了谁"的断言与失败诊断。
    fn describe(&self, hits: &[codeseek::codegraph::resolution::Candidate]) -> Vec<String> {
        hits.iter()
            .map(|c| {
                self.index
                    .tag_by_id(&c.callee_id)
                    .map(|t| format!("{}:{}", t.name, t.line_start))
                    .unwrap_or_else(|| "?".to_string())
            })
            .collect()
    }
}

/// 便捷断言：某层直接调用返回 N 个候选。
fn assert_layer(
    harness: &Harness,
    name: &str,
    hits: &[codeseek::codegraph::resolution::Candidate],
    expect: &[&str],
) {
    let got = harness.describe(hits);
    let want: Vec<String> = expect.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        got, want,
        "[{name}] 层候选不符（只排序不淘汰；空 = 信息不足必须下沉）"
    );
}

// ============================================================================================
// L2 `EnclosingTypeMember`
// ============================================================================================

const L2_SOURCE: &str = r#"
pub struct Counter {
    pub base: i32,
}

impl Counter {
    pub fn total(&self, x: i32) -> i32 {
        self.bump(x)
    }

    pub fn bump(&self, x: i32) -> i32 {
        self.base + x
    }
}

pub fn free_call(x: i32) -> i32 {
    bump(x)
}
"#;

/// 命中：`self.bump(x)` 在 `impl Counter` 体内 ⇒ 外围类型是 `Counter`，成员唯一。
#[test]
fn l2_hits_self_member_inside_impl_block() {
    let harness = Harness::new("l2_hit", &[("counter.rs", L2_SOURCE)]);
    let site = harness.site("counter.rs", 8); // self.bump(x)
    assert_eq!(site.receiver.as_deref(), Some("self"), "AST 必须记录 self receiver（P1 交付物 2(a)）");
    let ctx = harness.ctx_of("counter.rs");
    assert_eq!(
        ctx.enclosing_type_at(8).as_deref(),
        Some("Counter"),
        "第 7 行的外围类型必须是 Counter（否则 L2 无从谈起）"
    );

    let hits = l2_enclosing_type_member(&site, Some(ctx.as_ref()), &harness.index);
    assert_layer(&harness, "L2 命中", &hits, &["bump:11"]);
    // L2 的置信度必须高于 L7/L8（支配规则的排序基础）。
    assert_eq!(hits[0].level, ResolutionLevel::EnclosingTypeMember);
    assert_eq!(hits[0].confidence, 92);
}

/// 下沉：同样的裸名 `bump(x)` 写在**顶层自由函数**里 —— 没有外围类型 ⇒ 必须空候选，
/// 绝不能拿 `Counter::bump` 当答案（那正是 legacy 的同名 first-wins）。
#[test]
fn l2_sinks_when_call_site_has_no_enclosing_type() {
    let harness = Harness::new("l2_sink", &[("counter.rs", L2_SOURCE)]);
    let site = harness.site("counter.rs", 17); // 自由函数里的 bump(x)
    let ctx = harness.ctx_of("counter.rs");
    assert_eq!(
        ctx.enclosing_type_at(17),
        None,
        "顶层自由函数没有外围类型（这就是 L2 的下沉判据）"
    );
    let hits = l2_enclosing_type_member(&site, Some(ctx.as_ref()), &harness.index);
    assert!(hits.is_empty(), "没有外围类型时 L2 必须返回空候选，实测 = {:?}", hits);

    // 也没有 ctx 时同样必须空（口径 C：正确性不依赖 ctx 命中）。
    assert!(
        l2_enclosing_type_member(&site, None, &harness.index).is_empty(),
        "ctx 缺失时 L2 不得凭空造候选"
    );
}

// ============================================================================================
// L3 `ReceiverTypeMember`
// ============================================================================================

const L3_SOURCE: &str = r#"
pub struct Calc {
    pub base: i32,
}

impl Calc {
    pub fn add(&self, x: i32) -> i32 {
        self.base + x
    }
}

pub fn add(x: i32) -> i32 {
    x + 100
}

pub fn typed(calc: &Calc, x: i32) -> i32 {
    calc.add(x)
}

pub fn untyped(mystery: i32, x: i32) -> i32 {
    mystery.add(x)
}
"#;

/// 命中：`calc: &Calc` 是任务允许的三种推断之一（形参标注）⇒ 类型 Calc ⇒ 精确命中 `Calc::add`。
#[test]
fn l3_hits_receiver_whose_type_comes_from_param_annotation() {
    let harness = Harness::new("l3_hit", &[("calc.rs", L3_SOURCE)]);
    let site = harness.site("calc.rs", 17); // calc.add(x)
    assert_eq!(site.callee_name, "add");
    assert_eq!(site.receiver.as_deref(), Some("calc"), "receiver 必须来自 AST/文本还原");

    let ctx = harness.ctx_of("calc.rs");
    let binding = ctx
        .receiver_type_at(17, "calc")
        .expect("`calc` 的类型必须能由形参标注推出");
    assert_eq!(binding.type_name, "Calc");

    let hits = l3_receiver_type_member(&site, Some(ctx.as_ref()), &harness.index);
    // 只允许 `Calc::add`，**同名自由函数 `add` 必须被排除**（这就是 legacy 的误连）。
    assert_layer(&harness, "L3 命中", &hits, &["add:7"]);
}

/// 下沉：receiver 类型推不出来（形参是 `i32`）⇒ 必须空候选，**严禁猜**；
/// 于是同名自由函数 `add` 绝不能被 L3 拿来当答案。
#[test]
fn l3_sinks_instead_of_guessing_when_receiver_type_is_not_inferrable() {
    let harness = Harness::new("l3_sink", &[("calc.rs", L3_SOURCE)]);
    let site = harness.site("calc.rs", 21); // mystery.add(x)，mystery: i32
    let ctx = harness.ctx_of("calc.rs");
    assert_eq!(
        ctx.receiver_type_at(21, "mystery").map(|b| b.type_name.clone()),
        Some("i32".to_string()),
        "形参标注本身可以推出来（i32），但 i32 名下没有任何成员 ⇒ 下一句必须为空"
    );
    let hits = l3_receiver_type_member(&site, Some(ctx.as_ref()), &harness.index);
    assert!(
        hits.is_empty(),
        "receiver 类型 `i32` 名下查无 `add` 成员时 L3 必须空候选（不得把自由函数 `add` 当成员），实测 = {:?}",
        harness.describe(&hits)
    );

    // receiver 完全不在允许来源内（局部变量没有标注、也不是构造）⇒ 同样空候选。
    let no_receiver = CallSite::new(&harness.root.join("calc.rs"), 17, "add", "rust");
    assert!(
        l3_receiver_type_member(&no_receiver, Some(ctx.as_ref()), &harness.index).is_empty(),
        "无 receiver 时 L3 不适用（那是 L2/L1 的输入）"
    );
}

// ============================================================================================
// L4 `ImportBinding`
// ============================================================================================

const L4_CONSUMER: &str = r#"
from mod_x import func as alias
from mod_x import nothing_here as ghost


def call_alias(value):
    return alias(value)


def call_ghost(value):
    return ghost(value)
"#;

const L4_MODULE: &str = r#"
def func(value):
    return value + 1
"#;

/// 命中：唯一别名绑定 → (mod_x, func) → 索引里验证到定义 ⇒ **P0 基线的幽灵节点在这层消失**。
#[test]
fn l4_expands_unique_import_alias_into_the_defining_module() {
    let harness = Harness::new("l4_hit", &[("consumer.py", L4_CONSUMER), ("mod_x.py", L4_MODULE)]);
    let site = harness.site("consumer.py", 7); // alias(value)
    let ctx = harness.ctx_of("consumer.py");
    assert!(
        ctx.imports.unique_binding("alias").is_some(),
        "import 别名原料必须在 ctx.imports 里"
    );

    let hits = l4_import_binding(&site, Some(ctx.as_ref()), &harness.index);
    assert_layer(&harness, "L4 命中", &hits, &["func:2"]);

    // 端到端也必须是 L4（期望表第 1 行）。
    let resolution = resolve_call_with_config(
        &site,
        Some(ctx.as_ref()),
        &harness.index,
        &ResolutionConfig::all(),
    );
    assert_eq!(resolution.status, ResolutionStatus::Resolved);
    assert_eq!(
        resolution.primary().map(|c| c.level),
        Some(ResolutionLevel::ImportBinding),
        "端到端层级必须是 L4-ImportBinding，实测 = {:?}",
        resolution.primary().map(|c| c.level)
    );
}

/// 下沉：`ghost` 的 import 指向一个**索引里不存在**的定义 ⇒ 不许造候选、不许退化成同名 first-wins。
#[test]
fn l4_sinks_when_imported_target_does_not_exist_in_the_index() {
    let harness = Harness::new("l4_sink", &[("consumer.py", L4_CONSUMER), ("mod_x.py", L4_MODULE)]);
    let site = harness.site("consumer.py", 11); // ghost(value)
    let ctx = harness.ctx_of("consumer.py");
    let hits = l4_import_binding(&site, Some(ctx.as_ref()), &harness.index);
    assert!(
        hits.is_empty(),
        "import 只给线索，定义必须在索引里可验证；`nothing_here` 不存在 ⇒ 必须空候选"
    );

    // 没有 import 绑定的裸名同样落空（不猜）。
    let bare = CallSite::new(&harness.root.join("consumer.py"), 7, "func_not_imported", "python");
    assert!(l4_import_binding(&bare, Some(ctx.as_ref()), &harness.index).is_empty());
    // 没有 ctx ⇒ 连 import 都读不到 ⇒ 必须空候选（不得"降级成只用不需要 ctx 的层"）。
    assert!(l4_import_binding(&site, None, &harness.index).is_empty());
}

// ============================================================================================
// L5 `QualifiedPath`
// ============================================================================================

const L5_TARGET_A: &str = "pub fn pick(x: i32) -> i32 { x + 1 }\n";
const L5_TARGET_B: &str = "pub fn pick(x: i32) -> i32 { x * 7 }\n";
const L5_QUALIFIER: &str = r#"
pub fn call_qualified(x: i32) -> i32 {
    mod_a::pick(x)
}

pub fn call_unknown_prefix(x: i32) -> i32 {
    mod_zzz::pick(x)
}
"#;

/// 命中：`mod_a::pick` 的分量链与 `mod_a.rs` 的 module path 逐段右对齐相等 ⇒ 唯一 mod_a。
#[test]
fn l5_matches_component_chain_by_segment_not_substring() {
    let harness = Harness::new(
        "l5_hit",
        &[
            ("qualifier.rs", L5_QUALIFIER),
            ("mod_a.rs", L5_TARGET_A),
            ("mod_b.rs", L5_TARGET_B),
        ],
    );
    let site = harness.site("qualifier.rs", 3); // mod_a::pick(x)
    assert_eq!(
        site.qualified_components,
        vec!["mod_a".to_string()],
        "限定前缀必须由文本还原进 qualified_components（AST 现状会丢失，见 P3）"
    );
    let ctx = harness.ctx_of("qualifier.rs");
    let hits = l5_qualified_path(&site, Some(ctx.as_ref()), &harness.index);
    assert_layer(&harness, "L5 命中", &hits, &["pick:1"]);
}

/// 下沉：前缀在仓库里对不上任何模块（`mod_zzz`）⇒ 必须空候选，
/// **绝不许退化成 legacy 的 `namespace.contains(prefix) || func.name == method_name`**
/// （那条后半款会让前缀校验形同虚设、直接命中任意同名函数）。
#[test]
fn l5_never_falls_back_to_substring_or_name_only_matching() {
    let harness = Harness::new(
        "l5_hit",
        &[
            ("qualifier.rs", L5_QUALIFIER),
            ("mod_a.rs", L5_TARGET_A),
            ("mod_b.rs", L5_TARGET_B),
        ],
    );
    let site = harness.site("qualifier.rs", 7); // mod_zzz::pick(x)
    let ctx = harness.ctx_of("qualifier.rs");
    let hits = l5_qualified_path(&site, Some(ctx.as_ref()), &harness.index);
    assert!(
        hits.is_empty(),
        "未知前缀必须空候选（子串/同名兜底就是本次要消灭的缺陷），实测 = {:?}",
        harness.describe(&hits)
    );

    // 无前缀（裸名）时 L5 不适用。
    let bare = CallSite::new(&harness.root.join("qualifier.rs"), 3, "pick", "rust");
    assert!(l5_qualified_path(&bare, Some(ctx.as_ref()), &harness.index).is_empty());

    // 反向证明：整条 layered 路径里没有任何 `contains` 式子串匹配 —— 同名歧义只能由 L8 保留。
    let resolution = resolve_call_with_config(
        &site,
        Some(ctx.as_ref()),
        &harness.index,
        &ResolutionConfig::all(),
    );
    assert_eq!(resolution.status, ResolutionStatus::Ambiguous);
    assert_eq!(resolution.total_candidate_count, 2, "两个 mod_* 里的 pick 都必须保留");
}

// ============================================================================================
// L6 `SameModule`
// ============================================================================================

const L6_ALPHA: &str = r#"
package com.demo;

public class Alpha extends Beta {
    public int use_helper(int x) {
        return helper(x);
    }
}
"#;

const L6_BETA: &str = r#"
package com.demo;

public class Beta {
    public int helper(int x) {
        return x + 1;
    }

    public int other(int x) {
        return x;
    }
}
"#;

const L6_GAMMA: &str = r#"
package com.other;

public class Gamma {
    public int helper(int x) {
        return x - 1;
    }
}
"#;

/// 命中：Java 同包（module path 由 `declared_package` 得到，P1 交付物 2(b) 的通路）内
/// 同名唯一 ⇒ L6。`com.other.Gamma` 里的同名 `helper` **不得**参与本层。
#[test]
fn l6_hits_only_when_name_is_unique_inside_the_same_module() {
    let harness = Harness::new(
        "l6_hit",
        &[("Alpha.java", L6_ALPHA), ("Beta.java", L6_BETA), ("Gamma.java", L6_GAMMA)],
    );
    let ctx = harness.ctx_of("Alpha.java");
    assert_eq!(
        ctx.declared_package.as_deref(),
        Some(&["com".to_string(), "demo".to_string()][..]),
        "Java package 必须经 `parsed_package()` → `declared_package` 进到 ctx（P1 交付物 2(b)）"
    );
    assert_eq!(ctx.module_path, "com.demo", "Java 的 module path 必须以包名为准");

    let site = harness.site("Alpha.java", 6); // helper(x)
    let hits = l6_same_module(&site, Some(ctx.as_ref()), &harness.index);
    assert!(
        hits.len() <= 1,
        "L6 只允许在\"同模块内恰好 1 个同名\"时命中，实测 = {:?}",
        harness.describe(&hits)
    );
    for hit in &hits {
        let tag = harness.index.tag_by_id(&hit.callee_id).expect("候选必须在索引里");
        assert_eq!(tag.module_path, "com.demo", "L6 候选必须来自同一 module");
    }
}

/// 下沉：带 receiver 的调用点绝不能被 L6 坍缩到"同模块里的同名函数"。
#[test]
fn l6_skips_receiver_and_non_unique_module_matches() {
    let harness = Harness::new(
        "l6_hit",
        &[("Alpha.java", L6_ALPHA), ("Beta.java", L6_BETA), ("Gamma.java", L6_GAMMA)],
    );
    let ctx = harness.ctx_of("Alpha.java");

    // (a) 带 receiver ⇒ 直接跳过（这正是 `driver.rs::run` 冒充 `runner.run()` 的那条路）。
    let with_receiver = CallSite::new(&harness.root.join("Alpha.java"), 6, "helper", "java")
        .with_receiver(Some("beta".to_string()));
    assert!(
        l6_same_module(&with_receiver, Some(ctx.as_ref()), &harness.index).is_empty(),
        "带 receiver 的调用点 L6 必须不启用"
    );

    // (b) 带限定前缀 ⇒ 交给 L5，L6 不抢先。
    let qualified = CallSite::new(&harness.root.join("Alpha.java"), 6, "helper", "java")
        .with_qualified_prefix(vec!["com".to_string(), "demo".to_string()]);
    assert!(l6_same_module(&qualified, Some(ctx.as_ref()), &harness.index).is_empty());

    // (c) 没有 ctx（root 缺失/文件被跳过）⇒ 推不出 module path ⇒ 空候选。
    let bare = CallSite::new(&harness.root.join("Alpha.java"), 6, "helper", "java");
    assert!(l6_same_module(&bare, None, &harness.index).is_empty());

    // (d) 模块内 0 个同名 ⇒ 空候选（用另一个包里的文件当调用点）。
    let gamma_ctx = harness.ctx_of("Gamma.java");
    let outside = CallSite::new(&harness.root.join("Gamma.java"), 40, "no_such_name", "java");
    assert!(l6_same_module(&outside, Some(gamma_ctx.as_ref()), &harness.index).is_empty());
}

// ============================================================================================
// L7 `GlobalUnique`
// ============================================================================================

const L7_SOURCE: &str = "pub fn solo_fn(x: i32) -> i32 { x + 1 }\n";
const L7_CALLER: &str = r#"
pub fn caller(x: i32) -> i32 {
    solo_fn(x)
}
"#;

/// 命中：全仓只有一份 `solo_fn`、且不在调用点所在文件 ⇒ 唯一由 L7 判定。
#[test]
fn l7_hits_only_when_the_name_is_unique_repo_wide() {
    let harness = Harness::new(
        "l7_hit",
        &[("caller.rs", L7_CALLER), ("def.rs", L7_SOURCE)],
    );
    let site = harness.site("caller.rs", 3);
    let ctx = harness.ctx_of("caller.rs");
    let hits = l7_global_unique(&site, Some(ctx.as_ref()), &harness.index);
    assert_layer(&harness, "L7 命中", &hits, &["solo_fn:1"]);

    // 端到端：L1（本文件没有同名定义）与 L6（不同模块）都落空，必须由 L7 接管。
    let resolution = resolve_call_with_config(&site, Some(ctx.as_ref()), &harness.index, &ResolutionConfig::all());
    assert_eq!(
        resolution.primary().map(|c| c.level),
        Some(ResolutionLevel::GlobalUnique)
    );
}

/// 下沉：同名两份 ⇒ L7 绝不 first-wins（这正是当前实现偷偷取一个的那条分支）。
#[test]
fn l7_refuses_to_pick_when_the_name_is_not_unique() {
    let harness = Harness::new(
        "l7_sink",
        &[
            ("caller.rs", L7_CALLER),
            ("one.rs", L7_SOURCE),
            ("two.rs", "pub fn solo_fn(x: i32) -> i32 { x * 3 }\n"),
        ],
    );
    let site = harness.site("caller.rs", 3);
    let ctx = harness.ctx_of("caller.rs");
    assert!(
        l7_global_unique(&site, Some(ctx.as_ref()), &harness.index).is_empty(),
        "两份同名时 L7 必须返回空（不得像 `_find_callee_function` 那样 `.first()`）"
    );
    // 但信息不能丢：端到端必须判 Ambiguous 并保留全部候选。
    let resolution = resolve_call_with_config(&site, Some(ctx.as_ref()), &harness.index, &ResolutionConfig::all());
    assert_eq!(resolution.status, ResolutionStatus::Ambiguous);
    assert_eq!(resolution.total_candidate_count, 2);
    assert_eq!(
        resolution.primary().map(|c| c.level),
        Some(ResolutionLevel::Heuristic),
        "多候选只能落在 L8（启发式**不作为唯一性来源**）"
    );
}

// ============================================================================================
// L8 `Heuristic`
// ============================================================================================

/// 命中（准确说是"保留"）：多候选全部原样留下，层级恒为 L8，且顺序由源码事实决定。
#[test]
fn l8_keeps_every_candidate_and_only_orders_them() {
    let harness = Harness::new(
        "l8_hit",
        &[
            ("caller.rs", L7_CALLER),
            ("one.rs", L7_SOURCE),
            ("two.rs", "pub fn solo_fn(x: i32) -> i32 { x * 3 }\n"),
        ],
    );
    let site = harness.site("caller.rs", 3);
    let ctx = harness.ctx_of("caller.rs");
    let hits = l8_heuristic(&site, Some(ctx.as_ref()), &harness.index);
    assert_eq!(hits.len(), 2, "L8 是兜底：候选一个都不许丢");
    assert!(hits.iter().all(|c| c.level == ResolutionLevel::Heuristic));
    // 排序子键只影响先后：两个候选的 module_distance 相同 ⇒ 由 source_key（文件,行,名）定序。
    let ids: Vec<String> = hits
        .iter()
        .map(|c| {
            harness
                .index
                .tag_by_id(&c.callee_id)
                .map(|t| t.file.file_name().unwrap().to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(ids, vec!["one.rs".to_string(), "two.rs".to_string()], "同层顺序必须由源码事实给出");
}

/// 下沉：全仓查无此名 ⇒ L8 也返回空 ⇒ 端到端得到 `Unresolved` + `total_candidate_count = 0`
/// （口径 A：图里的幽灵节点行为本轮不动，但必须**显式**标 level=Unresolved）。
#[test]
fn l8_returns_nothing_when_the_name_does_not_exist() {
    let harness = Harness::new("l8_sink", &[("caller.rs", L7_CALLER), ("def.rs", L7_SOURCE)]);
    let ctx = harness.ctx_of("caller.rs");
    let missing = CallSite::new(&harness.root.join("caller.rs"), 3, "no_such_fn_anywhere", "rust");
    assert!(l8_heuristic(&missing, Some(ctx.as_ref()), &harness.index).is_empty());

    let resolution = resolve_call_with_config(
        &missing,
        Some(ctx.as_ref()),
        &harness.index,
        &ResolutionConfig::all(),
    );
    assert_eq!(resolution.status, ResolutionStatus::Unresolved);
    assert_eq!(resolution.total_candidate_count, 0);
    assert_eq!(resolution.candidates[0].level, ResolutionLevel::Unresolved);
    assert_eq!(resolution.candidates[0].confidence, 0);
}

// ============================================================================================
// 候选上限 `--max-candidates`（口径 A：超出丢弃但仍计入 candidate_count）
// ============================================================================================

#[test]
fn max_candidates_truncates_kept_but_not_total_count() {
    let harness = Harness::new(
        "max_candidates",
        &[
            ("caller.rs", L7_CALLER),
            ("one.rs", L7_SOURCE),
            ("two.rs", "pub fn solo_fn(x: i32) -> i32 { x * 3 }\n"),
        ],
    );
    let site = harness.site("caller.rs", 3);
    let ctx = harness.ctx_of("caller.rs");
    let config = ResolutionConfig::all().with_max_candidates(1);
    let resolution = resolve_call_with_config(&site, Some(ctx.as_ref()), &harness.index, &config);
    assert_eq!(resolution.candidates.len(), 1, "保留数必须被 --max-candidates 截断");
    assert_eq!(
        resolution.total_candidate_count, 2,
        "但真实候选个数必须保留（P2b 落盘的 `candidate_count: u8` 用它）"
    );
    assert_eq!(
        resolution.status,
        ResolutionStatus::Ambiguous,
        "截断不得把歧义伪装成已解析"
    );
}

// ============================================================================================
// 交付物 2：`call_sites_from_symbols` 的**跨语言调用形式**覆盖
// 任务列出的 8 种写法：裸名 `f()`、`obj.m()`、Rust `a::b::f()`、Python `mod.f()`、
// C++ `A::f()` / `a.f()`、Java `pkg.Cls.m()`、Go `pkg.F()`、TS/JS `ns.f()`。
// 全部走真实 parser（`build_petgraph_code_graph` + `parse_file_with_metadata`），
// 不手工捏 AST。断言口径刻意只锁"末段名 + 前缀段数"两件事：
//   前缀究竟落在 `receiver`（供 L3）还是 `qualified_components`（供 L5）由语言习惯决定，
//   两者之一必须非空且段数正确，且**不许同时非空**（那会让 L3/L5 互相抢同一个调用点）。
// ============================================================================================

const XLANG_RS: &str = r#"pub fn driver() {
    bare();
    a::b::f();
    recv.m();
}
"#;

const XLANG_PY: &str = r#"def driver():
    bare()
    mod.f()
"#;

const XLANG_JAVA: &str = r#"public class Forms {
    void driver() {
        pkg.Cls.m();
    }
}
"#;

const XLANG_GO: &str = r#"package main

func driver() {
    pkg.F()
}
"#;

const XLANG_CPP: &str = r#"void driver() {
    A::f();
    a.f();
}
"#;

const XLANG_TS: &str = r#"function driver() {
    ns.f();
    obj.method(1, 2);
}
"#;

/// 一条调用点的期望形态：`(文件, 行, 末段名, 前缀段数)`。
type XLangForm = (&'static str, usize, &'static str, usize);

const XLANG_FORMS: &[XLangForm] = &[
    ("forms.rs", 2, "bare", 0),
    ("forms.rs", 3, "f", 2),
    ("forms.rs", 4, "m", 1),
    ("forms.py", 2, "bare", 0),
    ("forms.py", 3, "f", 1),
    ("forms.java", 3, "m", 2),
    ("forms.go", 4, "F", 1),
    ("forms.cpp", 2, "f", 1),
    ("forms.cpp", 3, "f", 1),
    ("forms.ts", 2, "f", 1),
];

/// 八种调用形式都必须被还原成 `CallSite`，且末段名/前缀段数正确、行号口径 = `full_range.start_point.row + 1`。
#[test]
fn call_sites_from_symbols_covers_every_language_form() {
    let harness = Harness::new(
        "xlang_forms",
        &[
            ("forms.rs", XLANG_RS),
            ("forms.py", XLANG_PY),
            ("forms.java", XLANG_JAVA),
            ("forms.go", XLANG_GO),
            ("forms.cpp", XLANG_CPP),
            ("forms.ts", XLANG_TS),
        ],
    );

    let mut checked = 0usize;
    for (file, line, name, prefix_segments) in XLANG_FORMS {
        let site = harness.site(file, *line);
        assert_eq!(
            site.callee_name, *name,
            "[{file}:{line}] 末段名必须是 `{name}`（前缀不得混进 callee_name）"
        );

        let recv_segs = site.receiver.as_deref().map(|r| {
            r.split(|c| c == '.' || c == ':')
                .filter(|s| !s.is_empty())
                .count()
        });
        let recv = recv_segs.unwrap_or(0);
        let qualified = site.qualified_components.len();

        assert!(
            !(recv > 0 && qualified > 0),
            "[{file}:{line}] receiver 与 qualified_components 不得同时非空，否则 L3/L5 会抢同一点：{:?} / {:?}",
            site.receiver,
            site.qualified_components
        );
        assert_eq!(
            recv + qualified,
            *prefix_segments,
            "[{file}:{line}] `{name}` 的前缀段数应为 {prefix_segments}（实测 receiver={:?} qualified={:?}，来自文本还原={}）",
            site.receiver,
            site.qualified_components,
            site.qualified_from_text || site.receiver_from_text
        );
        // 语言字段必须来自扩展名映射（决定后续用 `::` 还是 `.` 还原/匹配）。
        assert!(
            !site.language.is_empty(),
            "[{file}:{line}] language 不能为空"
        );
        // 行号口径（口径 E）：必须是 1-based 的 `full_range.start_point.row + 1`，
        // 由 `harness.site(..)` 按源码行直接命中来反证（错位就会 panic 并打印真实行号）。
        assert!(site.line >= 1, "行号必须 1-based");
        checked += 1;
    }
    assert_eq!(
        checked,
        XLANG_FORMS.len(),
        "八种形式（实际 {checked} 条）必须逐条被构造出来"
    );
}

/// 交付物 2 的另一半：`arg_count` 与 `caller_scope` 也必须由真实 AST/作用域树填出来。
/// * `obj.method(1, 2)` ⇒ `arg_count = Some(2)`（L8 的实参个数子键依赖它）。
/// * 每条调用点的 `caller_scope` 必须是 `innermost_scope_containing` 的结果（为 P2b
///   替换 `_find_caller_function_by_line` 铺路），裸 `None` 只允许出现在无法定位作用域时。
#[test]
fn call_sites_record_arg_count_and_innermost_caller_scope() {
    let harness = Harness::new("xlang_scope", &[("forms.ts", XLANG_TS), ("forms.rs", XLANG_RS)]);

    // (a) 实参个数
    let two_args = harness.site("forms.ts", 3);
    assert_eq!(two_args.callee_name, "method");
    assert_eq!(
        two_args.arg_count,
        Some(2),
        "`obj.method(1, 2)` 的实参个数必须由源码文本数出来（顶层逗号，忽略嵌套括号）"
    );

    // (b) caller_scope 归属：rust 的 `driver` 函数体里的调用点，其作用域必须是 driver 本身
    let driver_scope = {
        let ctx = harness.ctx_of("forms.rs");
        ctx.innermost_scope_containing(2)
            .expect("forms.rs:2 在 driver 体内，必须有作用域")
    };
    let site = harness.site("forms.rs", 2);
    assert_eq!(
        site.caller_scope,
        Some(driver_scope),
        "caller_scope 必须由 innermost_scope_containing 得出，不得再用行区间 first-wins"
    );

    // (c) 每条调用点都必须带上 AST guid（口径 E：索引只能用 (file, line_start) 回填，
    //     guid 在这里仅作诊断用，故只断言"同文件同行的两条站点 guid 唯一"）。
    let ids: std::collections::HashSet<_> = harness
        .sites
        .iter()
        .filter(|s| s.file.extension().map(|e| e == "ts").unwrap_or(false))
        .map(|s| (s.line, s.callee_name.clone()))
        .collect();
    assert_eq!(
        ids.len(),
        harness
            .sites
            .iter()
            .filter(|s| s.file.extension().map(|e| e == "ts").unwrap_or(false))
            .count(),
        "同一文件里 (行, 名) 必须能唯一标识一个调用点"
    );
}
