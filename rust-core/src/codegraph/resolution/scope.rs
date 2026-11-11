//! Phase P1 交付物 3：作用域树（`ScopeTree` / `ScopeBuilder`）。
//!
//! ## 为什么需要它
//! 当前 callee 解析完全没有"作用域"概念：`parser.rs:344` / `parser.rs:1469` /
//! `incremental.rs:275` 都是 `functions.first()` 这种"同名即命中、命中取第一个"，
//! 而 caller 归属靠 `parser.rs:1223` `_find_caller_function_by_line` 的**行区间 first-wins**
//! （嵌套函数场景下取到内层纯属 tree-sitter 产出顺序的巧合，P0 基线已用注释锁住这一点）。
//! `ScopeTree` 把"哪个函数定义在哪个作用域层、哪一行区间内"显式建出来，
//! 让 P2 能用 `innermost_scope_containing(line)` 替换掉那套顺序巧合。
//!
//! ## 硬约束
//! - **复用 `AstSymbolFields` 的 `parent_guid` / `childs_guid` 建树**，绝不重写 7 个语言 parser；
//! - 行号一律由 `full_range.start_point.row + 1` 推导（与 `parser.rs` 造 `FunctionInfo.line_start`
//!   的口径**完全一致**，否则 P2 无法把 scope 与图节点对齐）；
//! - 本模块**不产出、不删除、不改动任何符号**，只读。
//! - 生产代码不做 I/O；只有 `ScopeBuilder::build` 消费 `Vec<AstSymbolInstanceArc>`（trait getter），
//!   `ScopeTree::from_items` 是与之等价的纯数据入口，便于脱离 tree-sitter 单测。

use std::collections::{HashMap, HashSet};

/// 作用域/函数名比较用的**匿名安全名**。
///
/// tree-sitter 给匿名闭包/函数造的名字是 `format!("anon-{guid}", guid=Uuid::new_v4())`
/// （见 `treesitter/parsers/{js,ts,cpp}.rs`），每次建图都是全新随机串。若把它当作
/// tie-break 的排序键，压缩/单行 JS 里大量"同 span、同深度、同为匿名"的作用域就会
/// 随进程内 HashMap/随机数翻转 —— 实测 caller 会在 `onClick` 与 `Va` 之间来回跳
/// （`tests/test_p3_final.rs::p3b_incremental_rounds_never_yield_empty_graph_over_four_repos`
/// 的 R3 边多重集全维度抖动）。
///
/// 归一为定长前缀后，所有匿名作用域在这一档**完全相等**，比较结果落到稳定插入顺序
/// （`self.scopes` / `items` 的 Vec 下标 = tree-sitter 符号产出顺序，与进程种子无关），
/// 既确定又不改变语义（匿名作用域本就没有可比较的名字身份）。
pub(crate) fn scope_sort_name(name: &str) -> &str {
    if name.starts_with("anon-") {
        ""
    } else {
        name
    }
}
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::codegraph::treesitter::structs::SymbolType;
use crate::codegraph::treesitter::AstSymbolInstanceArc;

/// 作用域句柄。用 `usize`（`ScopeTree::scopes` 的下标）而不是 `Uuid`，
/// 是为了让 P2 在热路径上按 id 反复上溯时不引入哈希查找；对外仍以 `guid` 关联 AST 符号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(pub usize);

/// 作用域类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    /// 整个文件（永远是根）。
    File,
    /// 模块/命名空间级别（Rust `mod x {}`、Java `package`、Python 包路径）。
    Module,
    /// 类型级别（`struct` / `class` / `impl` / `trait`）。
    Type,
    /// 函数/方法体。
    Function,
    /// 普通块（局部作用域；当前 AST 层的 `VariableDefinition` 保守归到这里）。
    Block,
    /// 闭包 / lambda。
    ///
    /// 现状说明：`nested_function_caller/nested.rs` 的 `let doubled = |y| y * 2;`
    /// 在 tree-sitter Rust parser 下**只产出一个 `VariableDefinition` 符号**，
    /// 拿不到"它是个可调用实体"的信息（P0 基线里那 2 个幽灵节点之一就是它）。
    /// 因此 `ScopeBuilder::build` 目前不会自动产出 `Closure`，
    /// 该类别由 `from_items` 路径与单测覆盖，等 P2 补上 lambda 识别后再接。
    Closure,
}

impl ScopeKind {
    /// 作用域嵌套的"粗细"次序仅用于确定性 tie-break，不代表可信度。
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::File => "file",
            ScopeKind::Module => "module",
            ScopeKind::Type => "type",
            ScopeKind::Function => "function",
            ScopeKind::Block => "block",
            ScopeKind::Closure => "closure",
        }
    }
}

/// 作用域链上的一段（`ScopeTree::scope_path` 的元素）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeSegment {
    pub name: String,
    pub kind: ScopeKind,
}

/// 建树的纯数据输入项（与 AST 符号一一对应，但不依赖 AST 类型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeItem {
    pub guid: Uuid,
    /// 父符号 guid（`AstSymbolFields.parent_guid`）；`None`/查不到时退到 `childs_guid` 反推，再退到文件根。
    pub parent_guid: Option<Uuid>,
    /// 子符号 guid 列表（`AstSymbolFields.childs_guid`），用于父指针缺失时反推父子关系。
    pub childs_guid: Vec<Uuid>,
    pub kind: ScopeKind,
    pub name: String,
    /// 1-based 起始行（= `full_range.start_point.row + 1`）。
    pub line_start: usize,
    /// 1-based 结束行（= `full_range.end_point.row + 1`）。
    pub line_end: usize,
    pub param_count: Option<usize>,
    /// Phase P2a：该函数作用域**是否带函数体**（来自 AST getter `has_definition_body`）。
    ///
    /// 只有 trait 方法/抽象签名才会是 `false`。P2a 的 L3 判据用它拒绝"把动态派发
    /// 的抽象签名当成唯一实现"（AST 里没有 impl→trait 的反向链接，无法枚举实现者）。
    /// `Block`/`Type` 等非函数作用域恒为 `true`（该字段对它们无意义）。
    pub has_body: bool,
}

impl ScopeItem {
    /// 便于测试与 P2 构造。
    pub fn new(
        guid: Uuid,
        parent_guid: Option<Uuid>,
        kind: ScopeKind,
        name: &str,
        line_start: usize,
        line_end: usize,
    ) -> Self {
        Self {
            guid,
            parent_guid,
            childs_guid: vec![],
            kind,
            name: name.to_string(),
            line_start,
            line_end,
            param_count: None,
            has_body: true,
        }
    }

    pub fn with_children(mut self, childs_guid: Vec<Uuid>) -> Self {
        self.childs_guid = childs_guid;
        self
    }

    pub fn with_param_count(mut self, param_count: Option<usize>) -> Self {
        self.param_count = param_count;
        self
    }

    /// Phase P2a：标记"无函数体的抽象签名"（trait 方法等）。
    pub fn with_has_body(mut self, has_body: bool) -> Self {
        self.has_body = has_body;
        self
    }

    /// 行区间宽度（闭区间，至少 0）。
    pub fn span(&self) -> usize {
        self.line_end.saturating_sub(self.line_start)
    }
}

/// 一个作用域节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub id: ScopeId,
    pub guid: Uuid,
    pub kind: ScopeKind,
    pub name: String,
    pub line_start: usize,
    pub line_end: usize,
    pub parent: Option<ScopeId>,
    pub children: Vec<ScopeId>,
    /// 距文件根的层数（根为 0）。
    pub depth: usize,
    pub param_count: Option<usize>,
    /// Phase P2a：函数作用域是否带函数体（`false` = trait/抽象签名，见 [`ScopeItem::has_body`]）。
    pub has_body: bool,
}

impl Scope {
    pub fn contains_line(&self, line: usize) -> bool {
        self.line_start <= line && line <= self.line_end
    }

    pub fn span(&self) -> usize {
        self.line_end.saturating_sub(self.line_start)
    }
}

/// 单文件作用域树。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeTree {
    scopes: Vec<Scope>,
    root: ScopeId,
    by_guid: HashMap<Uuid, ScopeId>,
    /// 归属文件（仅用于诊断信息与限定名拼接）。
    file: PathBuf,
}

impl Default for ScopeTree {
    fn default() -> Self {
        Self::empty(Path::new(""))
    }
}

impl ScopeTree {
    /// 只含文件根的空树。
    pub fn empty(file: &Path) -> Self {
        let root = Scope {
            id: ScopeId(0),
            guid: Uuid::nil(),
            kind: ScopeKind::File,
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            line_start: 1,
            line_end: 1,
            parent: None,
            children: vec![],
            depth: 0,
            param_count: None,
            // 文件根不是函数，`has_body` 对它无意义，取"有体"以免被 L3 的抽象签名判据误伤。
            has_body: true,
        };
        Self {
            scopes: vec![root],
            root: ScopeId(0),
            by_guid: HashMap::new(),
            file: file.to_path_buf(),
        }
    }

    /// 从纯数据项建树（不依赖任何 AST 类型）。
    ///
    /// 挂接规则（**顺序无关、结果确定**）：
    /// 1. `parent_guid` 能在本批 item 里找到 → 挂它；
    /// 2. 否则若有别的 item 的 `childs_guid` 包含本 guid → 挂那个"反推出来的父"
    ///    （多个候选时取行区间**最小**的那个，再按 guid 稳定 tie-break）；
    /// 3. 都没有 → 挂文件根。
    /// 自环（parent == 自己）一律降级到文件根，绝不制造环。
    pub fn from_items(file: &Path, items: impl IntoIterator<Item = ScopeItem>) -> Self {
        let items: Vec<ScopeItem> = items.into_iter().collect();

        let mut tree = Self::empty(file);
        if items.is_empty() {
            return tree;
        }

        // guid -> 在 items 中的下标，用于第一遍父子解析。
        let mut index_of: HashMap<Uuid, usize> = HashMap::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            // 同 guid 重复出现时只认第一条（正常 AST 不会重复；这是防御，不是塌缩）。
            index_of.entry(item.guid).or_insert(idx);
        }

        // 为每个 item 预先算出"父在 items 中的下标"（None = 挂文件根），完全两趟分离，避免借用冲突。
        let mut chosen_parent: Vec<Option<usize>> = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            let direct = item
                .parent_guid
                .as_ref()
                .and_then(|guid| index_of.get(guid).copied())
                .filter(|parent_idx| *parent_idx != idx);
            let resolved = match direct {
                Some(parent_idx) => Some(parent_idx),
                None => {
                    // 反推：谁的 childs_guid 里含有我？
                    let mut candidates: Vec<usize> = items
                        .iter()
                        .enumerate()
                        .filter(|(other_idx, other)| {
                            *other_idx != idx && other.childs_guid.contains(&item.guid)
                        })
                        .map(|(other_idx, _)| other_idx)
                        .collect();
                    if candidates.is_empty() {
                        None
                    } else {
                        candidates.sort_by(|a, b| {
                            items[*a]
                                .span()
                                .cmp(&items[*b].span())
                                // Phase P3 收口 C：tie-break 用源码派生字段，不用 guid。
                                // guid 是每次建图重新生成的 Uuid ⇒ 只保证同进程可复现，
                                // 不保证"同一份源码两次独立建图可复现"（压缩 JS 里几十个
                                // 作用域同 span、同名，比较结果整条落到 guid 那一位）。
                                .then_with(|| {
                                    (
                                        scope_sort_name(&items[*a].name),
                                        items[*a].line_start,
                                        items[*a].line_end,
                                        items[*a].param_count,
                                        items[*a].has_body,
                                    )
                                        .cmp(&(
                                            scope_sort_name(&items[*b].name),
                                            items[*b].line_start,
                                            items[*b].line_end,
                                            items[*b].param_count,
                                            items[*b].has_body,
                                        ))
                                })
                        });
                        Some(candidates[0])
                    }
                }
            };
            // 反推出的父若形成直接互指（A 的父是 B 且 B 的父是 A），保留哪一个都行，
            // 但 depth 计算里有环保护，绝不会死循环。
            chosen_parent.push(resolved);
        }

        // 第二遍：把 item 变成 Scope（下标 = ScopeId(idx + 1)，因为 0 是文件根）。
        let mut line_max = 0usize;
        for (idx, item) in items.iter().enumerate() {
            line_max = line_max.max(item.line_end);
            let scope = Scope {
                id: ScopeId(idx + 1),
                guid: item.guid,
                kind: item.kind,
                name: item.name.clone(),
                line_start: item.line_start,
                line_end: item.line_end,
                parent: Some(match chosen_parent[idx] {
                    Some(parent_idx) => ScopeId(parent_idx + 1),
                    None => tree.root,
                }),
                children: vec![],
                depth: 0,
                param_count: item.param_count,
                has_body: item.has_body,
            };
            tree.by_guid.insert(item.guid, scope.id);
            tree.scopes.push(scope);
        }

        // 第三遍：填 children（按 ScopeId 升序 ⇒ 与 items 顺序一致，稳定可复现）。
        let child_pairs: Vec<(ScopeId, ScopeId)> = (1..tree.scopes.len())
            .map(|idx| {
                let parent = tree.scopes[idx].parent.unwrap_or(tree.root);
                (parent, ScopeId(idx))
            })
            .collect();
        for (parent, child) in child_pairs {
            tree.scopes[parent.0].children.push(child);
        }

        // 文件根覆盖**整个文件**：起点固定为第 1 行（不是"第一个符号所在行"），
        // 终点取所有作用域的最大行。
        // 为什么必须从 1 开始：调用点可能落在任何函数之外（模块级语句、顶层 import 之后的调用），
        // 若根从第一个符号的行开始，这些行会 `innermost_scope_containing` 返回 None，
        // P2 就没有"文件级作用域"这个兜底层可用了。
        // TODO(P2)：真正的文件尾行需要文件总行数（`ScopeBuilder::build` 可以顺带拿到，
        // 因为 `AstSymbolFields.full_range` 里有；目前用最大符号行做下界，宁可少覆盖尾行）。
        if line_max > 0 {
            tree.scopes[tree.root.0].line_start = 1;
            tree.scopes[tree.root.0].line_end = line_max;
        }

        // 第四遍：从根逐层算 depth（BFS + visited，成环也不会死循环）。
        let mut visited: HashSet<usize> = HashSet::new();
        let mut frontier: Vec<ScopeId> = vec![tree.root];
        visited.insert(tree.root.0);
        while !frontier.is_empty() {
            let mut next: Vec<ScopeId> = Vec::new();
            for id in frontier {
                let depth = tree.scopes[id.0].depth;
                let children = tree.scopes[id.0].children.clone();
                for child in children {
                    if visited.insert(child.0) {
                        tree.scopes[child.0].depth = depth + 1;
                        next.push(child);
                    }
                }
            }
            frontier = next;
        }

        tree
    }

    pub fn root(&self) -> ScopeId {
        self.root
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// 作用域总数（含文件根）。
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    pub fn is_empty(&self) -> bool {
        // 恒有文件根，所以"空树"= 只有根。
        self.scopes.len() <= 1
    }

    pub fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.0)
    }

    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    pub fn scope_id_of_guid(&self, guid: &Uuid) -> Option<ScopeId> {
        self.by_guid.get(guid).copied()
    }

    pub fn guid_of_scope(&self, id: ScopeId) -> Option<Uuid> {
        self.scope(id).map(|s| s.guid)
    }

    /// 覆盖 `line` 的全部作用域，按**外层 → 内层**排序（行区间宽度降序）。
    ///
    /// 这条 API 存在的意义：P0 基线第 6 项断言"第 12 行同时落在 `inner(11..13)` 与
    /// `outer(10..16)` 两个函数区间内"，而 caller 归属靠产出顺序巧合。把它显式化后，
    /// P2 只需要 `containing(line).last()`（即 `innermost_scope_containing`）。
    pub fn scopes_containing(&self, line: usize) -> Vec<ScopeId> {
        let mut hits: Vec<ScopeId> = self
            .scopes
            .iter()
            .filter(|scope| scope.contains_line(line))
            .map(|scope| scope.id)
            .collect();
        hits.sort_by(|a, b| {
            let sa = &self.scopes[a.0];
            let sb = &self.scopes[b.0];
            sb.span()
                .cmp(&sa.span())
                .then_with(|| sb.depth.cmp(&sa.depth))
                // Phase P3 收口 C：末位改用源码派生字段（`guid` 每次建图都是新 Uuid，
                // 只保证同进程可复现，不保证同一份源码两次独立建图可复现）。
                // ⚠ 收口 C 的补丁本身漏了一处：匿名作用域的 `name` 是
                //   `format!("anon-{guid}")`（见 treesitter/parsers/js.rs / ts.rs / cpp.rs），
                //   guid 仍是每次建图的新 Uuid ⇒ 拿 `name` 参与比较，压缩 JS 里
                //   几十条同 span/同 depth 的匿名作用域照样随机翻转 caller（实测
                //   `onClick` ↔ `Va`）。因此这里比较的是**匿名安全名**：`anon-*` 一律归一为
                //   空串，让同 span 的匿名作用域落到 `self.scopes` 的稳定插入顺序
                //   （= tree-sitter 符号产出顺序，与进程种子无关）上，绝不参与 name 比较。
                .then_with(|| {
                    (
                        scope_sort_name(&sa.name),
                        sa.line_start,
                        sa.line_end,
                        sa.param_count,
                        sa.has_body,
                    )
                        .cmp(&(
                            scope_sort_name(&sb.name),
                            sb.line_start,
                            sb.line_end,
                            sb.param_count,
                            sb.has_body,
                        ))
                })
        });
        hits
    }

    /// **最内层**覆盖 `line` 的作用域。这是 P2 替换 `_find_caller_function_by_line`
    /// 行区间 first-wins 的目标 API（`parser.rs:1223`，调用点 1180/1200/1260/1357）。
    ///
    /// 判定：行区间宽度最小者优先；同宽度取 `depth` 更大者；再同按 `guid` 升序，
    /// 从而**与 tree-sitter 的符号产出顺序、与 HashMap 种子都无关**。
    /// 文件根也参与候选（它覆盖全文件），所以文件级调用点会稳定落到根。
    pub fn innermost_scope_containing(&self, line: usize) -> Option<ScopeId> {
        self.scopes
            .iter()
            .filter(|scope| scope.contains_line(line))
            .min_by(|a, b| {
                a.span()
                    .cmp(&b.span())
                    .then_with(|| b.depth.cmp(&a.depth))
                    // Phase P3 收口 C：末位改用源码派生字段（`guid` 每次建图都是新 Uuid ⇒
                    // 同一份源码两次独立建图可能把调用点归到不同 caller，实测压缩 JS 里
                    // caller 会在 `onClick` 与 `Va` 之间来回跳）。
                    // ⚠ 同 `scopes_containing`：匿名作用域的 `name` 内嵌 `anon-{guid}` 随机量，
                    //   必须用 `scope_sort_name` 归一，否则"源码派生字段"这一档仍然随机。
                    .then_with(|| {
                        (
                            scope_sort_name(&a.name),
                            a.line_start,
                            a.line_end,
                            a.param_count,
                            a.has_body,
                        )
                            .cmp(&(
                                scope_sort_name(&b.name),
                                b.line_start,
                                b.line_end,
                                b.param_count,
                                b.has_body,
                            ))
                    })
            })
            .map(|scope| scope.id)
    }

    /// 从根到 `id` 的作用域链（含两端）。带步数上限防环。
    pub fn scope_chain(&self, id: ScopeId) -> Vec<ScopeId> {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        let mut seen: HashSet<usize> = HashSet::new();
        let limit = self.scopes.len() + 1;
        while let Some(current) = cursor {
            if !seen.insert(current.0) || chain.len() > limit {
                break;
            }
            let scope = match self.scope(current) {
                Some(s) => s,
                None => break,
            };
            chain.push(current);
            cursor = scope.parent;
        }
        chain.reverse();
        chain
    }

    /// 作用域链的"可读形式"：`module → type → function` 的段序列（含文件根，根为 `File`）。
    ///
    /// 任务要求的 (ii)：P2 拼限定名（`mod_a::pick` / `com.example.Main::run`）就靠它。
    pub fn scope_path(&self, id: ScopeId) -> Vec<ScopeSegment> {
        self.scope_chain(id)
            .into_iter()
            .filter_map(|scope_id| {
                self.scope(scope_id).map(|scope| ScopeSegment {
                    name: scope.name.clone(),
                    kind: scope.kind,
                })
            })
            .collect()
    }

    /// 拼接限定名。跳过文件根与空名字段；默认分隔符 `::`。
    pub fn qualified_name(&self, id: ScopeId, separator: &str) -> String {
        self.scope_chain(id)
            .into_iter()
            .filter_map(|scope_id| {
                let scope = self.scope(scope_id)?;
                if scope.kind == ScopeKind::File || scope.name.is_empty() {
                    None
                } else {
                    Some(scope.name.clone())
                }
            })
            .collect::<Vec<String>>()
            .join(separator)
    }

    /// 作用域链上**最内层**的类型作用域名（`impl`/`class`/`struct`/`trait` 块）。
    ///
    /// Phase P2a 交付物 1 的 L2 `EnclosingTypeMember` 判据：调用点所在函数若挂在某个类型
    /// 作用域下，无限定成员调用（以及 `self.x()` / `this.x()`）只该在该类型的成员里找。
    /// 顶层自由函数 → `None`（没有外围类型，L2 必须下沉，不许猜）。
    /// 取"链上最后一个 Type"（最内层），因为内层遮蔽外层是支配规则的一部分。
    pub fn enclosing_type_name(&self, id: ScopeId) -> Option<String> {
        self.scope_chain(id)
            .into_iter()
            .filter_map(|scope_id| self.scope(scope_id))
            .filter(|scope| scope.kind == ScopeKind::Type && !scope.name.is_empty())
            .last()
            .map(|scope| scope.name.clone())
    }

    /// 本文件里所有函数/方法/闭包作用域（按 ScopeId 升序，稳定）。
    pub fn callable_scopes(&self) -> Vec<ScopeId> {
        (1..self.scopes.len())
            .filter(|idx| {
                matches!(
                    self.scopes[*idx].kind,
                    ScopeKind::Function | ScopeKind::Closure
                )
            })
            .map(ScopeId)
            .collect()
    }

    /// 本文件里所有类型作用域。
    pub fn type_scopes(&self) -> Vec<ScopeId> {
        (1..self.scopes.len())
            .filter(|idx| self.scopes[*idx].kind == ScopeKind::Type)
            .map(ScopeId)
            .collect()
    }

    /// 某个类型作用域的**成员**（名字 + 行区间），P2 的 L2 `EnclosingTypeMember` 需要。
    pub fn members_of_type(&self, type_scope: ScopeId) -> Vec<(String, Vec<ScopeId>)> {
        let children = match self.scope(type_scope) {
            Some(s) => s.children.clone(),
            None => return vec![],
        };
        let mut out: Vec<(String, Vec<ScopeId>)> = children
            .iter()
            .filter(|id| {
                matches!(
                    self.scope(**id).map(|s| s.kind),
                    Some(ScopeKind::Function) | Some(ScopeKind::Closure)
                )
            })
            .map(|id| {
                (
                    self.scope(*id).map(|s| s.name.clone()).unwrap_or_default(),
                    vec![*id],
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1[0].cmp(&b.1[0])));
        out
    }
}

/// 从 AST 符号列表建 `ScopeTree` 的构造器。
///
/// 收录规则（**只读符号，不改符号**）：
/// - `SymbolType::FunctionDeclaration` → `ScopeKind::Function`
/// - `SymbolType::StructDeclaration`   → `ScopeKind::Type`
/// - `SymbolType::VariableDefinition`  → `ScopeKind::Block`（保守：拿不到"是否 lambda"，见 `ScopeKind::Closure` 注释）
/// - 其余（含 `FunctionCall` / `VariableUsage` / `ImportDeclaration` / `CommentDefinition` …）
///   **不进树**，但它们的 `parent_guid` 仍可能指向已入树的作用域，父链照样成立。
pub struct ScopeBuilder;

impl ScopeBuilder {
    /// 符号类型 → 作用域类别。`None` = 不建作用域。
    pub fn scope_kind_of(symbol_type: SymbolType) -> Option<ScopeKind> {
        match symbol_type {
            SymbolType::FunctionDeclaration => Some(ScopeKind::Function),
            SymbolType::StructDeclaration => Some(ScopeKind::Type),
            SymbolType::VariableDefinition => Some(ScopeKind::Block),
            _ => None,
        }
    }

    /// 由真实 AST 符号列表建树。
    ///
    /// 注意 `full_range().start_point.row` 是 **0-based**，这里统一 `+1`，
    /// 与 `parser.rs:197` / `parser.rs:499` 两处造 `FunctionInfo.line_start` 的口径完全一致 ——
    /// 这是 P2 能把 scope 与图节点对齐的前提。
    pub fn build(file: &Path, symbols: &[AstSymbolInstanceArc]) -> ScopeTree {
        let items: Vec<ScopeItem> = symbols
            .iter()
            .filter_map(|symbol| {
                let guard = symbol.read();
                let symbol_ref = guard.as_ref();
                let kind = Self::scope_kind_of(symbol_ref.symbol_type())?;
                Some(ScopeItem {
                    guid: *symbol_ref.guid(),
                    parent_guid: symbol_ref.parent_guid().as_ref().copied(),
                    childs_guid: symbol_ref.childs_guid().clone(),
                    kind,
                    name: symbol_ref.name().to_string(),
                    line_start: symbol_ref.full_range().start_point.row + 1,
                    line_end: symbol_ref.full_range().end_point.row + 1,
                    param_count: symbol_ref.param_count(),
                    // Phase P2a：trait/抽象签名（无 body）必须带上这个标记，
                    // L3 靠它拒绝把"唯一一条抽象签名"当作最终实现。
                    has_body: symbol_ref.has_definition_body(),
                })
            })
            .collect();
        ScopeTree::from_items(file, items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    fn file() -> PathBuf {
        PathBuf::from("nested.rs")
    }

    /// 复刻 P0 基线第 6 项 `nested_function_caller/nested.rs` 的真实行区间：
    /// `outer(10..16)` 里嵌套 `inner(11..13)`，调用点在**第 12 行**。
    fn nested_tree() -> ScopeTree {
        ScopeTree::from_items(
            file().as_path(),
            vec![
                // 故意把 inner 放在 outer **之后**：证明"最内层"不依赖输入顺序。
                ScopeItem::new(guid(2), Some(guid(1)), ScopeKind::Function, "inner", 11, 13),
                ScopeItem::new(guid(1), None, ScopeKind::Function, "outer", 10, 16),
                ScopeItem::new(guid(3), Some(guid(1)), ScopeKind::Function, "leaf", 14, 15),
            ],
        )
    }

    #[test]
    fn builds_tree_from_parent_guid_and_attaches_orphans_to_file_root() {
        let tree = nested_tree();
        assert_eq!(tree.len(), 4, "文件根 + 3 个函数");
        let outer = tree
            .scope_id_of_guid(&guid(1))
            .expect("outer 应在树里");
        let inner = tree.scope_id_of_guid(&guid(2)).expect("inner 应在树里");
        let leaf = tree.scope_id_of_guid(&guid(3)).expect("leaf 应在树里");
        // 孤儿（parent_guid = None）挂文件根。
        assert_eq!(tree.scope(outer).unwrap().parent, Some(tree.root()));
        // 父子关系来自 parent_guid，而不是产出顺序。
        assert_eq!(tree.scope(inner).unwrap().parent, Some(outer));
        // outer 有两个 child_guid=outer 的孩子：inner 与 leaf（children 按 ScopeId 升序，
        // 即 items 的输入顺序 ⇒ inner 在下标 1、leaf 在下标 3）。
        assert_eq!(tree.scope(outer).unwrap().children, vec![inner, leaf]);
        assert_eq!(tree.scope(outer).unwrap().depth, 1);
        assert_eq!(tree.scope(inner).unwrap().depth, 2);
        assert_eq!(tree.scope(tree.root()).unwrap().depth, 0);
    }

    /// 任务要求的 (i)：innermost 判定 —— 这正是 P2 要替换掉的"行区间 first-wins"。
    #[test]
    fn innermost_scope_containing_picks_the_narrowest_interval_not_the_first_produced() {
        let tree = nested_tree();
        let hits = tree.scopes_containing(12);
        // 文件根(10..16)、outer(10..16)、inner(11..13) 都覆盖第 12 行。
        assert!(hits.len() >= 3, "第 12 行应被 3 个作用域覆盖，实际 = {hits:?}");
        let innermost = tree
            .innermost_scope_containing(12)
            .expect("第 12 行必有最内层作用域");
        assert_eq!(
            tree.guid_of_scope(innermost),
            Some(guid(2)),
            "必须取到 inner(11..13)，而不是 outer —— P0 基线里那是 tree-sitter 产出顺序的巧合"
        );

        // 输入顺序颠倒，结果必须完全一致。
        let flipped = ScopeTree::from_items(
            file().as_path(),
            vec![
                ScopeItem::new(guid(1), None, ScopeKind::Function, "outer", 10, 16),
                ScopeItem::new(guid(2), Some(guid(1)), ScopeKind::Function, "inner", 11, 13),
                ScopeItem::new(guid(3), Some(guid(1)), ScopeKind::Function, "leaf", 14, 15),
            ],
        );
        assert_eq!(
            flipped.guid_of_scope(flipped.innermost_scope_containing(12).unwrap()),
            Some(guid(2))
        );
    }

    #[test]
    fn scopes_containing_is_ordered_outside_in_by_span_width() {
        let tree = nested_tree();
        let hits = tree.scopes_containing(12);
        let spans: Vec<usize> = hits
            .iter()
            .map(|id| tree.scope(*id).unwrap().span())
            .collect();
        for pair in spans.windows(2) {
            assert!(pair[0] >= pair[1], "外层→内层，区间宽度必须递减：{spans:?}");
        }
        assert_eq!(*hits.last().unwrap(), tree.innermost_scope_containing(12).unwrap());
    }

    /// 闭包用例：`Closure` 作用域只能由 `from_items` 建出（当前 AST 拿不到 lambda 事实）。
    #[test]
    fn closure_scope_is_built_and_is_innermost_for_its_body() {
        let tree = ScopeTree::from_items(
            file().as_path(),
            vec![
                ScopeItem::new(guid(1), None, ScopeKind::Function, "closure_user", 19, 21),
                ScopeItem::new(
                    guid(2),
                    Some(guid(1)),
                    ScopeKind::Closure,
                    "doubled",
                    20,
                    20,
                ),
            ],
        );
        assert_eq!(tree.scope_id_of_guid(&guid(2)).is_some(), true);
        let innermost = tree.innermost_scope_containing(20).unwrap();
        assert_eq!(tree.guid_of_scope(innermost), Some(guid(2)));
        assert_eq!(tree.scope(innermost).unwrap().kind, ScopeKind::Closure);
        // callable 集合应同时含函数与闭包。
        assert_eq!(tree.callable_scopes().len(), 2);
    }

    /// 同宽度 tie-break 必须走 guid，绝不能依赖输入顺序或哈希种子。
    #[test]
    fn equal_span_is_broken_by_depth_then_guid_deterministically() {
        let items = vec![
            ScopeItem::new(guid(9), None, ScopeKind::Function, "b", 5, 8),
            ScopeItem::new(guid(3), None, ScopeKind::Function, "a", 5, 8),
        ];
        let one = ScopeTree::from_items(file().as_path(), items.clone());
        let two = ScopeTree::from_items(file().as_path(), items.into_iter().rev().collect::<Vec<_>>());
        // 比较 guid 而不是 ScopeId：下标随输入顺序变化，但选中的作用域必须是同一个。
        assert_eq!(
            one.guid_of_scope(one.innermost_scope_containing(6).unwrap()),
            two.guid_of_scope(two.innermost_scope_containing(6).unwrap()),
            "同区间宽度的 tie-break 必须与输入顺序无关"
        );
        assert_eq!(one.guid_of_scope(one.innermost_scope_containing(6).unwrap()), Some(guid(3)));
    }

    /// 任务要求的 (ii)：`scope_path` 能拼出 module + type 链 + 函数名形式的限定名。
    #[test]
    fn scope_path_and_qualified_name_build_module_type_fn_chain() {
        let tree = ScopeTree::from_items(
            PathBuf::from("src/engine/runner.java").as_path(),
            vec![
                ScopeItem::new(guid(1), None, ScopeKind::Module, "engine", 1, 40),
                ScopeItem::new(guid(2), Some(guid(1)), ScopeKind::Type, "Runner", 3, 30),
                ScopeItem::new(guid(3), Some(guid(2)), ScopeKind::Function, "run", 10, 20),
            ],
        );
        let run = tree.scope_id_of_guid(&guid(3)).unwrap();
        let path = tree.scope_path(run);
        assert_eq!(path[0].kind, ScopeKind::File, "第一段恒为文件根");
        assert_eq!(
            path.iter().skip(1).cloned().collect::<Vec<_>>(),
            vec![
                ScopeSegment { name: "engine".into(), kind: ScopeKind::Module },
                ScopeSegment { name: "Runner".into(), kind: ScopeKind::Type },
                ScopeSegment { name: "run".into(), kind: ScopeKind::Function },
            ]
        );
        assert_eq!(tree.qualified_name(run, "::"), "engine::Runner::run");
        assert_eq!(tree.qualified_name(run, "."), "engine.Runner.run");
        // 文件根本身不得产出限定名（它是容器，不是名字）。
        assert_eq!(tree.qualified_name(tree.root(), "::"), "");
    }

    #[test]
    fn childs_guid_backfills_parent_when_parent_guid_is_missing() {
        // 模拟"语言 parser 只填了 childs_guid、没填 parent_guid"的现实形态。
        let tree = ScopeTree::from_items(
            file().as_path(),
            vec![
                ScopeItem::new(guid(1), None, ScopeKind::Type, "Calc", 1, 15)
                    .with_children(vec![guid(2)]),
                ScopeItem::new(guid(2), None, ScopeKind::Function, "add", 5, 6),
            ],
        );
        let outer = tree.scope_id_of_guid(&guid(1)).unwrap();
        let inner = tree.scope_id_of_guid(&guid(2)).unwrap();
        assert_eq!(
            tree.scope(inner).unwrap().parent,
            Some(outer),
            "parent_guid 缺失时应由 childs_guid 反推父子关系"
        );
        assert_eq!(tree.scope(inner).unwrap().depth, 2);
    }

    #[test]
    fn self_parenting_item_is_demoted_to_file_root_instead_of_looping() {
        let tree = ScopeTree::from_items(
            file().as_path(),
            vec![ScopeItem::new(guid(7), Some(guid(7)), ScopeKind::Function, "loop", 2, 4)],
        );
        let id = tree.scope_id_of_guid(&guid(7)).unwrap();
        assert_eq!(tree.scope(id).unwrap().parent, Some(tree.root()));
        assert_eq!(tree.scope(id).unwrap().depth, 1);
        // 防环：上溯链不会失控。
        assert_eq!(tree.scope_chain(id).len(), 2);
    }

    #[test]
    fn two_way_parent_child_pointer_swap_is_stable() {
        // A 声明父为 B，B 通过 childs_guid 声明父为 A —— 不允许死循环。
        let tree = ScopeTree::from_items(
            file().as_path(),
            vec![
                ScopeItem::new(guid(1), Some(guid(2)), ScopeKind::Function, "a", 1, 10),
                ScopeItem::new(guid(2), None, ScopeKind::Function, "b", 1, 10)
                    .with_children(vec![guid(1)]),
            ],
        );
        // a(ScopeId(1)) 的父是 b(ScopeId(2))，b 挂文件根 ⇒ 链长 3；b 自己链长 2。
        assert_eq!(tree.scope_chain(ScopeId(1)).len(), 3);
        assert_eq!(tree.scope_chain(ScopeId(2)).len(), 2);
        assert!(tree.innermost_scope_containing(5).is_some());
        // 环保护：即使父子声明互相矛盾，上溯也必须终止且不重复访问。
        assert_eq!(tree.scope_chain(ScopeId(1))[0], tree.root());
    }

    #[test]
    fn file_root_spans_whole_file_and_empty_tree_has_only_root() {
        let tree = nested_tree();
        let root = tree.scope(tree.root()).unwrap();
        // 文件根**恒从第 1 行开始**（第一个符号在第 10 行也一样）：
        // 否则函数之前的调用点（模块级语句）会拿不到"文件级作用域"这个兜底层。
        // 尾行目前只能取"最大符号行"，见 from_items 里的 TODO(P2)。
        assert_eq!((root.line_start, root.line_end), (1, 16));
        assert_eq!(root.kind, ScopeKind::File);

        let empty = ScopeTree::empty(Path::new("x/y.rs"));
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 1);
        // `file()` 保留传入的**完整路径**（诊断与 module path 推导要用），
        // 只有文件根的 `name` 取 `file_name()`（限定名拼接时不该带上目录）。
        assert_eq!(empty.file(), Path::new("x/y.rs"));
        assert_eq!(empty.scope(empty.root()).unwrap().name, "y.rs");
        assert_eq!(empty.innermost_scope_containing(1), Some(empty.root()));
        assert_eq!(empty.scope_path(empty.root()).len(), 1);
    }

    #[test]
    fn line_outside_every_item_still_resolves_to_file_root_only() {
        let tree = nested_tree();
        let hits = tree.scopes_containing(99);
        assert!(hits.is_empty(), "根区间是 10..16，第 99 行谁都不覆盖");
        assert!(tree.innermost_scope_containing(99).is_none());
    }

    #[test]
    fn members_of_type_lists_child_functions_by_name() {
        let tree = ScopeTree::from_items(
            file().as_path(),
            vec![
                ScopeItem::new(guid(1), None, ScopeKind::Type, "Calc", 1, 20),
                ScopeItem::new(guid(2), Some(guid(1)), ScopeKind::Function, "sub", 8, 9),
                ScopeItem::new(guid(3), Some(guid(1)), ScopeKind::Function, "add", 4, 5),
            ],
        );
        let ty = tree.scope_id_of_guid(&guid(1)).unwrap();
        assert_eq!(tree.type_scopes(), vec![ty]);
        let members = tree.members_of_type(ty);
        let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["add", "sub"], "必须按名字稳定排序");
    }

    /// `scope_kind_of` 的映射表 —— 同时证明"调用/变量使用/import 不进树"。
    #[test]
    fn symbol_type_to_scope_kind_mapping_excludes_calls_and_imports() {
        assert_eq!(
            ScopeBuilder::scope_kind_of(SymbolType::FunctionDeclaration),
            Some(ScopeKind::Function)
        );
        assert_eq!(
            ScopeBuilder::scope_kind_of(SymbolType::StructDeclaration),
            Some(ScopeKind::Type)
        );
        assert_eq!(
            ScopeBuilder::scope_kind_of(SymbolType::VariableDefinition),
            Some(ScopeKind::Block)
        );
        for excluded in [
            SymbolType::FunctionCall,
            SymbolType::VariableUsage,
            SymbolType::ImportDeclaration,
            SymbolType::CommentDefinition,
            SymbolType::ClassFieldDeclaration,
            SymbolType::TypeAlias,
            SymbolType::Module,
            SymbolType::Unknown,
        ] {
            // SymbolType 不是 Copy，先把名字格式化出来再消耗它。
            let label = format!("{excluded:?}");
            assert_eq!(
                ScopeBuilder::scope_kind_of(excluded),
                None,
                "{label} 不应建作用域"
            );
        }
    }
}
