//! 分层精度名称解析引擎（`codegraph::resolution`）：作用域树 + import 绑定 + 不塌缩索引 + L0–L8 分层判定。
//!
//! # 这个模块是什么
//! 它把"callee 到底指向哪个函数"从"名字相等就取第一个"升级为**按层判定、歧义保留**。
//! P0 量化出缺陷面（4 组同名歧义名共 13 个候选、只体现为 11 条已解析边）⇒ P1 做实原料与数据结构 ⇒
//! P2/P3 接入产边链路：改造**已全部交付**，权威口径见 `docs/call-graph-resolution.md`。
//!
//! | 文件 | 职责 |
//! |---|---|
//! | `level` | 9 级 `ResolutionLevel`（`#[repr(u8)]`，已落盘为 `CallRelation::resolution_level`）+ `ResolutionReason` |
//! | `candidate` | `Candidate` / `Resolution` / `ResolutionStatus` + 状态判定纯函数 |
//! | `scope` | `ScopeTree` / `ScopeBuilder`：复用 `parent_guid`/`childs_guid` 建作用域树，提供 `innermost_scope_containing` |
//! | `imports` | `ImportMap` / `ImportEntry`：把 `ImportDeclaration` 的原料忠实搬出来（7 语言形态各有单测） |
//! | `index` | `ResolutionIndex`：`by_name: HashMap<String, Vec<FunctionTag>>` —— **不塌缩**的全仓函数索引 |
//! | `resolver` | `CallSite` / `resolve_call`：**L0–L8 全部实现**。支配规则淘汰（按层升序，第一个产出非空候选的层接管并终止下探），同层排序不淘汰；≥2 候选保持 `Ambiguous` |
//!
//! # 硬约束（历史 → 当前）
//! - **只建不消费**（Phase P1 的历史红线，**已失效**）：当时 `resolve_call` 没接进任何建图路径，
//!   四处同名 first-wins 与 `storage/incremental.rs` 的重复副本一字未改。**当前**：legacy 全量 /
//!   legacy 刷新 / layered 全量 / layered 刷新（增量链路复用最后一项）四条产边路径全部收敛到唯一
//!   写入口 `PetCodeGraph::upsert_call_site_edges`，`incremental.rs` 的重复副本已删除；legacy 分支
//!   （`--resolver=legacy`）有意保留作回滚逃生口，数值红线不变 —— 这就是代码里仍有其副本的原因。
//! - 纯函数、无 I/O：只有 `ScopeBuilder::build` / `ImportMap::build` 读取已经解析好的
//!   `Vec<AstSymbolInstanceArc>`，不读文件、不碰数据库、不建图（落边在 `emit_layered_resolution`）。
//! - 无新依赖：只用 `std` + `uuid`。**禁止引入 `stack-graphs`**（已被 GitHub 归档，
//!   挂 RUSTSEC-2026-0303）；本模块只借它的分层模型。
//!
//! # 与 `CodeParser` 的关系
//! `CodeParser::file_scopes` 由 `CodeParser::_extract_entities_from_file` 与 `CodeParser::parse_file`
//! 两处符号消费循环产出，经 `file_scope_ctx` / `ensure_file_scope_ctx` 供本模块的 `resolve_call` 消费。

pub mod candidate;
pub mod imports;
pub mod index;
pub mod level;
pub mod resolver;
pub mod scope;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::codegraph::types::FunctionInfo;
use crate::codegraph::treesitter::{AstSymbolInstanceArc, structs::SymbolType};

pub use candidate::{
    dedup_by_callee, rank_candidates, status_of, Candidate, Resolution, ResolutionStatus,
};
pub use imports::{ImportEntry, ImportMap, LOCAL_NAME_RULES};
// Phase P2a：索引的新入口（`build_resolution_index_from_graph` 是交付物 2(c) 的唯一构造点）。
pub use index::{
    build_resolution_index_from_graph, declared_before_call_site, module_distance_between,
    CallableFact, FunctionTag, ResolutionIndex,
};
pub use level::{ResolutionLevel, ResolutionReason};
// Phase P2a：`ResolutionConfig`（按层关闭诊断）与 CallSite 构造入口。
pub use resolver::{
    call_sites_from_symbols, call_sites_from_symbols_with_source, resolve_call,
    resolve_call_with_config, CallSite, ResolutionConfig,
};
pub use scope::{
    Scope, ScopeBuilder, ScopeId, ScopeItem, ScopeKind, ScopeSegment, ScopeTree,
};

/// 单个文件的解析上下文：作用域树 + import 绑定表 + 本文件 module path。
///
/// 这是 P2 的 `resolve_call` 需要的全部"文件内"信息，一次构建、随文件生命周期缓存。
#[derive(Debug, Clone)]
pub struct FileScopeCtx {
    /// 所属文件。
    pub file_path: PathBuf,
    /// 该文件的 module path（推导失败时为空串，见 [`index::derive_module_path`]）。
    pub module_path: String,
    /// 作用域树（`ScopeBuilder::build` 产出）。
    pub scopes: ScopeTree,
    /// import 绑定表（`ImportMap::build` 产出）。
    pub imports: ImportMap,
    /// 语言级包声明（目前只有 Java 的 `package a.b.c;` 会有值）。
    ///
    /// 数据来源：`TreeSitterParser::parse_file_with_metadata`（P1 交付物 2(b)）返回的
    /// `ParsedFileAst::declared_package`，由 `CodeParser` 的两处解析点透传进来 ——
    /// 因此 `CodeParser::file_scopes` 里的 Java ctx **不再** 是 `None`。
    /// 只有显式调用 [`build_file_scope_ctx`] 并传 `None` 时才是 `None`（单测路径）。
    pub declared_package: Option<Vec<String>>,
    /// Phase P2a 交付物 1：本文件内**可静态推断类型**的绑定（L3 的唯一信息来源）。
    ///
    /// 只从已解析出的符号计算（`param_bindings()` / `types()`），不做任何 I/O，
    /// 也**只覆盖任务允许的三种推断形式**（形参标注、`let v = T::new(..)`、`new T(..)`）；
    /// 推断不出来 ⇒ 该 receiver 没有条目 ⇒ L3 返回空候选并下沉，绝不猜。
    pub typed_bindings: Vec<TypedBinding>,
}

/// Phase P2a：receiver 类型推断的**允许来源**（口径来自任务的分层规则 L3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingVia {
    /// 函数形参 `v: T` / `T v`（来自 AST getter `param_bindings`）。
    Param,
    /// `let v: T = ...` 显式类型标注（仅当 AST 记录了标注时）。
    LetAnnotation,
    /// `let v = T::new(...)` / `let v = new T(...)` 构造表达式。
    ConstructorCall,
}

/// Phase P2a：一条"名字 → 类型"的可见绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedBinding {
    /// 绑定名（形参名 / `let` 变量名）。
    pub name: String,
    /// 推断出的类型名（规范化成单个标识符，如 `&Calc` → `Calc`）。
    pub type_name: String,
    /// 推断来源（诊断用：报告里能说清"凭什么认为它是 Calc"）。
    pub via: BindingVia,
    /// 生效区间起（1-based，含）。
    pub scope_start: usize,
    /// 生效区间止（1-based，含）。
    pub scope_end: usize,
    /// 绑定自身的声明行（遮蔽时取更晚的那条）。
    pub decl_line: usize,
    /// 来源符号 guid（确定性 tie-break 用）。
    pub guid: Uuid,
}

impl TypedBinding {
    /// 可见区间宽度（用于"取最内层绑定"）。
    pub fn span(&self) -> usize {
        self.scope_end.saturating_sub(self.scope_start)
    }
}

impl FileScopeCtx {
    /// 显式构造（用于已知 `declared_package` 的场合，以及单测）。
    pub fn new(
        file_path: &Path,
        symbols: &[AstSymbolInstanceArc],
        root: Option<&Path>,
        declared_package: Option<Vec<String>>,
        language: &str,
    ) -> Self {
        let scopes = ScopeBuilder::build(file_path, symbols);
        let imports = ImportMap::build(symbols);
        let module_path =
            index::derive_module_path(file_path, root, language, declared_package.as_deref());
        let typed_bindings = collect_typed_bindings(symbols, &scopes);
        Self {
            file_path: file_path.to_path_buf(),
            module_path,
            scopes,
            imports,
            declared_package,
            typed_bindings,
        }
    }

    /// Phase P2a 交付物 1：`line` 处看到 `receiver` 这个绑定是什么类型。
    ///
    /// 选择规则（**遮蔽语义**）：在所有"名字相同 + 区间覆盖该行"的绑定里，
    /// 取可见区间最窄（最内层）的一条；同宽度取声明行更晚的一条（后声明遮蔽先声明）；
    /// 再同按 guid 升序 —— 与符号产出顺序、HashMap 种子都无关。
    /// 找不到 ⇒ `None`，L3 必须返回空候选并下沉。
    pub fn receiver_type_at(&self, line: usize, receiver: &str) -> Option<&TypedBinding> {
        if receiver.is_empty() {
            return None;
        }
        self.typed_bindings
            .iter()
            .filter(|binding| {
                binding.name == receiver
                    && binding.scope_start <= line
                    && line <= binding.scope_end
            })
            .min_by(|a, b| {
                a.span()
                    .cmp(&b.span())
                    .then_with(|| b.decl_line.cmp(&a.decl_line))
                    // Phase P3 收口 C：末位改用源码派生字段（`guid` 每次建图都是新 Uuid ⇒
                    // 只保证同进程可复现，不保证同一份源码两次独立建图可复现）。
                    .then_with(|| {
                        (&a.type_name, a.scope_start, a.scope_end).cmp(&(
                            &b.type_name,
                            b.scope_start,
                            b.scope_end,
                        ))
                    })
            })
    }

    /// Phase P2a 交付物 1：`line` 处的**外围类型名**（L2 `EnclosingTypeMember` 判据）。
    ///
    /// 顶层自由函数/模块级语句 ⇒ `None`（没有外围类型，L2 必须下沉）。
    pub fn enclosing_type_at(&self, line: usize) -> Option<String> {
        let scope = self.innermost_scope_containing(line)?;
        self.scopes.enclosing_type_name(scope)
    }

    /// Phase P2a 交付物 1：`line` 处所在的最内层**函数**作用域名。
    ///
    /// 用途：给 harness 证明 caller 归属由 `innermost_scope_containing` 得出
    /// （`nested_function_caller` 场景里必须是 `inner`，而不是靠符号产出顺序巧合拿到的那个）。
    /// 落在文件根（没有函数包住这一行）时返回 `None`。
    pub fn enclosing_function_at(&self, line: usize) -> Option<String> {
        let id = self.innermost_scope_containing(line)?;
        let scope = self.scopes.scope(id)?;
        match scope.kind {
            ScopeKind::Function | ScopeKind::Closure => Some(scope.name.clone()),
            _ => None,
        }
    }

    /// 调用点在 `line` 处的最内层作用域。
    ///
    /// P2 用它替掉 `_find_caller_function_by_line`（`parser.rs:1223`）的行区间 first-wins。
    pub fn innermost_scope_containing(&self, line: usize) -> Option<ScopeId> {
        self.scopes.innermost_scope_containing(line)
    }

    /// 抽取"可调用事实"（名字 + 起始行 + 形参数 + 外围类型），供
    /// [`ResolutionIndex::build`] 用 `(file, line_start)` 回填 `FunctionTag.param_count`。
    ///
    /// 之所以要靠 `(file, line_start)` 而不是 id：`FunctionInfo.id` 是 `parser.rs` 里现场
    /// `Uuid::new_v4()` 生成的，与 AST `guid` 无任何对应关系（两处的 `line_start` 都等于
    /// `full_range.start_point.row + 1`，口径一致才敢这样关联）。
    pub fn callable_facts(&self) -> Vec<CallableFact> {
        self.scopes
            .callable_scopes()
            .into_iter()
            .filter_map(|id| {
                self.scopes.scope(id).map(|scope| {
                    // Phase P2a：与 L2 用同一个口径（`ScopeTree::enclosing_type_name` = 链上
                    // **最内层**的 Type），否则"最内层遮蔽外层"和索引进来的是外层类型会互相打架。
                    let enclosing_type = self.scopes.enclosing_type_name(id);
                    CallableFact {
                        name: scope.name.clone(),
                        line_start: scope.line_start,
                        param_count: scope.param_count,
                        enclosing_type,
                        // Phase P2a：抽象签名（trait 方法）标记随作用域一起搬进索引，
                        // L3 命中唯一一条时必须据它拒绝"已解析"。
                        has_body: scope.has_body,
                    }
                })
            })
            .collect()
    }

    /// 本文件 import 引入的本地绑定名集合（P2 判断"这个名字是不是外来名"）。
    pub fn imported_bindings(&self) -> Vec<String> {
        self.imports.bound_names()
    }
}

/// 把"符号列表 → FileScopeCtx"的构造收敛成一个函数，供 `CodeParser` 的两处消费点调用。
///
/// 这个函数**只读符号**：不修改、不增删任何 `AstSymbolInstanceArc`，
/// 也不返回任何会影响建图的数据。
///
/// `declared_package` 来自 `TreeSitterParser::parse_file_with_metadata`
/// （P1 交付物 2(b)：Java 的 `package a.b.c;` 过去被 `java.rs` 的空分支彻底丢弃）。
/// 它只影响 [`FileScopeCtx::module_path`] 的推导（`index::derive_module_path` 以包名为准），
/// 而 module path 目前**没有任何建图消费者**，所以调用边语义不变。
pub fn build_file_scope_ctx(
    file_path: &Path,
    symbols: &[AstSymbolInstanceArc],
    root: Option<&Path>,
    language: &str,
    declared_package: Option<Vec<String>>,
) -> Arc<FileScopeCtx> {
    Arc::new(FileScopeCtx::new(
        file_path,
        symbols,
        root,
        declared_package,
        language,
    ))
}

/// Phase P2a 交付物 1：从**已解析符号**收集"可静态推断类型"的绑定。
///
/// 只允许任务 L3 规定的三种形式，且**一律不猜**：
/// 1. **函数形参 `v: T` / `T v`** —— 走 AST getter `param_bindings()`，
///    可见区间 = 该函数声明的 `full_range`（形参在整个函数体内可见）。
/// 2. **`let v: T = ...` 显式标注** —— 只有当 AST 真的记录了标注类型才算
///    （实测：`rust.rs` 的 `VariableDefinition.type_` 是从**右值**推导的，
///     `let v: Calc = c` 得到的是 `name=None, inference_info=Some("c")`，
///     因此这一支目前对 Rust 基本拿不到信息 —— 这正是"信息不足即下沉"）。
/// 3. **`let v = T::new(..)` / `let v = new T(..)`** —— 从 `type_.inference_info`
///    的右值文本里**精确**识别这两种构造形态（别的构造写法一律不认）。
///
/// `let` 绑定的可见区间 = 声明行的下一行起，到**最内层包住它的作用域**结束为止
/// （用现成的 `ScopeTree`，因此遮蔽语义天然正确：内层块更窄 ⇒ `receiver_type_at` 优先取它）。
///
/// 输出排序：`(name, scope_start, scope_end, decl_line, guid)`，与符号产出顺序无关。
fn collect_typed_bindings(symbols: &[AstSymbolInstanceArc], scopes: &ScopeTree) -> Vec<TypedBinding> {
    let mut out: Vec<TypedBinding> = Vec::new();
    // 文件根的作用域区间（给"没有任何函数包住"的顶层 `let` 兜底）。
    let root_span = scopes
        .scope(scopes.root())
        .map(|root| (root.line_start, root.line_end))
        .unwrap_or((1, 1));

    for symbol in symbols {
        let guard = symbol.read();
        let symbol_ref = guard.as_ref();
        let guid = *symbol_ref.guid();
        match symbol_ref.symbol_type() {
            SymbolType::FunctionDeclaration => {
                let line_start = symbol_ref.full_range().start_point.row + 1;
                let line_end = symbol_ref.full_range().end_point.row + 1;
                for (name, type_name) in symbol_ref.param_bindings() {
                    out.push(TypedBinding {
                        name,
                        type_name,
                        via: BindingVia::Param,
                        scope_start: line_start,
                        scope_end: line_end,
                        decl_line: line_start,
                        guid,
                    });
                }
            }
            SymbolType::VariableDefinition => {
                let decl_line = symbol_ref.full_range().end_point.row + 1;
                let name = symbol_ref.name().to_string();
                if name.is_empty() {
                    continue;
                }
                // types()[0] 就是 `VariableDefinition.type_`（见 ast_instance_structs.rs）。
                let first = symbol_ref.types().into_iter().next();
                let (type_name, via) = match first.as_ref() {
                    // (2) 显式标注
                    Some(t) if t.binding_type_name().is_some() => (
                        t.binding_type_name().unwrap_or("").to_string(),
                        BindingVia::LetAnnotation,
                    ),
                    // (3) 构造表达式
                    Some(t) => match constructor_type_from_text(
                        t.inference_info.as_deref().unwrap_or_default(),
                    ) {
                        Some(parsed) => (parsed, BindingVia::ConstructorCall),
                        None => continue,
                    },
                    None => continue,
                };
                if type_name.is_empty() {
                    continue;
                }
                // 可见区间：声明行之后 ⇒ 最内层包住该行的作用域末尾。
                let (scope_start, scope_end) = scopes
                    .innermost_scope_containing(decl_line)
                    .and_then(|id| scopes.scope(id))
                    .filter(|scope| scope.kind != ScopeKind::File)
                    .map(|scope| (decl_line + 1, scope.line_end))
                    .unwrap_or((decl_line + 1, root_span.1));
                if scope_start > scope_end {
                    continue;
                }
                out.push(TypedBinding {
                    name,
                    type_name,
                    via,
                    scope_start,
                    scope_end,
                    decl_line,
                    guid,
                });
            }
            _ => {}
        }
    }

    out.sort_by(|a, b| {
        (
            &a.name,
            a.scope_start,
            a.scope_end,
            a.decl_line,
            // Phase P3 收口 C：末位用源码派生字段（`type_name`）而不是 `guid`——
            // guid 每次建图都是新 Uuid，用它补位等于把"同一份源码两次独立建图"
            // 的绑定顺序交给随机数。
            &a.type_name,
        )
            .cmp(&(
                &b.name,
                b.scope_start,
                b.scope_end,
                b.decl_line,
                &b.type_name,
            ))
    });
    out
}

/// Phase P2a：从 `let` 的右值文本里识别 **`T::new(..)` / `new T(..)`** 两种构造形态。
///
/// 刻意只认这两种（任务 L3 的原文口径），其它（`T::default()`、`T{..}`、工厂函数）
/// 一律返回 `None` ⇒ 上层下沉。**不做任何"看起来像构造"的启发式**。
pub fn constructor_type_from_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // `T::new(...)` / `a::T::new(...)`：取 `::new` 之前的最后一个路径段。
    if let Some(head) = text.strip_suffix("::new()").or_else(|| {
        text.find("::new(").map(|pos| &text[..pos])
    }) {
        let head = head.trim();
        let last = head.rsplit([':', '.']).next().unwrap_or("").trim();
        if is_type_ident(last) {
            return Some(last.to_string());
        }
        return None;
    }
    // `new T(...)` / `new T{...}`
    if let Some(rest) = text.strip_prefix("new ") {
        let rest = rest.trim();
        let head: String = rest
            .chars()
            .take_while(|c| !matches!(c, '(' | '{' | '<'))
            .collect();
        let head = head.trim();
        if is_type_ident(head) {
            return Some(head.to_string());
        }
    }
    None
}

/// 是否像一个类型标识符（首字母大写、只含字母数字下划线）。
fn is_type_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() => {}
        _ => return false,
    }
    s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ============================================================================================
// Phase P2b 交付物 2：接线用的两个共用件。
//
// 放在 `resolution` 里而不是 `parser.rs`，原因只有一个：**全量、刷新（CodeParser
// 的 per-file 链路）、增量（`storage::IncrementalManager`）三条链路必须跑同一段
// 判定代码**，否则"全量 vs 增量一致"这个 P2b 硬验收只能靠人肉对齐两份实现 ——
// 而那正是 P0/P1 记录在案的"三套并行 first-wins 副本"缺陷的复发方式。
// P3 删除重复实现时，这两个函数就是唯一留下的实现。
// ============================================================================================

/// Phase P2b：caller 归属（任务交付物 2 的要求 (iii)）。
///
/// 有 ctx 时：`ScopeTree::innermost_scope_containing(line)` 给出**最内层**作用域，
/// 再沿作用域链**由内向外**回退，取第一个能与图节点对齐的可调用作用域。
/// 这一步替掉了 legacy 的"行区间 first-wins"（`_find_caller_function_by_line`），
/// 是闭包/嵌套函数不再被误归外层的唯一手段。
///
/// 对齐键：`(name, line_start)`。不能用 id —— `FunctionInfo.id` 是建图时现场
/// `Uuid::new_v4()`，与 AST `guid` 无对应关系（任务红线 5 禁止改行号口径）。
/// 同键多节点按 id 升序取第一个，保证跨进程确定性。
///
/// 没有 ctx 时（该文件本轮没被解析过）：**不退化成 first-wins**，而是用纯图事实
/// 做同一条"最内层优先"规则 —— 在所有覆盖该行的候选里取行区间最窄者，同宽取
/// `line_start` 更晚者，再同按 id 升序。结果是"更保守"而不是"更错"。
pub fn layered_caller_node(
    site: &CallSite,
    ctx: Option<&FileScopeCtx>,
    funcs: &[FunctionInfo],
) -> Option<FunctionInfo> {
    if let Some(ctx) = ctx {
        if let Some(innermost) = ctx.innermost_scope_containing(site.line) {
            // `scope_chain` 是"外 → 内"，caller 要"内 → 外"，故 reverse。
            for scope_id in ctx.scopes.scope_chain(innermost).into_iter().rev() {
                let scope = ctx.scopes.scope(scope_id)?;
                if !matches!(scope.kind, ScopeKind::Function | ScopeKind::Closure) {
                    continue;
                }
                if scope.name.is_empty() {
                    continue;
                }
                if let Some(node) = funcs
                    .iter()
                    .filter(|f| f.name == scope.name && f.line_start == scope.line_start)
                    // Phase P3 收口 C：tie-break 换成源码派生字段。`id` 是每次建图重新生成的
                    // Uuid ⇒ 同名同起始行的两个函数（压缩 JS 里整仓都在第 2 行，极常见）
                    // 会在两次独立建图里被选成不同的 caller，caller 的文件一变，
                    // L1（同文件词法命中）/ L7（全仓同名唯一）就跟着翻转。
                    .min_by(|a, b| {
                        (
                            &a.file_path,
                            a.line_end,
                            &a.namespace,
                            &a.qualified_name,
                        )
                            .cmp(&(&b.file_path, b.line_end, &b.namespace, &b.qualified_name))
                    })
                {
                    return Some(node.clone());
                }
            }
        }
    }
    // 兜底路径：只用图节点自身携带的行区间，规则同上（最内层优先，绝不 first-wins）。
    funcs
        .iter()
        .filter(|f| f.file_path == site.file && site.line >= f.line_start && site.line <= f.line_end)
        .min_by(|a, b| {
            let span_a = a.line_end.saturating_sub(a.line_start);
            let span_b = b.line_end.saturating_sub(b.line_start);
            span_a
                .cmp(&span_b)
                .then_with(|| b.line_start.cmp(&a.line_start))
                // Phase P3 收口 C：末位用源码派生字段代替 `id`（每次建图都是新 Uuid）。
                .then_with(|| {
                    (&a.file_path, &a.name, a.line_end, &a.namespace)
                        .cmp(&(&b.file_path, &b.name, b.line_end, &b.namespace))
                })
        })
        .cloned()
}

/// Phase P2b：把一个 `Resolution` 落成调用边（口径 A + 口径 B + 要求 (v)）。
///
/// - **口径 A**：每个候选一条边，最多 `max_candidates` 条；超出上限的候选丢弃但
///   仍计入 `candidate_count`，且 `is_resolved` 的判定用**截断前**的真实候选数
///   （所以 `--max-candidates=1` 只是少写几条边，绝不会把歧义伪装成唯一）。
/// - **口径 B（覆盖中性）**：`Unresolved` ⇒ 合成与 legacy `_create_unresolved_call_relation`
///   **逐字段同形**的幽灵节点 + 幽灵边（`level=0`、`confidence=0`、`candidate_count=0`），
///   因此同一份源码下"有边的调用点数"两种模式必然相等，layered 不会净减少调用边。
/// - **要求 (v)**：写边之前先删该 `(caller, line)` 的旧边，禁止叠边。
///
/// 返回本次实际写出的边数（诊断与统计用）。
pub fn emit_layered_resolution(
    site: &CallSite,
    caller: &FunctionInfo,
    resolution: &Resolution,
    code_graph: &mut crate::codegraph::types::PetCodeGraph,
    max_candidates: usize,
) -> usize {
    use crate::codegraph::types::{CallRelation, FunctionInfo as GFunctionInfo};

    // 要求 (v) + 任务口径 B：**发射必须幂等**。本函数不再自己"先删后写"，
    // 而是把整个调用点的新候选一次性交给 [`crate::codegraph::types::PetCodeGraph::upsert_call_site_edges`]
    // —— 那是 legacy / layered、全量 / 增量四条产边路径**共用**的唯一边写入入口
    // （它内部先删同调用点的旧边再写新候选，删除范围严格限定在
    // `(caller, line, callee 名族)`；按 `(caller, line)` 全删会抹掉同一行其它调用点，
    // P2b 实测踩过：`simple_js_project` 边数 746 → 52）。
    // 截断前的真实候选数（u8 饱和到 255）。
    let total = resolution.total_candidate_count.min(u8::MAX as usize) as u8;

    if resolution.status == ResolutionStatus::Unresolved || total == 0 {
        // 口径 B：幽灵节点走 `FunctionInfo::legacy` —— `module_path`/`qualified_name`
        // 保持 `None`，与 legacy 的幽灵节点逐字段一致（幽灵不是源码里的定义，没有身份可言）。
        //
        // **先查再建**：同一个调用点如果已经有幽灵节点就复用。
        // 不做复用的后果是实测出来的：`add_call_relation` 按 `(caller, callee, line)`
        // 去重，第二条边被静默丢弃，而它对应的幽灵节点已经进了图 ⇒ 出现
        // "节点数暴涨、边数却不涨"的孤儿节点（`simple_js_project` 1929 个幽灵 / 797 条未解析边）。
        //
        // Phase P3 交付物 2：**合并粒度与 legacy 的有效行为对齐为「同裸名复用」，忽略行号**。
        // 改造前这里还要求 `line_start == line_end == site.line`，于是同一个文件里同一个裸名
        // 在不同行各造一个幽灵 ⇒ layered 的幽灵节点数系统性高于 legacy（P2b 收口实测整仓：
        // rust 46→49、python 45→55、ts 92→121、js 1577→1607）。
        // legacy 之所以"更少"，是因为它的全局 `find_functions_by_name()` 会命中早先造出的
        // 同名幽灵并跨行/跨文件复用。本轮先按「同文件 + 同裸名」对齐（下面这条谓词），
        // 实测仍高于 legacy ⇒ 进一步放宽到「全仓同裸名」（只按裸名 + `namespace == "unresolved"`
        // 复用，忽略行号与文件）。完整口径见 `docs/call-graph-resolution.md`「幽灵节点规则」一节。
        // ⚠ 关键取舍：幽灵**节点**可以全仓共用一个，但幽灵**边**的 `callee_file`
        //   一律写调用点自己的文件（下面 `CallRelation::layered_ghost` 传的是 `site.file`），
        //   因此这次合并不引入任何假的跨文件边 —— 这正是 legacy 靠"跨文件复用同名幽灵"
        //   压低节点数时付出的代价，layered 不付这个代价。
        let existing_ghost = code_graph
            .find_functions_by_name(&site.callee_name)
            .into_iter()
            .find(|f| f.namespace == "unresolved")
            .map(|f| f.id);
        let ghost_id = match existing_ghost {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4();
                code_graph.add_function(GFunctionInfo::legacy(
                    id,
                    site.callee_name.clone(),
                    site.file.clone(),
                    site.line,
                    site.line,
                    "unresolved".to_string(),
                    caller.language.clone(),
                    Some(format!("unresolved_call_{}", site.callee_name)),
                ));
                id
            }
        };
        let relation = CallRelation::layered_ghost(
            caller.id,
            ghost_id,
            caller.name.clone(),
            site.callee_name.clone(),
            caller.file_path.clone(),
            site.file.clone(),
            site.line,
        );
        return code_graph.upsert_call_site_edges(
            &caller.id,
            site.line,
            &site.callee_name,
            vec![relation],
        );
    }

    // 口径 A：每个候选一条边（截断上限由 `--max-candidates` 决定），整个调用点一次性发射，
    // 使得"重写 N 次"与"写 1 次"的图状态完全一致。
    let mut relations: Vec<CallRelation> = Vec::new();
    for candidate in resolution.candidates.iter().take(max_candidates.max(1)) {
        // 候选必须真的是图里的节点（索引本就来自图，理论恒成立）。拿不到就跳过并告警，
        // **绝不新建假节点** —— 那会把"引擎认为存在"变成"图里凭空多一个函数"。
        let Some(callee) = code_graph.get_function_by_id(&candidate.callee_id).cloned() else {
            tracing::warn!(
                "layered 解析出的候选不在图里：{} @ {}:{}",
                candidate.callee_id,
                site.file.display(),
                site.line
            );
            continue;
        };
        let relation = CallRelation::layered(
            caller.id,
            callee.id,
            caller.name.clone(),
            callee.name.clone(),
            caller.file_path.clone(),
            callee.file_path.clone(),
            site.line,
            candidate.level.as_u8(),
            candidate.confidence,
            candidate.rank,
            total,
        );
        relations.push(relation);
    }
    code_graph.upsert_call_site_edges(&caller.id, site.line, &site.callee_name, relations)
}

/// 全仓解析上下文：`ResolutionIndex` + 每文件 ctx。
///
/// P2 建图时用它替代"每次调用都线性扫全图"的做法。P1 里它只被单测与守卫测试构造，
/// 没有任何生产调用点。
#[derive(Debug, Clone, Default)]
pub struct ResolutionContext {
    index: ResolutionIndex,
    per_file: HashMap<PathBuf, Arc<FileScopeCtx>>,
    root: Option<PathBuf>,
}

impl ResolutionContext {
    /// 由"已缓存的文件 ctx" + "全仓函数节点"组装。
    pub fn new(
        per_file: HashMap<PathBuf, Arc<FileScopeCtx>>,
        functions: &[FunctionInfo],
        root: Option<&Path>,
    ) -> Self {
        // 把每个文件的 AST 可调用事实汇总，用于回填 param_count。
        let mut callables: HashMap<PathBuf, Vec<CallableFact>> = HashMap::new();
        let mut packages: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for (path, ctx) in per_file.iter() {
            callables.insert(path.clone(), ctx.callable_facts());
            if let Some(pkg) = ctx.declared_package.as_ref() {
                packages.insert(path.clone(), pkg.clone());
            }
        }
        let index = ResolutionIndex::build(functions, root, &packages, &callables);
        Self {
            index,
            per_file,
            root: root.map(|p| p.to_path_buf()),
        }
    }

    pub fn index(&self) -> &ResolutionIndex {
        &self.index
    }

    pub fn file_ctx(&self, path: &Path) -> Option<&Arc<FileScopeCtx>> {
        self.per_file.get(path)
    }

    pub fn per_file(&self) -> &HashMap<PathBuf, Arc<FileScopeCtx>> {
        &self.per_file
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// 解析一个调用点（P2 的唯一入口；P1 只保证可用，不接生产路径）。
    pub fn resolve(&self, call_site: &CallSite) -> Resolution {
        resolve_call(
            call_site,
            self.file_ctx(&call_site.file).map(Arc::as_ref),
            &self.index,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegraph::treesitter::language_id::LanguageId;
    use crate::codegraph::treesitter::parsers::get_ast_parser;
    use uuid::Uuid;

    fn parse_source(lang: LanguageId, code: &str, filename: &str) -> Vec<AstSymbolInstanceArc> {
        let mut parser = get_ast_parser(lang).expect("get_ast_parser");
        parser.parse(code, &PathBuf::from(filename))
    }

    /// `CodeParser::file_scopes` 的真实产出路径：走 `build_file_scope_ctx`，
    /// 并且证明"建了但没消费"—— 它不参与 `FunctionInfo` 的生成。
    #[test]
    fn build_file_scope_ctx_from_real_rust_source() {
        let source = "use std::fmt;\n\
                      pub fn outer(v: i32) -> i32 { inner(v) }\n\
                      pub fn inner(v: i32) -> i32 { helper(v) }\n\
                      fn helper(v: i32) -> i32 { v + 1 }\n";
        let symbols = parse_source(LanguageId::Rust, source, "mod_ctx.rs");
        let ctx = build_file_scope_ctx(
            Path::new("/repo/src/mod_ctx.rs"),
            &symbols,
            Some(Path::new("/repo")),
            "rust",
            None,
        );

        assert_eq!(ctx.file_path.to_string_lossy(), "/repo/src/mod_ctx.rs");
        assert_eq!(ctx.module_path, "src::mod_ctx", "Rust module path 用 `::`");
        assert!(!ctx.scopes.is_empty(), "作用域树必须有内容");
        // 交付物 2(b) 的通路：显式传 `None` 时 package 不参与 module path（真实 Java 用例见
        // `java_package_flows_from_parse_file_with_metadata_into_module_path`）。
        assert_eq!(ctx.declared_package, None);
        // import 原料被真的搬出来了（std::fmt 一条）。
        assert!(
            ctx.imports.entries().iter().any(|e| e.module_path == vec![
                "std".to_string(),
                "fmt".to_string()
            ]),
            "实测 = {:?}",
            ctx.imports.entries()
        );
    }

    #[test]
    fn callable_facts_carry_param_counts_and_enclosing_types() {
        let source = "struct Calc;\nimpl Calc { fn add(a: i32, b: i32) -> i32 { a + b } }\n";
        let symbols = parse_source(LanguageId::Rust, source, "calc.rs");
        let ctx = FileScopeCtx::new(
            Path::new("/repo/calc.rs"),
            &symbols,
            Some(Path::new("/repo")),
            None,
            "rust",
        );
        let facts = ctx.callable_facts();
        let add = facts
            .iter()
            .find(|f| f.name == "add")
            .expect("add 应是可调用事实");
        assert_eq!(add.param_count, Some(2), "trait::param_count 必须真的传到 CallableFact");
        assert!(add.line_start >= 1);
    }

    #[test]
    fn resolution_context_assembles_index_and_resolves() {
        let source = "fn target(x: i32) -> i32 { x }\nfn caller() { target(1) }\n";
        let symbols = parse_source(LanguageId::Rust, source, "app.rs");
        let file = PathBuf::from("/repo/app.rs");
        let ctx = build_file_scope_ctx(&file, &symbols, Some(Path::new("/repo")), "rust", None);

        let mut per_file: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
        per_file.insert(file.clone(), ctx);

        let functions: Vec<FunctionInfo> = per_file
            .values()
            .flat_map(|c| {
                c.scopes
                    .callable_scopes()
                    .into_iter()
                    .filter_map(|id| c.scopes.scope(id))
                    .map(|scope| FunctionInfo {
                        id: scope.guid,
                        name: scope.name.clone(),
                        file_path: c.file_path.clone(),
                        line_start: scope.line_start,
                        line_end: scope.line_end,
                        namespace: c.module_path.clone(),
                        language: "rust".to_string(),
                        signature: Some(scope.name.clone()),
                        module_path: None,
                        qualified_name: None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        let resolution_ctx = ResolutionContext::new(per_file, &functions, Some(Path::new("/repo")));
        assert_eq!(resolution_ctx.index().len(), functions.len());

        // 同文件调用 → L1 命中。
        let site = CallSite::new(&file, 2, "target", "rust");
        let resolution = resolution_ctx.resolve(&site);
        assert_eq!(resolution.status, ResolutionStatus::Resolved);
        assert_eq!(resolution.primary().unwrap().level, ResolutionLevel::LocalLexical);

        // 未知名字 → Unresolved。
        let missing = resolution_ctx.resolve(&CallSite::new(&file, 2, "nope", "rust"));
        assert_eq!(missing.status, ResolutionStatus::Unresolved);
    }

    #[test]
    fn file_ctx_reports_module_path_and_imported_bindings() {
        let source = "use a::b::{c, d as e};\nfn user() { c(); }\n";
        let symbols = parse_source(LanguageId::Rust, source, "u.rs");
        let ctx = build_file_scope_ctx(Path::new("/repo/u.rs"), &symbols, Some(Path::new("/repo")), "rust", None);
        assert_eq!(ctx.module_path, "u");
        assert_eq!(ctx.imported_bindings(), vec!["c".to_string(), "e".to_string()]);
        assert_eq!(ctx.innermost_scope_containing(2).is_some(), true);
    }

    /// Java 的 package 只有显式传入时才会参与 module path（P1b 的接入点，见字段 TODO）。
    #[test]
    fn declared_package_is_used_when_supplied() {
        let symbols = parse_source(
            LanguageId::Java,
            "package com.example;\npublic class Runner { public void run() {} }\n",
            "Runner.java",
        );
        let ctx = FileScopeCtx::new(
            Path::new("/repo/src/main/java/whatever/Runner.java"),
            &symbols,
            Some(Path::new("/repo")),
            Some(vec!["com".to_string(), "example".to_string()]),
            "java",
        );
        assert_eq!(ctx.module_path, "com.example");
        assert_eq!(
            ctx.declared_package.as_deref(),
            Some(&["com".to_string(), "example".to_string()][..])
        );
    }

    /// 交付物 2(b) → 交付物 3 的**端到端真实通路**：
    /// 落一个 `package com.example;` 的 Java 文件，走 `TreeSitterParser::parse_file_with_metadata`
    /// （`CodeParser` 的两处解析点用的就是它），证明包名从 AST 一路走到 `module_path`。
    /// 改造前 `java.rs` 的 `package_declaration` 是空分支，这条链路上任何一环都拿不到包名。
    #[test]
    fn java_package_flows_from_parse_file_with_metadata_into_module_path() {
        let temp = tempfile::TempDir::new().expect("临时目录");
        // 故意把文件放在与包名**无关**的目录下：证明 module path 来自 `package` 声明而不是目录结构。
        let file = temp.path().join("wrong_dir").join("Runner.java");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "package com.example;\npublic class Runner { public void run() { helper(); } }\n",
        )
        .unwrap();

        let ts = crate::codegraph::treesitter::TreeSitterParser::new();
        let ast = ts
            .parse_file_with_metadata(&file)
            .expect("parse_file_with_metadata 必须成功");
        assert_eq!(
            ast.declared_package.as_deref(),
            Some(&["com".to_string(), "example".to_string()][..]),
            "交付物 2(b)：java.rs 必须把 package 名带出来（改造前是空分支）"
        );

        let ctx = FileScopeCtx::new(
            &file,
            &ast.symbols,
            Some(temp.path()),
            ast.declared_package.clone(),
            "java",
        );
        assert_eq!(
            ctx.module_path, "com.example",
            "有 package 声明时以包名为准，不再拼目录（derive_module_path 规则 2）"
        );
        // 符号集合不受影响：类 + 方法都在，import 表为空（本文件没有 import）。
        assert!(ctx.imports.is_empty(), "本用例不含 import");
        assert!(
            !ctx.scopes.is_empty(),
            "作用域树必须照常建立（Runner 类型 + run 方法）"
        );

        // 不传包名的对照组：module path 退化为目录链，证明两者确实是"包名优先"。
        let without = FileScopeCtx::new(&file, &ast.symbols, Some(temp.path()), None, "java");
        assert_eq!(without.module_path, "wrong_dir");
    }

    #[test]
    fn uuid_and_path_types_are_reachable_from_module_root() {
        // 聚合 re-export 的可达性冒烟测试（保证 P2 只 `use resolution::...` 就够）。
        let id = Uuid::nil();
        let tag = FunctionTag {
            id,
            file: PathBuf::from("x.rs"),
            module_path: "m".into(),
            name: "f".into(),
            qualified_name: "m::f".into(),
            param_count: None,
            language: "rust".into(),
            line_start: 1,
            // Phase P2a 新增的两个成员身份字段（中性值）。
            enclosing_type: None,
            has_body: true,
        };
        assert_eq!(tag.id, id);
        assert_eq!(std::mem::size_of::<ScopeKind>(), 1);
    }
}
