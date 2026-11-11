//! Phase P1 交付物 3 + **Phase P2a 交付物 1/2**：解析入口签名（`CallSite` / `resolve_call`）
//! 与分层解析引擎。
//!
//! ## 引擎已接线，且为默认档
//! 本文件实现的分层引擎已接入建图热路径：`_build_function_name_index` /
//! `_resolve_callee_function` / `_resolve_qualified_function_name` / `_analyze_file_calls`
//! 在 `resolver_mode == Layered`（默认）时调用 `resolve_call` / `resolve_call_with_config`
//! 并据此产边；`--resolver=legacy` 是逐字节回滚逃生口，其数值红线由
//! `tests/test_p1_edges_unchanged.rs` 守护。完整口径见 `docs/call-graph-resolution.md`。
//!
//! ## P1 → P2a 的差别
//! P1 只实现 L1（同文件词法）与 L7（全仓唯一同名），其余层留空钩子。
//! P2a 把 **L2/L3/L4/L5/L6/L8 补成真实候选来源**，并把"支配规则"与"排序规则"分开实现：
//!
//! | 层 | 支配判据（可淘汰低层候选） | 信息不足时的行为（**一律下沉，绝不猜**） |
//! |---|---|---|
//! | L1 `LocalLexical` | 调用点所在文件内可见的同名声明；内层遮蔽外层 | 带 receiver/限定前缀的调用点**不走**裸名层（见下方门控） |
//! | L2 `EnclosingTypeMember` | 调用点最内层外围类型的成员；`self.x()`/`this.x()` | 顶层自由函数没有外围类型 ⇒ 空候选 |
//! | L3 `ReceiverTypeMember` | `obj.m()` 且 receiver 类型可推断（只允许形参标注 / `let v: T` / `T::new()`·`new T()`） | 推不出类型 ⇒ 空候选；唯一命中但是**抽象签名**（trait 方法）⇒ 拒绝唯一判定并下沉 |
//! | L4 `ImportBinding` | 裸名命中 `ImportMap::unique_binding` ⇒ 展开成 (module, original_name) 并在索引里验证 | 别名不唯一 / 目标模块里查无此名 ⇒ 空候选 |
//! | L5 `QualifiedPath` | 分量链**逐分量按分隔符边界**精确匹配（含 ImportMap 展开） | 0 个或 ≥2 个模块命中 ⇒ 不唯一 |
//! | L6 `SameModule` | 同 module path 内唯一同名 | 带 receiver ⇒ 跳过（成员调用不得坍缩到"同模块同名自由/他类型函数"） |
//! | L7 `GlobalUnique` | 全仓同名唯一；带 receiver ⇒ 只允许成员候选 | 多候选 ⇒ 交 L8 |
//! | L8 `Heuristic` | **只排序，绝不作为唯一性来源** | 保留全部候选并判 Ambiguous |
//!
//! ## legacy first-wins 与本引擎的关键差异
//! 当前 first-wins 在"多个候选"时**偷偷取一个并标 is_resolved=true**；
//! `resolve_call` 在同样情形返回 `ResolutionStatus::Ambiguous` + **全部候选**。
//!
//! ## 禁止引入 `stack-graphs`
//! 该 crate 已被 GitHub 归档并挂 RUSTSEC-2026-0303，本模块只借它的**分层模型**。

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::candidate::{dedup_by_callee, status_of, Candidate, Resolution};
use super::imports::ImportEntry;
use super::index::{
    declared_before_call_site, module_distance_between, module_separator, FunctionTag,
    ResolutionIndex,
};
use super::level::{ResolutionLevel, ResolutionReason};
use super::scope::ScopeId;
use super::{FileScopeCtx, TypedBinding};
use crate::codegraph::treesitter::structs::SymbolType;
use crate::codegraph::treesitter::AstSymbolInstanceArc;

/// 一次调用点的完整描述。
///
/// 字段设计对照当前图里"调用边"实际带着什么信息（`CallRelation`：
/// `caller_id/callee_id/caller_name/callee_name/caller_file/callee_file/line_number/is_resolved`），
/// 再补上分层判定**必需但当前完全丢失**的三项：
/// `qualified_components`（限定名前缀）、`receiver`（P1 交付物 2(a) 新记录的）、
/// `caller_scope`（P1 交付物 3 的 `ScopeTree` 句柄）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// 归属的调用者函数节点（当前 `_find_caller_function_by_line` 的行区间 first-wins 结果）。
    pub caller_id: Option<Uuid>,
    /// 调用点所在的**最内层**作用域（`ScopeTree::innermost_scope_containing(line)`）。
    /// P2a 由 `call_sites_from_symbols` 真实填上，作为 P2b 替换
    /// `_find_caller_function_by_line` 的准备；本阶段不改那条管线。
    pub caller_scope: Option<ScopeId>,
    pub file: PathBuf,
    /// 1-based 行号（= `full_range.start_point.row + 1`，**口径禁止改动**：
    /// `ResolutionIndex` 与 AST 事实靠 `(file, line_start)` 对齐）。
    pub line: usize,
    /// 被调用名的裸名部分（AST 里的 `name`）。
    pub callee_name: String,
    /// 限定名前缀分段（`mod_a::pick` → ["mod_a"]；`mod_y.helper` → ["mod_y"]）。
    pub qualified_components: Vec<String>,
    /// receiver 源码文本（`calc.add(x)` → Some("calc")），来自 P1 交付物 2(a)。
    pub receiver: Option<String>,
    /// 语言（小写字符串，同 `FunctionInfo.language`）。
    pub language: String,
    /// 实参个数（P2a 的 L8 只能拿它**排序**）。
    pub arg_count: Option<usize>,
    // ============================================================================================
    // Phase P2a：原料出处标记（诊断与"分层关闭"报告用，不参与任何判定）。
    //
    // 实测硬事实（P2a 探针）：7 个语言的 `FunctionCall` 里**只有 Rust 的
    // `field_expression` 分支**会填 `receiver`；限定名前缀在**所有**语言里都丢失
    // （`rust.rs` 的 `scoped_identifier` 分支查的是 `parent.child_by_field_name("path")`，
    // 而 `call_expression` 没有 `path` 字段 ⇒ `namespace` 恒为空串；
    // Python/Java/Go/TS/JS/C++ 的 parser 同样只把最后一段写进 `name`）。
    // 因此 `mod_a::pick` / `mod_y.helper` 这类限定名只能从**调用点源码文本**还原
    // （见 [`call_sites_from_symbols_with_source`]）。这两个布尔就是"这条前缀是文本还原的、
    // 不是 AST 自带的"的诚实标记 —— P3 把语言 parser 补齐之后它们应当恒为 `false`。
    // ============================================================================================
    /// 限定前缀来自源码文本还原（`true` = AST 没给，是 P2a 兜的）。
    pub qualified_from_text: bool,
    /// receiver 来自源码文本还原（`true` = AST 没给，是 P2a 兜的）。
    pub receiver_from_text: bool,
    /// 调用点符号自身的 AST guid（P2b 之后可回溯到符号；P2a 仅用于去重与诊断）。
    pub symbol_guid: Option<Uuid>,
}

impl CallSite {
    /// 最小构造（便于单测与逐步补齐字段）。
    pub fn new(file: &Path, line: usize, callee_name: &str, language: &str) -> Self {
        Self {
            caller_id: None,
            caller_scope: None,
            file: file.to_path_buf(),
            line,
            callee_name: callee_name.to_string(),
            qualified_components: vec![],
            receiver: None,
            language: language.to_string(),
            arg_count: None,
            qualified_from_text: false,
            receiver_from_text: false,
            symbol_guid: None,
        }
    }

    pub fn with_caller_scope(mut self, caller_id: Option<Uuid>, scope: Option<ScopeId>) -> Self {
        self.caller_id = caller_id;
        self.caller_scope = scope;
        self
    }

    pub fn with_receiver(mut self, receiver: Option<String>) -> Self {
        self.receiver = receiver;
        self
    }

    pub fn with_qualified_prefix(mut self, components: Vec<String>) -> Self {
        self.qualified_components = components;
        self
    }

    pub fn with_arg_count(mut self, arg_count: Option<usize>) -> Self {
        self.arg_count = arg_count;
        self
    }

    /// 把限定名前缀 + 裸名拼成完整点号/双冒号形式。
    pub fn full_qualified_name(&self) -> String {
        if self.qualified_components.is_empty() {
            return self.callee_name.clone();
        }
        let separator = module_separator(&self.language);
        let mut parts = self.qualified_components.clone();
        parts.push(self.callee_name.clone());
        parts.join(separator)
    }

    /// 调用点是否是"成员调用形态"：有 receiver（`obj.m()` / `a.b.m()`）。
    ///
    /// 这个判定是 **支配规则的一部分**：带 receiver 的调用点绝不能靠裸名去命中
    /// 同名的自由函数或别的类型的成员（否则 `driver.rs::run` 会冒充 `runner.run()` 的答案）。
    pub fn has_receiver(&self) -> bool {
        self.receiver.as_deref().map(|r| !r.is_empty()).unwrap_or(false)
    }

    /// receiver 是否是 `self` / `this` / `cls`（→ 交给 L2 的外围类型判定）。
    pub fn receiver_is_self(&self) -> bool {
        matches!(
            self.receiver.as_deref().map(|r| r.trim()),
            Some("self") | Some("this") | Some("cls") | Some("super")
        )
    }

    /// 是否带限定前缀（`mod_a::pick` / `mod_y.helper` / `pkg.Cls.m`）。
    pub fn has_qualifier(&self) -> bool {
        !self.qualified_components.is_empty()
    }
}

/// Phase P2a 交付物 1：分层解析的可配置开关（供 `--levels=` / `--max-candidates=` 消费）。
///
/// `enabled_levels` 是 **bitmask**：bit `n` = `ResolutionLevel` 的 `as_u8()` 序号（0..=8）；
/// 因此 `u16` 足够容纳，且 P2b 落盘时可以直接按位与检查。
/// `0` = 一层都不开（任何调用点都只能得到 `Unresolved`），`0x1FF` = 全开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionConfig {
    /// 允许的层级集合（bit = `ResolutionLevel as u8`）。
    pub enabled_levels: u16,
    /// 单次解析保留的候选上限（对应 `--max-candidates`，默认 8）。
    ///
    /// 超出上限的候选**丢弃但仍计数**（见 [`Resolution::total_candidate_count`]），
    /// 与 P2b 的 `candidate_count: u8`（饱和到 255）口径一致。
    pub max_candidates: u8,
}

/// `--max-candidates` 的默认值（任务口径）。
pub const DEFAULT_MAX_CANDIDATES: u8 = 8;

impl Default for ResolutionConfig {
    /// 默认 = **全层开启** + `max_candidates = 8`。
    ///
    /// 注意：这个默认值只影响 `resolve_call*` 引擎（P2a 里只有测试与新只读 API 调用），
    /// **不影响 CLI 默认**：CLI 的 `--resolver` 默认是 `legacy`，`CodeParser` 里存的
    /// mode/max 在 P2a 一律不参与产边。
    fn default() -> Self {
        Self {
            enabled_levels: Self::ALL_LEVELS,
            max_candidates: DEFAULT_MAX_CANDIDATES,
        }
    }
}

impl ResolutionConfig {
    /// 某一层的 bit。
    pub const fn bit(level: ResolutionLevel) -> u16 {
        1u16 << (level.as_u8() as u16)
    }

    /// 全部 9 层（含 `Unresolved` 占位，实际判定不会用到它的 bit）。
    pub const ALL_LEVELS: u16 = 0x01FF;
    /// P1 当时的实际实现面（只有 L1 + L7）。
    pub const P1_LEVELS: u16 = Self::bit(ResolutionLevel::LocalLexical)
        | Self::bit(ResolutionLevel::GlobalUnique);
    /// 只开 L1（诊断"是不是词法层引入的错"）。
    pub const L1_ONLY: u16 = Self::bit(ResolutionLevel::LocalLexical);
    /// 只开 L5（诊断"是不是限定名层给的正确答案"）。
    pub const L5_ONLY: u16 = Self::bit(ResolutionLevel::QualifiedPath);

    /// 全开。
    pub fn all() -> Self {
        Self {
            enabled_levels: Self::ALL_LEVELS,
            max_candidates: DEFAULT_MAX_CANDIDATES,
        }
    }

    /// 该层是否启用。
    pub fn is_enabled(&self, level: ResolutionLevel) -> bool {
        self.enabled_levels & Self::bit(level) != 0
    }

    /// 启用/停用某层（返回新值，便于链式构造诊断组合）。
    pub fn with_level(mut self, level: ResolutionLevel, enabled: bool) -> Self {
        if enabled {
            self.enabled_levels |= Self::bit(level);
        } else {
            self.enabled_levels &= !Self::bit(level);
        }
        self
    }

    /// 设置候选上限（返回新值）。`CodeParser::resolution_config` 用它把
    /// `--max-candidates`（usize）搬进引擎（u8，>255 由调用方饱和）。
    pub fn with_max_candidates(mut self, max_candidates: u8) -> Self {
        self.max_candidates = max_candidates;
        self
    }

    /// 只保留 `1..=highest` 这些层（诊断用：`--levels=l1-l5` 的编程入口）。
    /// `0` 表示全关。
    pub fn up_to(level: ResolutionLevel) -> Self {
        let highest = level.as_u8().min(8);
        let mut bits = 0u16;
        for l in 1..=highest {
            bits |= 1u16 << l;
        }
        Self {
            enabled_levels: bits,
            max_candidates: DEFAULT_MAX_CANDIDATES,
        }
    }

    /// `--levels=` 的语法（大小写不敏感，逗号分隔，可混合预设与显式层）：
    ///
    /// ```text
    /// --levels=all          全开（= 默认）
    /// --levels=p1           只开 P1 落地的两层（L1,L7）
    /// --levels=none         全关（任何调用点都 Unresolved，用于定位"哪一层在起作用"）
    /// --levels=l1,l5        显式层号
    /// --levels=l1-l5        闭区间（l1..=l5）
    /// --levels=all,-l6      先取预设再关掉某层（-/~ 前缀 = 关闭）
    /// ```
    ///
    /// 任何无法识别的 token ⇒ `None`（调用方应当把它当 CLI 参数错误，**不得静默忽略**，
    /// 否则诊断结论会建立在"以为开了其实没开"的配置上）。
    pub fn from_levels_spec(spec: &str) -> Option<Self> {
        let mut bits: u16 = 0;
        let mut max = DEFAULT_MAX_CANDIDATES;
        let mut saw_token = false;

        for raw in spec.split(',') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            saw_token = true;
            // `all,-l6` 形式：第一段决定基线，后面的 `-` 项做减法；不带前缀的显式层是加法。
            let (negate, body) = match token.strip_prefix('-').or_else(|| token.strip_prefix('~')) {
                Some(rest) => (true, rest.trim()),
                None => (false, token),
            };
            let piece = match body.to_ascii_lowercase().as_str() {
                "all" => Self::ALL_LEVELS,
                "p1" => Self::P1_LEVELS,
                "none" => {
                    // `none` 必须是唯一 token：与别的 token 混用没有意义（全关后再开一层是自相矛盾）。
                    bits = 0;
                    continue;
                }
                other => {
                    if let Some(parsed) = Self::parse_level_range(other) {
                        parsed
                    } else if let Some(value) = other.strip_prefix("max=").and_then(|v| v.parse::<u8>().ok()) {
                        max = value;
                        continue;
                    } else {
                        return None;
                    }
                }
            };
            if negate {
                bits &= !piece;
            } else {
                bits |= piece;
            }
        }

        if !saw_token {
            return None;
        }
        Some(Self {
            enabled_levels: bits,
            max_candidates: max,
        })
    }

    /// 解析 `l5` / `l2-l6`（闭区间）。`Unresolved`（层号 0）允许写出来但没有实际作用。
    fn parse_level_range(body: &str) -> Option<u16> {
        let normalized = body.trim_start_matches("level_");
        if let Some((from, to)) = normalized.split_once('-') {
            let a = parse_one_level(from)?;
            let b = parse_one_level(to)?;
            if a > b {
                return None;
            }
            let mut bits = 0u16;
            for level in a..=b {
                bits |= 1u16 << level;
            }
            return Some(bits);
        }
        let only = parse_one_level(normalized)?;
        Some(1u16 << only)
    }
}

/// `l5` / `5` / `L5` → 层号。
fn parse_one_level(token: &str) -> Option<u8> {
    let trimmed = token.trim();
    let digits = trimmed
        .strip_prefix('l')
        .or_else(|| trimmed.strip_prefix('L'))
        .unwrap_or(trimmed);
    if digits.is_empty() {
        return None;
    }
    let value: u8 = digits.parse().ok()?;
    if value > 8 {
        return None;
    }
    Some(value)
}

/// Phase P2a 交付物 2(a)：由**真实 AST 符号列表**构造调用点（不读文件）。
///
/// 与 [`call_sites_from_symbols_with_source`] 的关系：本函数就是 `source = None` 的特化，
/// 保持任务规定的签名。`source = None` 时**限定名前缀只能来自 `symbol.namespace()`**，
/// 而实测 7 个语言里该字段对调用符号恒为空串 ⇒ 前缀为空、L5 自然落空并下沉到 L7。
/// 这是"原料缺失"而不是"规则未实现"，差异原因见 `qualified_from_text` 字段注释。
pub fn call_sites_from_symbols(
    symbols: &[AstSymbolInstanceArc],
    ctx: &FileScopeCtx,
) -> Vec<CallSite> {
    call_sites_from_symbols_with_source(symbols, ctx, None)
}

/// Phase P2a 交付物 2(a)：允许传入调用点所在文件的源码文本来**还原限定分量链**。
///
/// 覆盖任务要求的形态（逐条实测，见 `tests/test_resolution_engine.rs` 的原料表）：
/// - 裸名 `f()` ⇒ `callee_name = "f"`，无前缀、无 receiver；
/// - `obj.m()` ⇒ receiver = `"obj"`（Rust 走 P1 的 `call_receiver`；其余语言只能从文本还原）；
/// - Rust `a::b::f()` ⇒ `qualified_components = ["a","b"]`（**现状：AST 完全丢失该前缀**）；
/// - Python `mod.f()` / Java `pkg.Cls.m()` / Go `pkg.F()` / TS·JS `ns.f()` / C++ `A::f()`、`a.f()`
///   ⇒ 同样只能文本还原，且**首段是"模块"还是"对象"由 `ImportMap` 与索引里的 module path 判定**
///   （是已导入的模块 ⇒ 限定前缀；否则 ⇒ receiver）。
///
/// 行号口径：一律 `full_range.start_point.row + 1`（与 `FunctionInfo.line_start` 同口径，
/// 任务口径 E 明令禁止改动）。`caller_scope` 用 `innermost_scope_containing` ——
/// 这是为 P2b 替换 `_find_caller_function_by_line` 铺的路，**本阶段不接管线**。
pub fn call_sites_from_symbols_with_source(
    symbols: &[AstSymbolInstanceArc],
    ctx: &FileScopeCtx,
    source: Option<&str>,
) -> Vec<CallSite> {
    let mut sites: Vec<CallSite> = Vec::new();
    for symbol in symbols {
        let guard = symbol.read();
        let symbol_ref = guard.as_ref();
        if symbol_ref.symbol_type() != SymbolType::FunctionCall {
            continue;
        }
        let range = symbol_ref.full_range();
        let line = range.start_point.row + 1;
        let language = ctx
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(extension_to_language)
            .unwrap_or_else(|| crate_language(&symbol_ref.language().to_string()));
        // Phase P2a：AST 名字为空的补救。实测 C++ 的 `A::f()` ——
        // `treesitter/parsers/cpp.rs::parse_call_expression` 的 `_ =>` 分支（callee 既不是
        // `identifier` 也不是 `field_expression`，而是 `qualified_identifier`）只把该节点重新
        // 塞回 candidates、`decl.ast_fields.name` **留空**，于是这条调用在 AST 层面是"有调用
        // 符号、无名字"。红线 5 禁止语言 parser 新增/改动符号，因此这里**只用源码文本**把末段
        // 名字还原出来（拿不到文本就照旧跳过），不新增 AST 符号、不参与建图。
        // 判据与 `callee_expression_prefix` 完全一致：形态不干净（下标/链式调用/括号表达式）
        // 一律放弃，**绝不猜**。
        let ast_name = symbol_ref.name().to_string();
        let name = if ast_name.is_empty() {
            source
                .and_then(|text| slice_bytes(text, range.start_byte, range.end_byte))
                .and_then(|expression| callee_name_from_expression(expression, &language))
                .unwrap_or_default()
        } else {
            ast_name
        };
        // Phase P3 交付物 2（覆盖中性·口径 B）：AST 名为空、且文本还原不出合法标识符时，
        // **不再跳过**该调用点，而是与 legacy `_create_unresolved_call_relation` 逐字段同形地
        // 用空名 `""` 发射 —— legacy 对这些匿名函数表达式调用（如压缩 bundle 里的
        // `onClick: function(){…}`）正是用 `""` 造幽灵；若 layered 改而"还原一个脏名"或"直接丢弃"，
        // 前者会多造 layered 独有的幽灵节点（幽灵数 > legacy），后者会破坏"有边调用点相等"。
        // 用空名发射后，所有这类站点折叠进同一个共享 `""` 幽灵 ⇒ 幽灵数与 legacy 严格同形。
        let mut site = CallSite::new(&ctx.file_path, line, &name, &language)
            .with_receiver(symbol_ref.call_receiver().map(|s| s.to_string()))
            .with_symbol_guid(*symbol_ref.guid())
            .with_caller_scope(None, ctx.innermost_scope_containing(line));

        // 1) AST 自带的 `namespace`（Rust 的 scoped_identifier 分支在某些形状下会填）。
        let ast_namespace = symbol_ref.namespace().to_string();
        if !ast_namespace.is_empty() {
            site = site.with_qualified_prefix(split_path_segments(&ast_namespace, &language));
        }

        // 2) 源码文本还原（只有调用方把文本传进来才有这一步）。
        if let Some(text) = source {
            if let Some(expression) = slice_bytes(text, range.start_byte, range.end_byte) {
                if let Some((prefix, head_is_module)) =
                    callee_expression_prefix(expression, &name, &language)
                {
                    if !prefix.is_empty() {
                        // 首段是"已导入的模块/包" ⇒ 整条链当限定前缀；
                        // 否则 ⇒ 首段是 receiver，其余段才是前缀（`a.b.m()` 里 `a` 是对象）。
                        let head_bound_by_import = ctx
                            .imports
                            .unique_binding(&prefix[0])
                            .is_some()
                            || head_is_module;
                        if head_bound_by_import {
                            site.qualified_components = prefix.clone();
                            site.qualified_from_text = true;
                        } else {
                            if site.receiver.is_none() {
                                site.receiver = Some(prefix[0].clone());
                                site.receiver_from_text = true;
                            }
                            site.qualified_components = prefix[1..].to_vec();
                            if !site.qualified_components.is_empty() {
                                site.qualified_from_text = true;
                            }
                        }
                    }
                }
                // 实参个数：文本里顶层括号的逗号计数（拿不到就 None，L8 只能用中性值）。
                if site.arg_count.is_none() {
                    site.arg_count = count_top_level_args(expression);
                }
            }
        }

        // 3) 裸名里本来就带分隔符的形态（少数语言把整条链塞进 name）也要拆出来。
        if !site.has_qualifier() && !site.has_receiver() {
            if prefix_in_name(&name) {
                let mut segments = split_path_segments(&name, &language);
                if segments.len() >= 2 {
                    let callee = segments.pop().unwrap_or_default();
                    let head = segments[0].clone();
                    let head_is_module =
                        ctx.imports.unique_binding(&head).is_some() || looks_like_module(&head);
                    if head_is_module {
                        site.qualified_components = segments;
                        site.callee_name = callee;
                    } else {
                        site.receiver = Some(head.clone());
                        site.qualified_components = segments[1..].to_vec();
                        site.callee_name = callee;
                    }
                }
            }
        }
        sites.push(site);
    }
    // 输出顺序 = 符号在列表里的顺序（tree-sitter 产出顺序，同一份源码稳定）；
    // 为了让"两次独立建图结果逐字段相等"不依赖符号产出顺序，再按源码派生字段重排。
    //
    // Phase P3 收口 C：末位**不再是 `symbol_guid`** —— guid 每次建图都是新的 Uuid，
    // 用它补位意味着"同一份源码的两次独立建图"可能以不同顺序发射同一行的调用点。
    sites.sort_by(|a, b| {
        (
            a.line,
            &a.callee_name,
            &a.file,
            &a.qualified_components,
            &a.receiver,
            a.arg_count,
        )
            .cmp(&(
                b.line,
                &b.callee_name,
                &b.file,
                &b.qualified_components,
                &b.receiver,
                b.arg_count,
            ))
    });
    sites
}

impl CallSite {
    /// 记录调用符号自身的 guid（链式构造）。
    pub fn with_symbol_guid(mut self, guid: Uuid) -> Self {
        self.symbol_guid = Some(guid);
        self
    }
}

/// 文件扩展名 → 语言（与 `CodeParser::_detect_language` 同一口径的**最小子集**，
/// 只用于限定名分隔符的选择）。
pub fn extension_to_language(extension: &str) -> String {
    let mapped = match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "java" => "java",
        "go" => "go",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "c" | "h" => "c",
        "cs" => "csharp",
        "kt" => "kotlin",
        _ => "unknown",
    };
    mapped.to_string()
}

/// AST `LanguageId` 的 Display 形式 → `FunctionInfo.language` 的小写口径
/// （`_detect_language` 用的是"扩展名小写"，而 `LanguageId` 的 Debug 形式大小写不一致，
/// 这里统一成前者，保证 `CallSite.language` 与索引里 `FunctionTag.language` 同源）。
fn crate_language(raw: &str) -> String {
    raw.to_ascii_lowercase()
}

/// 名字里是否已经含有限定分隔符。
fn prefix_in_name(name: &str) -> bool {
    name.contains('.') || name.contains("::")
}

/// 拆分限定链：Rust/C++/Java 内部类用 `::`，其余用 `.`；混合出现时**两种都切**。
pub fn split_path_segments(raw: &str, language: &str) -> Vec<String> {
    let mut current = String::new();
    let mut out: Vec<String> = Vec::new();
    let bytes: Vec<char> = raw.chars().collect();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let ch = bytes[idx];
        if ch == ':' && idx + 1 < bytes.len() && bytes[idx + 1] == ':' {
            flush_segment(&mut current, &mut out);
            idx += 2;
            continue;
        }
        if ch == '.' || (ch == '/' && language == "go") {
            flush_segment(&mut current, &mut out);
            idx += 1;
            continue;
        }
        current.push(ch);
        idx += 1;
    }
    flush_segment(&mut current, &mut out);
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

fn flush_segment(current: &mut String, out: &mut Vec<String>) {
    let piece = current.trim().to_string();
    if !piece.is_empty() {
        out.push(piece);
    }
    current.clear();
}

/// 安全字节切片（越界/非 UTF-8 边界 ⇒ `None`，绝不 panic）。
fn slice_bytes(text: &str, start: usize, end: usize) -> Option<&str> {
    if start >= end || end > text.len() {
        return None;
    }
    text.get(start..end)
}

/// 看起来像模块/包名（全小写开头，例如 `mod_a`、`pkg`、`crate`）——
/// 只作为 `ImportMap` 查不到时的**次级**判据，且判错只会导致"L5 落空并下沉"，不会误命中。
fn looks_like_module(segment: &str) -> bool {
    let first = match segment.chars().next() {
        Some(c) => c,
        None => return false,
    };
    first.is_lowercase()
        && !matches!(
            segment,
            "self" | "this" | "cls" | "super" | "std" | "core"
        )
        && (segment.starts_with("mod") || segment.starts_with("pkg") || segment.contains('_'))
}

/// AST 把被调用名整个丢掉时（实测 C++ 的 `A::f()`），从调用表达式文本里还原**末段名字**。
///
/// 与 [`callee_expression_prefix`] 共用同一套保守判据：
/// 1. 只取第一个 `(` 之前的部分；
/// 2. 该部分去掉空白后必须完全由"标识符字符 + `::` / `.` / `->`"组成，否则放弃；
/// 3. 拆出来的段必须非空、且末段首字符是标识符起始字符（不能是数字），否则放弃。
///
/// 拿不到就返回 `None` ⇒ 该调用点和改造前一样被跳过，**绝不猜一个名字出来**。
pub fn callee_name_from_expression(expression: &str, language: &str) -> Option<String> {
    let head = match expression.find('(') {
        Some(pos) => &expression[..pos],
        None => expression,
    };
    let head = head.trim();
    if head.is_empty() {
        return None;
    }
    let cleaned = head.replace("::", " ").replace('.', " ").replace("->", " ");
    if cleaned
        .chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c.is_whitespace()))
    {
        return None;
    }
    let segments = split_path_segments(head, language);
    let last = segments.last().cloned()?;
    // Phase P3 交付物 2：还原出的末段必须是**单个合法标识符**。
    // 压缩 JS bundle 里的匿名函数表达式调用（`onClick: function(e,t){…}`、
    // `new function(){…}`）AST 名为空，早期版本会把整段 `head` 末"段"直接当名字，
    // 于是还原出 `function e` / `function t`（含内部空格，根本不是标识符）或
    // `function` / `super`（JS 保留字，不可能是被调用的函数名）。legacy 对这些空 AST
    // 名站点统一用空名 `""` 幽灵，故还原失败时本函数**同样返回 `None`**，让调用点
    // 落回与 legacy 同形的 `""` 幽灵，而不是造出一个 layered 独有的脏名节点。
    if !is_single_identifier(&last) {
        return None;
    }
    // 还原出的末段是语言**保留字**时同样判定为脏名（`function`/`super`/`new`… 不可能
    // 是被调用的函数名，它们出现只可能是匿名函数表达式/构造调用被 AST 丢了名字）。
    if is_reserved_word(&last) {
        return None;
    }
    let first = last.chars().next()?;
    if !first.is_alphanumeric() && first != '_' {
        return None;
    }
    Some(last)
}

/// 判断字符串是否为**单个合法标识符**：非空、不含空白、每个字符都是标识符字符
/// （字母/数字/下划线/`$`），且首字符不是数字。用于过滤文本还原产出的脏名。
fn is_single_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.chars().next().map_or(true, |c| c.is_numeric())
}

/// 文本还原末段是否为某语言的**保留字**（不可能是被调用的函数名）。
/// 只列"匿名函数表达式/构造/运算符被误当 callee"时真实会出现的词，避免误杀 `new_foo` 这类名。
fn is_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "function" | "super" | "this" | "class" | "new" | "typeof" | "void" | "delete"
    )
}

/// 从一次调用的源码文本里取出 `被调用表达式(实参)` 的前缀分量链。
///
/// 返回 `(segments, head_is_module_like)`，`segments` **不含**被调用名本身。
/// 规则（保守，宁可返回空也不猜）：
/// 1. 只取第一个顶层 `(` 之前的部分作为 callee 表达式；
/// 2. 表达式去掉空白后必须完全由标识符 + `::` / `.` / `->` 组成，否则放弃（`foo().bar`、
///    `(a+b).c`、数组下标等一律不认）；
/// 3. 末段必须等于 AST 给的 `name`，否则说明 AST 与该文本对不上 ⇒ 放弃。
pub fn callee_expression_prefix(
    expression: &str,
    callee_name: &str,
    language: &str,
) -> Option<(Vec<String>, bool)> {
    let head = match expression.find('(') {
        Some(pos) => &expression[..pos],
        None => expression,
    };
    let head = head.trim();
    if head.is_empty() {
        return None;
    }
    // 形态检查：只允许标识符字符与分隔符。
    let cleaned = head.replace("::", " ").replace('.', " ").replace("->", " ");
    if cleaned
        .chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c.is_whitespace()))
    {
        return None;
    }
    let mut segments = split_path_segments(head, language);
    if segments.len() < 2 {
        return None;
    }
    let last = segments.last().cloned().unwrap_or_default();
    if last != callee_name {
        // AST 的名字与文本末段对不上 ⇒ 我们读的不是同一个表达式，放弃。
        return None;
    }
    segments.pop();
    // ========================================================================================
    // Phase P2a：**只有真正出现 `::` 或首段"看起来像模块名"才算模块限定**。
    //
    // 早期版本在这里直接写了 `|| language == "rust" || language == "cpp"`，那会让
    // `calc.add(x)` 被读成"模块 calc 里的函数 add"，于是 `receiver` 被降级、L3 永远拿不到
    // 输入（任务期望表里 `calc.add` 必须是 ReceiverTypeMember）。Rust/C++ 里 `.` 是成员访问、
    // `::` 才是路径分隔，因此判据必须按**分隔符**而不是按语言。
    // Python/Java/Go/TS 的 `.` 既可能是模块也可能是对象，交给 `looks_like_module` +
    // `ImportMap` 判定；判错的后果只是"L5 落空并下沉"，不会误连。
    // ========================================================================================
    let used_double_colon = head.contains("::");
    let head_is_module =
        used_double_colon || looks_like_module(segments.first().map(String::as_str).unwrap_or(""));
    Some((segments, head_is_module))
}

/// 数顶层实参个数（文本口径，嵌套括号/尖括号不计数；空实参 ⇒ `Some(0)`）。
pub fn count_top_level_args(expression: &str) -> Option<usize> {
    let open = expression.find('(')?;
    let bytes: Vec<char> = expression.chars().collect();
    let mut depth = 0usize;
    let mut commas = 0usize;
    let mut saw_content = false;
    let mut idx = open + 1;
    let close = loop {
        if idx >= bytes.len() {
            break None;
        }
        match bytes[idx] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => break Some(idx),
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            c if !c.is_whitespace() => saw_content = true,
            _ => {}
        }
        idx += 1;
    };
    let close = close?;
    if !saw_content {
        return Some(0);
    }
    // 尾随逗号不计：`f(a,)` 仍是 1 个实参。
    let tail: String = bytes[open + 1..close].iter().collect();
    let trailing_comma = tail.trim_end().ends_with(',');
    Some(if trailing_comma {
        commas
    } else {
        commas + 1
    })
}

// ============================================================================================
// Phase P2a 交付物 1：分层引擎
//
// **每一层一个独立可测函数**，签名统一为
// `fn lN_...(call_site, file_ctx: Option<&FileScopeCtx>, index) -> Vec<Candidate>`，
// 返回**空 Vec = 该层信息不足 ⇒ 下沉**；返回非空 = 该层接管（唯一 ⇒ `Resolved`，
// 多于一个 ⇒ `Ambiguous` 且全部保留），调用方 `ResolutionConfig` 可以逐层关闭做诊断。
//
// 支配规则与排序规则的分工：
// - 支配：层的**先后次序**本身（高层一旦产出候选就不再看低层）+ 下面逐条写的准入判据；
// - 排序：只发生在**同一层内部**，全部经 `rank_candidates`（模块距离 / 声明先后 / arity）。
// ============================================================================================

/// arity 匹配度打分（L8 的排序子键；**只排序，不作为唯一性来源**）。
///
/// `2` = 实参数与形参数相等；`1` = 有一侧信息缺失（中性）；`0` = 明确不等。
pub fn arity_grade(arg_count: Option<usize>, param_count: Option<usize>) -> u8 {
    match (arg_count, param_count) {
        (Some(args), Some(params)) if args == params => 2,
        (Some(_), Some(_)) => 0,
        _ => 1,
    }
}

/// 给候选填上排序子键（模块距离 / 声明是否在前 / arity）。
fn decorate(site: &CallSite, level: ResolutionLevel, reason: ResolutionReason, tag: &FunctionTag) -> Candidate {
    Candidate::new(tag.id, level, 0, reason)
        .with_sort_keys(
            module_distance_between(&site.file, &tag.file),
            declared_before_call_site(&site.file, site.line, tag),
            arity_grade(site.arg_count, tag.param_count),
        )
        // Phase P2a：同层多候选的最终顺序必须由**源码事实**决定，
        // 不能由"每次重建都重新生成的 `FunctionInfo.id`"决定（跨进程确定性）。
        .with_source_key(&tag.file.to_string_lossy(), tag.line_start, &tag.name)
}

/// 候选集合 → 去重 → 确定性排序 → 截断，并给出 `Resolution`。
///
/// 截断只影响 `candidates` 的**长度**，`total_candidate_count` 仍记录截断前的真实个数，
/// 与 P2b 的 `candidate_count: u8`（饱和 255）口径对齐。
fn finish(level: ResolutionLevel, hits: Vec<Candidate>, max: u8) -> Resolution {
    let unique = dedup_by_callee(&hits);
    let total = unique.len();
    let limit = (max as usize).max(1);
    let kept: Vec<Candidate> = unique.into_iter().take(limit).collect();
    let mut resolution = Resolution::from_candidates(kept);
    if level != ResolutionLevel::Unresolved {
        debug_assert!(
            resolution.candidates.iter().all(|c| c.level == level),
            "同一层的候选必须同层级：{:?}",
            resolution.candidates
        );
    }
    resolution.total_candidate_count = total;
    // ============================================================================================
    // Phase P2a 交付物 1（`--max-candidates` 单测暴露的缺陷）：**状态必须由真实候选数决定**。
    //
    // `Resolution::from_candidates` 用"保留列表的长度"判状态，截断后长度是 1 ⇒
    // 会把"其实有 5 个候选、只是被上限截到 1 个"谎报成 `Resolved`。
    // 口径 A 的落盘判据是 `is_resolved = (level > 0 && candidate_count == 1)`，
    // 其中 `candidate_count` 是**截断前**的真实个数，因此这里必须按 `total` 重判。
    // （`max` 不生效时 `total == candidates.len()`，本行等价于 P1 的行为，P1 单测不受影响。）
    // ============================================================================================
    resolution.status = status_of(total);
    debug_assert_eq!(resolution.status, status_of(resolution.total_candidate_count));
    debug_assert!(resolution.candidates.len() <= resolution.total_candidate_count);
    resolution
}

/// **L1 `LocalLexical`**：调用点所在文件内、且在调用点的作用域链上可见的同名声明。
///
/// 支配判据：内层同名声明**遮蔽**外层/导入/全局 —— 用 `ScopeTree::innermost_scope_containing`
/// + 祖先链判定可见性，**不再用行区间 first-wins**。
/// 下沉/门控判据：
/// - 调用点带 receiver（`obj.m()`）或带限定前缀（`a::m()`）⇒ 本层一律不产出候选。
///   理由：裸名匹配对成员调用没有意义，正是 P0 基线里 `driver.rs::run` 冒充
///   `runner.run()` 答案的那类误连；
/// - 没有 ctx（文件被 MD5 跳过）⇒ 退回 P1 的"同文件"口径，**结果只会更保守不会更激进**；
/// - 本文件没有同名声明 ⇒ 空候选。
pub fn l1_local_lexical(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    if site.has_receiver() || site.has_qualifier() {
        return vec![];
    }
    let same_file: Vec<&FunctionTag> = index
        .candidates_by_name(&site.callee_name)
        .iter()
        .filter(|tag| tag.file == site.file)
        .collect();
    if same_file.is_empty() {
        return vec![];
    }

    // 有作用域信息时按**词法遮蔽**收紧：声明的父作用域必须在调用点的祖先链上
    // （不在链上 = 声明在别的函数体里，调用点看不见它）；链上有多个可见同名声明时，
    // 只保留"最深"的那一组 —— 内层同名声明遮蔽外层，这是 L1 的支配判据。
    if let Some(ctx) = ctx {
        if let Some(call_scope) = ctx.innermost_scope_containing(site.line) {
            let chain: Vec<ScopeId> = ctx.scopes.scope_chain(call_scope);
            let mut visible: Vec<(usize, &FunctionTag)> = Vec::new();
            for tag in &same_file {
                // 声明所在作用域 = "最内层包含该行、且不是它自己"的那一层 ⇒ 用声明行的
                // 父作用域近似（`innermost_scope_containing(line_start)` 会命中声明自身，
                // 因为它的行区间从这一行开始），所以这里取该作用域的 parent。
                let own = ctx.innermost_scope_containing(tag.line_start);
                let declared_in = own.and_then(|id| {
                    // 命中声明自身时往上取一层；否则就是它所在的容器作用域。
                    let scope = ctx.scopes.scope(id)?;
                    if scope.line_start == tag.line_start {
                        scope.parent
                    } else {
                        Some(id)
                    }
                })
                // 声明行不被任何作用域覆盖（异常形状）⇒ 视为模块级，仍对全文件可见。
                .or_else(|| Some(ctx.scopes.root()));
                match declared_in {
                    Some(id) if chain.contains(&id) => {
                        let depth = ctx.scopes.scope(id).map(|s| s.depth).unwrap_or(0);
                        visible.push((depth, *tag));
                    }
                    _ => {}
                }
            }
            if !visible.is_empty() {
                let deepest = visible.iter().map(|(depth, _)| *depth).max().unwrap_or(0);
                let winners: Vec<&FunctionTag> = visible
                    .into_iter()
                    .filter(|(depth, _)| *depth == deepest)
                    .map(|(_, tag)| tag)
                    .collect();
                return winners
                    .iter()
                    .map(|tag| {
                        decorate(
                            site,
                            ResolutionLevel::LocalLexical,
                            ResolutionReason::FoundInEnclosingScope,
                            tag,
                        )
                    })
                    .collect();
            }
        }
    }
    // 与 P1 逐字等价的兜底口径（没有 ctx / 祖先链上找不到任何同名声明）：
    // 同文件同名即词法可见。结果只会**更保守**（候选更多），绝不会凭空少一个。
    same_file
        .iter()
        .map(|tag| {
            decorate(
                site,
                ResolutionLevel::LocalLexical,
                ResolutionReason::DefinedInSameFile,
                tag,
            )
        })
        .collect()
}

/// **L2 `EnclosingTypeMember`**：调用点所属类型（`impl`/`class` 体内无限定成员调用、`self.x()`）。
///
/// 支配判据：候选必须**是该类型的成员**（`FunctionTag.enclosing_type == 外围类型名`）。
/// 下沉判据：
/// - 没有 ctx ⇒ 无法知道外围类型 ⇒ 空候选；
/// - 调用点在顶层自由函数/模块级语句里 ⇒ `enclosing_type_at` 返回 `None` ⇒ 空候选；
/// - 调用点带限定前缀 ⇒ 交给 L5（前缀语义优先于"外围类型"）。
pub fn l2_enclosing_type_member(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    // 没有 ctx ⇒ 不知道外围类型 ⇒ 空候选（下沉，绝不猜）。
    let ctx = match ctx {
        Some(ctx) => ctx,
        None => return vec![],
    };
    if site.has_qualifier() {
        return vec![];
    }
    // `self.x()` / `this.x()` 与"impl 体内的无限定调用"都走外围类型；
    // 但带**非 self** receiver 的调用点是 L3 的输入，本层不得抢先。
    if site.has_receiver() && !site.receiver_is_self() {
        return vec![];
    }
    // 调用点在顶层自由函数/模块级语句里 ⇒ 没有外围类型 ⇒ 空候选。
    let type_name = match ctx.enclosing_type_at(site.line) {
        Some(name) => name,
        None => return vec![],
    };
    if type_name.is_empty() {
        return vec![];
    }
    index
        .candidates_of_type(&type_name, &site.callee_name)
        .iter()
        .map(|tag| decorate(site, ResolutionLevel::EnclosingTypeMember, ResolutionReason::FoundInEnclosingScope, tag))
        .collect()
}

/// **L3 `ReceiverTypeMember`**：`obj.method()` 且 receiver 类型可推断。
///
/// 推断能力**严格限定在任务允许的三种**（`FileScopeCtx::typed_bindings` 只装这三种，
/// 见 `resolution::mod::collect_typed_bindings`）：
/// `let v: T = ...` 显式标注 / `let v = T::new(..)`·`new T(..)` 构造 / 函数形参 `v: T`。
///
/// 下沉判据（**严禁猜**）：
/// - 无 receiver、或 receiver 是 `self`/`this`（那是 L2 的口径）⇒ 空候选；
/// - 该 receiver 在调用点不可见/推不出类型 ⇒ 空候选；
/// - 按 `Type::method` 查到 0 条 ⇒ 空候选（同名自由函数**不能**被当成答案）；
/// - 恰好 1 条但它是**无函数体的抽象签名**（trait 方法）⇒ 空候选。
///   AST 里没有 impl→trait 反向链接，无法枚举实现者；此时让下层把
///   **全部同名成员**保留成 `Ambiguous`（任务对 `ambiguous_trait_impl` 的验收口径），
///   而不是自信地错指 trait 声明。
pub fn l3_receiver_type_member(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    let ctx = match ctx {
        Some(ctx) => ctx,
        None => return vec![],
    };
    let receiver = match site.receiver.as_deref() {
        Some(receiver) => receiver,
        None => return vec![],
    };
    if receiver.is_empty() || site.receiver_is_self() {
        return vec![];
    }
    // receiver 的类型推不出来（不在允许的三种来源内，或该绑定在调用点不可见）⇒ 空候选。
    let binding: &TypedBinding = match ctx.receiver_type_at(site.line, receiver) {
        Some(binding) => binding,
        None => return vec![],
    };
    let hits = index.candidates_of_type(&binding.type_name, &site.callee_name);
    if hits.is_empty() {
        return vec![];
    }
    if hits.len() == 1 && !hits[0].has_body {
        // 唯一命中是一条抽象签名 ⇒ 拒绝唯一判定，下沉（见函数注释）。
        return vec![];
    }
    hits.iter()
        .map(|tag| decorate(site, ResolutionLevel::ReceiverTypeMember, ResolutionReason::ReceiverTypeNameMatches, tag))
        .collect()
}

/// **L4 `ImportBinding`**：裸名命中 `ImportMap::unique_binding` ⇒ 展开成 (module, original_name)
/// 并**回到索引里验证该模块内确有此定义**。
///
/// 支配判据：别名唯一命中 + 模块内验证通过。
/// 下沉判据：
/// - `binding_count(name) != 1` ⇒ `unique_binding` 返回 `None`（多条 import 绑定同一本地名时
///   绝不 first-wins）；
/// - 索引里该模块下查无此名 ⇒ 空候选（import 只给了线索，定义必须可验证）；
/// - 调用点带 receiver / 带限定前缀 ⇒ 本层不适用（那是 L5/L3 的输入）；
/// - CJS `const {x} = require("m")` 至今不产出 `ImportDeclaration`（**口径 D：本轮不修**）
///   ⇒ 本层自然落空，下沉到 L7。见 `imports.rs` 的 `LOCAL_NAME_RULES` 与 TODO。
pub fn l4_import_binding(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    let ctx = match ctx {
        Some(ctx) => ctx,
        None => return vec![],
    };
    if site.has_receiver() || site.has_qualifier() {
        return vec![];
    }
    // 裸名在本文件没有**唯一**的 import 绑定（0 条或 ≥2 条）⇒ 空候选，绝不 first-wins。
    let entry: &ImportEntry = match ctx.imports.unique_binding(&site.callee_name) {
        Some(entry) => entry,
        None => return vec![],
    };
    let separator = module_separator(&site.language);
    let target_name = entry.original_name.as_deref().unwrap_or(&site.callee_name);

    // ============================================================================================
    // Phase P2a：模块链**右对齐后缀**匹配（修 L4 的第一版实现缺陷）。
    //
    // 缺陷（P2a 探针实测）：早期版本调用 `index.candidates_under_module(module_prefix, sep)`，
    // 那个 API 是**左锚定**的（`module == prefix || module.starts_with(prefix + sep)`）。
    // 但 `FunctionTag::module_path` 是由"文件相对仓库根"推导出来的（见 `index::derive_module_path`）：
    // Python `tests/.../python_import_alias/mod_x.py` → `"python_import_alias.mod_x"`。
    // import 语句里的名字只是**最后一段** `mod_x`，永远不可能成为整条链的前缀
    // ⇒ L4 恒空 ⇒ `alias()` 一路下沉到 L8 也查不到（全仓根本没有名为 `alias` 的函数），
    // 于是任务期望表里"唯一 → mod_x.py::func，level=4，candidate_count=1（幽灵节点消失）"落空。
    //
    // 修法与 L5 保持同一把尺子：用 `module_chain_matches` 做**按分隔符边界的右对齐后缀**比较，
    // 候选池取 `candidates_by_name(target_name)`（`by_name` 已按
    // `(file, line_start, name, id)` 稳定排序，因此顺序与 HashMap 种子无关）。
    // 这样 `not_mod_x`、`mod_xx`、`python_import_aliasX.mod_x` 之类都不会误命中（不是子串匹配）。
    //
    // 同时删掉旧的退化分支：它把 `module_path`（**已经含原函数名**，如 `["mod_x","func"]`）
    // 再拼一次 `target_name`，构造出 `mod_x.func.func` 这种不存在的限定名。
    // ============================================================================================
    let mut chain: Vec<String> = if entry.module_prefix.is_empty() {
        // 单段 import（Go `import "fmt"` / Python `import mod_x`）：module_prefix 为空时退回完整链，
        // 并把链尾若正好是原函数名的那一段剥掉。
        let mut full = entry.module_path.clone();
        if full.len() > 1 && full.last().map(String::as_str) == Some(target_name) {
            full.pop();
        }
        full
    } else {
        entry.module_prefix.clone()
    };
    if chain.last().map(String::as_str) == Some(target_name) {
        // 防止链尾与原函数名重复（不同语言 parser 对 module_prefix 的切分口径不完全一致）。
        chain.pop();
    }
    if chain.is_empty() {
        return vec![];
    }

    let mut hits: Vec<Candidate> = Vec::new();
    for tag in index.candidates_by_name(target_name) {
        if module_chain_matches(&chain, &tag.module_path, separator) {
            hits.push(decorate(
                site,
                ResolutionLevel::ImportBinding,
                ResolutionReason::BoundByImport,
                tag,
            ));
        }
    }
    hits
}

/// **L5 `QualifiedPath`**：分量链**逐分量精确匹配**。
///
/// 与 legacy 的关键差别：`parser.rs:1602` 用的是 `func.namespace.contains(prefix) || func.name == method_name`
/// —— 子串匹配 + 后半条让前缀校验形同虚设。本层**只按分隔符边界比较整段**，
/// 绝不复现子串匹配（`layered` 路径里没有任何 `contains`）。
///
/// 匹配顺序（每一步都是"整段相等"）：
/// 1. 前缀链首段若在 `ImportMap` 里有唯一绑定 ⇒ 用 import 的模块前缀**替换**它（`mod_y` → `mod_y`、
///    Java `pkg.Cls` → 包链）；
/// 2. 剩下的分量链与候选 `module_path` 做**右对齐后缀**比较（目录式 module path 缺少
///    crate/module 根，见 `index.rs` 的 TODO(P2)，后缀比较是它唯一安全的等价物）；
/// 3. 后缀比较为空时，再试"链首 == 候选的 enclosing_type"（`Calc::add` 这类类型限定）。
///
/// 下沉判据：无前缀 ⇒ 空候选；命中 0 条 ⇒ 空候选；命中 ≥2 条 ⇒ 保留全部并判 `Ambiguous`。
pub fn l5_qualified_path(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    if site.qualified_components.is_empty() {
        return vec![];
    }
    let separator = module_separator(&site.language);
    let mut chain: Vec<String> = site.qualified_components.clone();
    // 1) import 展开：链首是本文件已绑定的模块名 ⇒ 换成 import 的完整模块前缀。
    if let Some(entry) = ctx.and_then(|c| c.imports.unique_binding(&chain[0])) {
        if !entry.module_prefix.is_empty() {
            let expanded: Vec<String> = entry.module_prefix.clone();
            chain.splice(0..1, expanded);
        }
    }
    // 若链尾恰好等于被调用名（少数语言把整条链塞进 name 的形态），去掉它以免自我比较。
    if chain.last().map(String::as_str) == Some(site.callee_name.as_str()) {
        chain.pop();
    }
    if chain.is_empty() {
        return vec![];
    }

    // 候选池 = 全仓同名。带 receiver 的限定链（`a.b.f()`）在这里**不**再额外收缩：
    // 收缩已由 L3/L7 负责，本层的判据是"分量链逐段相等"，比池子收窄更严格。
    let pool = index.candidates_by_name(&site.callee_name);

    let mut hits: Vec<Candidate> = Vec::new();
    for tag in pool {
        if module_chain_matches(&chain, &tag.module_path, separator) {
            hits.push(decorate(site, ResolutionLevel::QualifiedPath, ResolutionReason::QualifiedPrefixMatchesModule, tag));
            continue;
        }
        // 3) 类型限定（`Calc::add` / `Loudish::run`）。
        if chain.len() == 1 {
            if let Some(enclosing) = tag.enclosing_type.as_deref() {
                if enclosing == chain[0] {
                    hits.push(decorate(site, ResolutionLevel::QualifiedPath, ResolutionReason::QualifiedPrefixMatchesModule, tag));
                }
            }
        }
    }
    hits
}

/// 右对齐后缀比较：`chain` 的每一段都必须是 `module_path` 的**整段**（按分隔符切开后逐一相等）。
///
/// 这就是"彻底取代 `namespace.contains(prefix)` 子串匹配"的实现（任务分层规则 L5 的硬要求）。
pub fn module_chain_matches(chain: &[String], module_path: &str, separator: &str) -> bool {
    if chain.is_empty() || module_path.is_empty() {
        return false;
    }
    let segments: Vec<&str> = module_path
        .split(separator)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < chain.len() {
        return false;
    }
    let offset = segments.len() - chain.len();
    segments[offset..].iter().zip(chain.iter()).all(|(a, b)| a == b)
}

/// **L6 `SameModule`**：同 module path 内**唯一**同名候选。
///
/// 下沉判据：
/// - 带 receiver 或带限定前缀 ⇒ 直接不启用（成员调用不得坍缩到"同模块里的同名函数"，
///   这正是 `driver.rs::run` 会冒充 `runner.run()` 答案的那条路径）；
/// - 本文件 module path 推不出来（root 缺失）⇒ 空候选；
/// - 同模块内 0 个或 ≥2 个同名 ⇒ 不唯一 ⇒ 空候选（交 L7/L8 聚合出歧义）。
pub fn l6_same_module(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    if site.has_receiver() || site.has_qualifier() {
        return vec![];
    }
    // 本文件 module path 推不出来（root 缺失）⇒ 空候选；同模块内 0 个或 ≥2 个同名 ⇒ 不唯一 ⇒ 空候选。
    let module = match ctx {
        Some(ctx) => ctx.module_path.clone(),
        None => return vec![],
    };
    if module.is_empty() {
        return vec![];
    }
    let hits: Vec<&FunctionTag> = index
        .by_module()
        .get(&module)
        .map(|tags| tags.iter().filter(|t| t.name == site.callee_name).collect())
        .unwrap_or_default();
    if hits.len() != 1 {
        return vec![];
    }
    hits.iter()
        .map(|tag| decorate(site, ResolutionLevel::SameModule, ResolutionReason::FoundInEnclosingScope, tag))
        .collect()
}

/// **L7 `GlobalUnique`**：全仓同名唯一。
///
/// 与 legacy 的差别只在"多候选"这一支：legacy 偷偷 `.first()` 并标 `is_resolved=true`，
/// 本层多候选时**返回空**，把候选交给 L8 全部保留（判 `Ambiguous`）。
/// 带 receiver 时候选集**收缩到成员**（`enclosing_type` 非空）：同名自由函数不是合法答案。
pub fn l7_global_unique(
    site: &CallSite,
    _ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    // 带 receiver ⇒ 候选集先**收缩到成员**（同名自由函数不是合法答案）；
    // 不带 receiver ⇒ 用全仓同名全集。两种情况下都只在"恰好 1 个"时才判定成功。
    let pool: Vec<&FunctionTag> = if site.has_receiver() {
        index.member_candidates_by_name(&site.callee_name)
    } else {
        index
            .candidates_by_name(&site.callee_name)
            .iter()
            .collect()
    };
    if pool.len() != 1 {
        return vec![];
    }
    pool.iter()
        .map(|tag| decorate(site, ResolutionLevel::GlobalUnique, ResolutionReason::UniqueNameInRepo, tag))
        .collect()
}

/// **L8 `Heuristic`**：兜底。**只排序，绝不作为唯一性来源**。
///
/// - 候选来源 = 全仓同名（带 receiver ⇒ 成员候选，再退全集以免零候选丢信息）；
/// - 无论几个候选都**原样保留**，由 `rank_candidates` 决定顺序；
///   多候选 ⇒ `Ambiguous`（P2b 的落盘形式 = 每候选一条边）；
/// - `0` 个候选 ⇒ `Unresolved`（保留现有幽灵节点行为，但必须显式标 level=Unresolved，
///   任务口径 A）。
pub fn l8_heuristic(
    site: &CallSite,
    _ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Vec<Candidate> {
    let mut pool: Vec<&FunctionTag> = index.candidates_by_name(&site.callee_name).iter().collect();
    if pool.is_empty() {
        return vec![];
    }
    if site.has_receiver() {
        let members: Vec<&FunctionTag> = pool.iter().cloned().filter(|t| t.enclosing_type.is_some()).collect();
        if !members.is_empty() {
            pool = members;
        }
    }
    // L8 的启发式只体现在排序子键上（arity / 模块距离 / 声明先后），不改候选集合本身。
    pool.iter()
        .map(|tag| decorate(site, ResolutionLevel::Heuristic, ResolutionReason::AmbiguousSameName, tag))
        .collect()
}

/// 按配置解析一个调用点（**P2b 生产路径的唯一入口候选**）。
///
/// 支配流程：按层号升序遍历启用的层，第一个产出非空候选的层接管并终止下探；
/// 全部落空 ⇒ `L8` 兜底（仍为空 ⇒ `Unresolved` + `NotFound`，与 P1 语义一致）。
///
/// `file_ctx = None`（该文件没有作用域缓存）时：需要 ctx 的 L2/L3/L4/L5/L6 一律空候选，
/// 于是退化成 P1 的"L1 + L7"口径 —— **正确性不依赖缓存是否命中**（任务口径 C），
/// 生产路径应在 P2b 用 `CodeParser::ensure_file_scope_ctx` 补齐 ctx 而不是接受降级。
pub fn resolve_call_with_config(
    call_site: &CallSite,
    file_ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
    config: &ResolutionConfig,
) -> Resolution {
    let max = if config.max_candidates == 0 {
        DEFAULT_MAX_CANDIDATES
    } else {
        config.max_candidates
    };

    // (层, 判定函数) 的遍历顺序 = 支配顺序（数值小的层级更可信，先判先接管）。
    let layers: [(ResolutionLevel, fn(&CallSite, Option<&FileScopeCtx>, &ResolutionIndex) -> Vec<Candidate>); 6] = [
        (ResolutionLevel::LocalLexical, l1_local_lexical),
        (ResolutionLevel::EnclosingTypeMember, l2_enclosing_type_member),
        (ResolutionLevel::ReceiverTypeMember, l3_receiver_type_member),
        (ResolutionLevel::ImportBinding, l4_import_binding),
        (ResolutionLevel::QualifiedPath, l5_qualified_path),
        (ResolutionLevel::SameModule, l6_same_module),
    ];

    for (level, runner) in layers.into_iter() {
        if !config.is_enabled(level) {
            continue;
        }
        let candidates = runner(call_site, file_ctx, index);
        if candidates.is_empty() {
            // 信息不足 ⇒ 该层不接管，继续下探（绝不猜）。
            continue;
        }
        return finish(level, candidates, max);
    }

    if config.is_enabled(ResolutionLevel::GlobalUnique) {
        let candidates = l7_global_unique(call_site, file_ctx, index);
        if !candidates.is_empty() {
            return finish(ResolutionLevel::GlobalUnique, candidates, max);
        }
    }

    if config.is_enabled(ResolutionLevel::Heuristic) {
        let candidates = l8_heuristic(call_site, file_ctx, index);
        if !candidates.is_empty() {
            return finish(ResolutionLevel::Heuristic, candidates, max);
        }
    }

    // 一层都没开 / 全仓查无此名 ⇒ 未解析。
    Resolution::unresolved(ResolutionReason::NotFound)
}

/// 解析一个调用点（P1 的签名，保持**逐字可用**）。
///
/// 等价于 `resolve_call_with_config(.., &ResolutionConfig::default())`：
/// 默认全层开启，但没有 ctx 时 L2..L6 自然落空 ⇒ 与 P1 的"L1+L7"结果一致。
pub fn resolve_call(
    call_site: &CallSite,
    file_ctx: Option<&FileScopeCtx>,
    index: &ResolutionIndex,
) -> Resolution {
    resolve_call_with_config(call_site, file_ctx, index, &ResolutionConfig::default())
}

// ============================================================================================
// Phase P1 的 8 条单测：**从 HEAD 原样搬回，一条不许改**（任务交付物 1 的硬要求：
// "保留 L1/L7 既有实现与测试不变"）。P2a 把 L2..L6/L8 补成真实候选来源之后，
// 这些用例仍然是绿的 —— 因为它们全部传 `file_ctx = None`，需要 ctx 的层一律空候选，
// 于是行为逐字回落到 P1 的"L1 + L7 + L8"口径。这正是"分层不破坏既有语义"的证据。
// ============================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegraph::types::FunctionInfo;
    use std::collections::HashMap;
    use std::path::Path;

    fn fid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    fn func(name: &str, file: &str, line: usize, id: u8) -> FunctionInfo {
        FunctionInfo {
            id: fid(id),
            name: name.to_string(),
            file_path: PathBuf::from(file),
            line_start: line,
            line_end: line + 2,
            namespace: String::new(),
            language: "rust".to_string(),
            signature: Some(name.to_string()),
            module_path: None,
            qualified_name: None,
        }
    }

    fn build_index(functions: &[FunctionInfo]) -> ResolutionIndex {
        ResolutionIndex::build(
            functions,
            Some(Path::new("/repo")),
            &HashMap::new(),
            &HashMap::new(),
        )
    }

    /// L7：全仓唯一同名 —— 当前 first-wins 在这一层结果相同，但这里是显式判定。
    #[test]
    fn l7_resolves_when_name_is_unique_in_repo() {
        let index = build_index(&[
            func("solo", "/repo/a.rs", 3, 1),
            func("other", "/repo/b.rs", 5, 2),
        ]);
        let site = CallSite::new(Path::new("/repo/b.rs"), 9, "solo", "rust");
        let resolution = resolve_call(&site, None, &index);
        assert_eq!(resolution.status, super::super::candidate::ResolutionStatus::Resolved);
        let primary = resolution.primary().expect("必须有候选");
        assert_eq!(primary.callee_id, fid(1));
        assert_eq!(primary.level, ResolutionLevel::GlobalUnique);
        assert_eq!(primary.confidence, 55);
        assert_eq!(primary.reason, ResolutionReason::UniqueNameInRepo);
    }

    /// L1：同文件有同名定义时**必须优先**，且层级高于 L7。
    #[test]
    fn l1_same_file_definition_wins_over_global_candidates() {
        let index = build_index(&[
            func("helper", "/repo/alpha.rs", 3, 1),
            func("helper", "/repo/beta.rs", 4, 2),
        ]);
        let site = CallSite::new(Path::new("/repo/beta.rs"), 9, "helper", "rust");
        let resolution = resolve_call(&site, None, &index);
        // 同文件只有一个 helper ⇒ 唯一确定（而不是"两个候选里猜一个"）。
        assert_eq!(resolution.status, super::super::candidate::ResolutionStatus::Resolved);
        let primary = resolution.primary().unwrap();
        assert_eq!(primary.callee_id, fid(2), "必须锁定本文件那份定义");
        assert_eq!(primary.level, ResolutionLevel::LocalLexical);
        assert_eq!(primary.confidence, 98);
    }

    /// 当前实现在这里偷偷 `.first()` 取一个；P1 数据层必须返回 Ambiguous + 全部候选。
    #[test]
    fn ambiguous_without_local_definition_keeps_every_candidate() {
        let index = build_index(&[
            func("pick", "/repo/mod_a.rs", 3, 1),
            func("pick", "/repo/mod_b.rs", 9, 2),
        ]);
        let site = CallSite::new(Path::new("/repo/qualifier.rs"), 9, "pick", "rust");
        let resolution = resolve_call(&site, None, &index);
        assert_eq!(resolution.status, super::super::candidate::ResolutionStatus::Ambiguous);
        assert_eq!(resolution.candidate_count(), 2, "两个候选都必须留住");
        assert!(
            resolution
                .candidates
                .iter()
                .all(|c| c.level == ResolutionLevel::Heuristic),
            "L2-L6 未实现时不得伪装成高可信层级"
        );
        assert!(resolution.primary().is_some(), "歧义时仍给出排序后的最优项供上层报错");
    }

    #[test]
    fn not_found_is_unresolved_with_nil_placeholder() {
        let index = build_index(&[func("solo", "/repo/a.rs", 3, 1)]);
        let site = CallSite::new(Path::new("/repo/a.rs"), 9, "ghost", "rust");
        let resolution = resolve_call(&site, None, &index);
        assert_eq!(resolution.status, super::super::candidate::ResolutionStatus::Unresolved);
        assert!(resolution.primary().is_none());
        assert_eq!(resolution.candidates[0].reason, ResolutionReason::NotFound);
        assert_eq!(resolution.candidates[0].callee_id, Uuid::nil());
    }

    #[test]
    fn same_file_multi_definitions_are_all_returned_as_lexical_ambiguity() {
        let index = build_index(&[
            func("add", "/repo/calc.rs", 4, 1),
            func("add", "/repo/calc.rs", 12, 2),
            func("add", "/repo/misc.rs", 3, 3),
        ]);
        let site = CallSite::new(Path::new("/repo/calc.rs"), 20, "add", "rust");
        let resolution = resolve_call(&site, None, &index);
        assert_eq!(resolution.candidate_count(), 2, "本文件两份定义都要留住，第三份在别处");
        assert_eq!(resolution.status, super::super::candidate::ResolutionStatus::Ambiguous);
        assert_eq!(
            resolution.candidates.iter().map(|c| c.callee_id).collect::<Vec<_>>(),
            vec![fid(1), fid(2)],
            "必须按行号确定性排序，而不是 HashMap 顺序"
        );
    }

    /// 没有作用域缓存（例如从图缓存加载、未重新解析）时也必须能工作。
    #[test]
    fn works_without_file_ctx() {
        let index = build_index(&[func("solo", "/repo/a.rs", 3, 1)]);
        let site = CallSite::new(Path::new("/repo/a.rs"), 4, "solo", "rust");
        let resolution = resolve_call(&site, None, &index);
        assert_eq!(resolution.candidate_count(), 1);
    }

    #[test]
    fn qualified_name_uses_language_separator() {
        let rust_site = CallSite::new(Path::new("/repo/q.rs"), 9, "pick", "rust")
            .with_qualified_prefix(vec!["mod_a".to_string()]);
        assert_eq!(rust_site.full_qualified_name(), "mod_a::pick");

        let py_site = CallSite::new(Path::new("/repo/q.py"), 9, "helper", "python")
            .with_qualified_prefix(vec!["mod_y".to_string()]);
        assert_eq!(py_site.full_qualified_name(), "mod_y.helper");

        // 无前缀时就是裸名（当前 AST 对 Rust `::` 的实际产出）。
        let bare = CallSite::new(Path::new("/repo/q.rs"), 9, "pick", "rust");
        assert_eq!(bare.full_qualified_name(), "pick");
    }

    #[test]
    fn builder_methods_record_receiver_and_args_and_scope() {
        let site = CallSite::new(Path::new("/repo/u.rs"), 11, "add", "rust")
            .with_receiver(Some("calc".to_string()))
            .with_arg_count(Some(1))
            .with_caller_scope(Some(fid(7)), Some(ScopeId(3)));
        assert_eq!(site.receiver.as_deref(), Some("calc"));
        assert_eq!(site.arg_count, Some(1));
        assert_eq!(site.caller_id, Some(fid(7)));
        assert_eq!(site.caller_scope, Some(ScopeId(3)));
    }
}
