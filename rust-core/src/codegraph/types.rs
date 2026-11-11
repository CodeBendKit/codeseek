use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use petgraph::visit::EdgeRef;

/// 函数信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub id: Uuid,
    pub name: String,
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub namespace: String,
    pub language: String,
    pub signature: Option<String>,
    // ============================================================================================
    // Phase P2b 交付物 1：把"分层解析需要的身份"提升为节点的一等属性。
    //
    // 两个字段都是 `Option` 且 `#[serde(default)]`：
    // - **layered 路径**在建立 `FunctionInfo` 时用 `FileScopeCtx` 的 module path 与
    //   `ScopeTree` 的 `scope_path` 填充；
    // - **legacy 路径**一律保持 `None`（红线 1：不许改变 legacy 节点内容，逐字节不变）。
    //
    // 因为它们是"追加在结构体尾部"的新字段，序列化形状发生变化，所以 `GRAPH_SCHEMA_VERSION`
    // 必须在同一次改动里 1→2（见 `storage/petgraph_storage.rs`）。
    // ============================================================================================
    /// 该函数所属文件的 module path（`layered` 填充；`legacy` = `None`）。
    #[serde(default)]
    pub module_path: Option<String>,
    /// 该函数的限定名 `module_path::name`（`layered` 填充；`legacy` = `None`）。
    #[serde(default)]
    pub qualified_name: Option<String>,
}

impl FunctionInfo {
    /// **legacy 路径**的构造入口：新身份字段一律 `None`，因此产出的节点与 P1 逐字节一致
    /// （红线 1）。所有既有 legacy 构造点统一走这里，避免逐字面量补字段。
    #[allow(clippy::too_many_arguments)]
    pub fn legacy(
        id: Uuid,
        name: String,
        file_path: PathBuf,
        line_start: usize,
        line_end: usize,
        namespace: String,
        language: String,
        signature: Option<String>,
    ) -> Self {
        Self {
            id,
            name,
            file_path,
            line_start,
            line_end,
            namespace,
            language,
            signature,
            module_path: None,
            qualified_name: None,
        }
    }
}

/// 调用关系
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRelation {
    pub caller_id: Uuid,
    pub callee_id: Uuid,
    pub caller_name: String,
    pub callee_name: String,
    pub caller_file: PathBuf,
    pub callee_file: PathBuf,
    pub line_number: usize,
    pub is_resolved: bool,
    // ============================================================================================
    // Phase P2b 交付物 1：把"解析层级 / 置信度 / 歧义候选"变成调用边的一等属性。
    //
    // 派生规则（口径 A）：
    //   `is_resolved = (resolution_level > 0 && candidate_count == 1)`
    // 即 `is_resolved` 字段保留、不删，只是 layered 路径按上式重算；legacy 路径继续写
    // 它原有的 `true`/`false`，四个新标量对 legacy 恒为默认值（`0`），因此 legacy 的
    // 边语义与逐字节内容均不变。
    //
    // - `resolution_level`：`ResolutionLevel as u8`（0=Unresolved .. 8=Heuristic）。
    // - `confidence`：由 `ResolutionLevel::confidence()` 推导（0..=98）。
    // - `rank`：该候选在同层候选里的排序位（0 起，`rank_candidates` 决定）。
    // - `candidate_count`：该调用点截断前的**真实**候选个数（u8 饱和到 255）。
    // ============================================================================================
    /// 命中该边所依据的解析层级（0=未解析/legacy 默认；见 `ResolutionLevel`）。
    #[serde(default)]
    pub resolution_level: u8,
    /// 该层级的置信度（与 `resolution_level` 同步冗余存一份，便于落盘后不重算）。
    #[serde(default)]
    pub confidence: u8,
    /// 同一调用点候选里的排序位（0 = 最优）。
    #[serde(default)]
    pub rank: u8,
    /// 该调用点截断前的真实候选总数（u8 饱和到 255）。
    #[serde(default)]
    pub candidate_count: u8,
}

impl CallRelation {
    /// **legacy 路径**的构造入口：四个新标量一律 `0`，`is_resolved` 由调用方按旧语义传入。
    /// 这样 legacy 边的逐字节内容与 P1 完全一致（红线 1），而分层元信息只在 layered 分支
    /// 经 [`CallRelation::layered`] 写入。
    #[allow(clippy::too_many_arguments)]
    pub fn legacy(
        caller_id: Uuid,
        callee_id: Uuid,
        caller_name: String,
        callee_name: String,
        caller_file: PathBuf,
        callee_file: PathBuf,
        line_number: usize,
        is_resolved: bool,
    ) -> Self {
        Self {
            caller_id,
            callee_id,
            caller_name,
            callee_name,
            caller_file,
            callee_file,
            line_number,
            is_resolved,
            resolution_level: 0,
            confidence: 0,
            rank: 0,
            candidate_count: 0,
        }
    }

    /// **layered 路径**的构造入口（Phase P2b 交付物 1）。
    ///
    /// 四个新标量在这里一次性写全，`is_resolved` 由**口径 A 的派生规则**决定，
    /// 不允许调用方自带一个 bool —— 否则"歧义被伪装成已解析"这个 P0 缺陷会以新的
    /// 形式复发：
    ///
    /// ```text
    /// is_resolved = (resolution_level > 0 && candidate_count == 1)
    /// ```
    ///
    /// `resolution_level` / `confidence` 取 `ResolutionLevel as u8` 与其
    /// `confidence()`，`rank` 是候选在 `rank_candidates` 之后的名次（0 起），
    /// `candidate_count` 是**截断前**的真实候选数（`--max-candidates` 只截断落盘条数，
    /// 不改这个数，也不改 `is_resolved` 的判定）。
    #[allow(clippy::too_many_arguments)]
    pub fn layered(
        caller_id: Uuid,
        callee_id: Uuid,
        caller_name: String,
        callee_name: String,
        caller_file: PathBuf,
        callee_file: PathBuf,
        line_number: usize,
        resolution_level: u8,
        confidence: u8,
        rank: u8,
        candidate_count: u8,
    ) -> Self {
        Self {
            caller_id,
            callee_id,
            caller_name,
            callee_name,
            caller_file,
            callee_file,
            line_number,
            is_resolved: resolution_level > 0 && candidate_count == 1,
            resolution_level,
            confidence,
            rank,
            candidate_count,
        }
    }

    /// **口径 B（覆盖中性）**：layered 遇到 `ResolutionStatus::Unresolved` 时合成的幽灵边。
    ///
    /// 与 [`CallRelation::legacy`] 的未解析边逐字段同形（四个标量 = 0、`is_resolved=false`），
    /// 差别只在于它是 layered 路径写的：这样"有边的调用点数"在两种模式下必然相等，
    /// layered 不会因为"更诚实"而**净减少**调用边。
    pub fn layered_ghost(
        caller_id: Uuid,
        callee_id: Uuid,
        caller_name: String,
        callee_name: String,
        caller_file: PathBuf,
        callee_file: PathBuf,
        line_number: usize,
    ) -> Self {
        Self::layered(
            caller_id,
            callee_id,
            caller_name,
            callee_name,
            caller_file,
            callee_file,
            line_number,
            // 口径 B 明令：level=0、confidence=0、candidate_count=0。
            0,
            0,
            0,
            0,
        )
    }

    /// 该边是否是"分层解析唯一确定"的（layered 且候选恰好 1 个）。
    pub fn layered_unique(&self) -> bool {
        self.resolution_level > 0 && self.candidate_count == 1
    }

    /// 该边是否来自歧义调用点（layered 且候选 ≥2）。
    pub fn layered_ambiguous(&self) -> bool {
        self.resolution_level > 0 && self.candidate_count >= 2
    }
}

/// 图节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: Uuid,
    pub function_info: FunctionInfo,
    pub in_degree: usize,
    pub out_degree: usize,
}

/// 图关系
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRelation {
    pub source: Uuid,
    pub target: Uuid,
    pub relation_type: RelationType,
}

/// 关系类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelationType {
    Call,
    Import,
    Inherit,
    Implement,
}

/// 代码图统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphStats {
    pub total_functions: usize,
    pub total_files: usize,
    pub total_languages: usize,
    pub resolved_calls: usize,
    pub unresolved_calls: usize,
    pub languages: HashMap<String, usize>,
}

impl Default for CodeGraphStats {
    fn default() -> Self {
        Self {
            total_functions: 0,
            total_files: 0,
            total_languages: 0,
            resolved_calls: 0,
            unresolved_calls: 0,
            languages: HashMap::new(),
        }
    }
}

/// 基于petgraph的代码图结构
///
/// 警告：结构体里的 `function_to_node` / `node_to_function` 缓存了 petgraph 的 `NodeIndex`，
/// 整体 serde 落盘会把索引一起序列化，重新加载后与实际图结构耦合而失效。
/// 落盘请走 `storage/petgraph_storage.rs` 的 `PetGraphStorage`（按函数/关系列表重建索引）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetCodeGraph {
    /// petgraph有向图
    pub graph: DiGraph<FunctionInfo, CallRelation>,
    /// 函数ID -> 节点索引映射
    pub function_to_node: HashMap<Uuid, NodeIndex>,
    /// 节点索引 -> 函数ID映射
    pub node_to_function: HashMap<NodeIndex, Uuid>,
    /// 函数名 -> 函数ID列表（支持重载）
    pub function_names: HashMap<String, Vec<Uuid>>,
    /// 文件路径 -> 函数ID列表
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,
    /// 统计信息
    pub stats: CodeGraphStats,
}

impl PetCodeGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            function_to_node: HashMap::new(),
            node_to_function: HashMap::new(),
            function_names: HashMap::new(),
            file_functions: HashMap::new(),
            stats: CodeGraphStats::default(),
        }
    }

    /// 添加函数节点
    pub fn add_function(&mut self, function: FunctionInfo) -> NodeIndex {
        let id = function.id;
        let name = function.name.clone();
        let file_path = function.file_path.clone();
        let language = function.language.clone();

        // 添加到petgraph
        let node_index = self.graph.add_node(function.clone());
        
        // 更新映射
        self.function_to_node.insert(id, node_index);
        self.node_to_function.insert(node_index, id);
        
        // 添加到函数名映射
        self.function_names.entry(name.clone()).or_default().push(id);
        
        // 添加到文件映射
        self.file_functions.entry(file_path).or_default().push(id);
        
        // 更新统计信息
        self.stats.total_functions += 1;
        *self.stats.languages.entry(language).or_default() += 1;

        node_index
    }

    /// 索引安全地删除一个函数节点（连带其全部关联边）。
    ///
    /// **必须经由本方法删除 `graph` 中的节点**：petgraph 的 `remove_node` 是 swap-remove，
    /// 会把当前最后一个节点搬移到被删位置。若直接对 `self.graph` 调用 `remove_node`，
    /// `function_to_node` / `node_to_function` 会静默失效，之后按 id 取到的 NodeIndex
    /// 指向另一个函数，导致 `add_call_relation` / `get_callers` / `get_callees` 连错边。
    ///
    /// 返回是否真正删除了一个节点。
    pub fn remove_function(&mut self, function_id: &Uuid) -> bool {
        let node_index = match self.function_to_node.get(function_id).copied() {
            Some(idx) => idx,
            None => return false,
        };

        // remove_node 之后节点数据就没了，先取出来
        let node_info = self
            .graph
            .node_weight(node_index)
            .map(|f| (f.language.clone(), f.file_path.clone()));

        // 随节点级联消失的边要递减调用计数；按 EdgeIndex 去重，避免自环被 Outgoing/Incoming 双计
        let mut incident_edges: Vec<petgraph::graph::EdgeIndex> = self
            .graph
            .edges_directed(node_index, Direction::Outgoing)
            .map(|e| e.id())
            .collect();
        incident_edges.extend(
            self.graph
                .edges_directed(node_index, Direction::Incoming)
                .map(|e| e.id()),
        );
        let mut seen_edges = std::collections::HashSet::new();
        for edge_index in incident_edges {
            if !seen_edges.insert(edge_index) {
                continue;
            }
            match self.graph.edge_weight(edge_index).map(|r| r.is_resolved) {
                Some(true) => {
                    self.stats.resolved_calls = self.stats.resolved_calls.saturating_sub(1)
                }
                Some(false) => {
                    self.stats.unresolved_calls = self.stats.unresolved_calls.saturating_sub(1)
                }
                None => {}
            }
        }

        // DiGraph 的节点索引恒为 0..node_count() 连续，故最后一个索引就是 swap 的搬移来源
        let last_index = NodeIndex::new(self.graph.node_count() - 1);

        self.graph.remove_node(node_index);

        // 清掉被删节点自身的两条映射
        if let Some(stale_id) = self.node_to_function.remove(&node_index) {
            self.function_to_node.remove(&stale_id);
        }
        self.function_to_node.remove(function_id);

        // 修正被搬移的节点：原 last_index 上的节点现在位于 node_index
        if node_index != last_index {
            if let Some(moved_id) = self.node_to_function.remove(&last_index) {
                self.node_to_function.insert(node_index, moved_id);
                self.function_to_node.insert(moved_id, node_index);
            }
        }

        // 统计口径：按实际删除数递减，不在此处之外重复计算
        self.stats.total_functions = self.stats.total_functions.saturating_sub(1);
        if let Some((language, file_path)) = node_info {
            if let Some(count) = self.stats.languages.get_mut(&language) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.stats.languages.remove(&language);
                }
            }
            // 清理文件反向索引
            if let Some(ids) = self.file_functions.get_mut(&file_path) {
                ids.retain(|id| id != function_id);
                if ids.is_empty() {
                    self.file_functions.remove(&file_path);
                }
            }
        }

        // 清理函数名反向索引（支持重载的 Vec）
        for ids in self.function_names.values_mut() {
            ids.retain(|id| id != function_id);
        }
        self.function_names.retain(|_, ids| !ids.is_empty());

        true
    }

    /// 删除指定文件的所有函数及其调用关系（增量更新时使用）
    ///
    /// 逐个走索引安全的 `remove_function`；`stats.total_functions` / `stats.languages` /
    /// 调用计数都在其中按实际删除数修正，此处不再重复递减。
    pub fn remove_functions_by_file(&mut self, file_path: &PathBuf) {
        if let Some(function_ids) = self.file_functions.remove(file_path) {
            for id in &function_ids {
                self.remove_function(id);
            }
        }
    }

    /// 添加调用关系边
    pub fn add_call_relation(&mut self, relation: CallRelation) -> Result<(), String> {
        let caller_node = self.function_to_node.get(&relation.caller_id)
            .ok_or_else(|| format!("Caller function {} not found", relation.caller_id))?;
        let callee_node = self.function_to_node.get(&relation.callee_id)
            .ok_or_else(|| format!("Callee function {} not found", relation.callee_id))?;

        // 检查是否已有相同边（相同 caller, callee, line_number）
        let is_duplicate = self.graph.edges_directed(*caller_node, Direction::Outgoing)
            .any(|e| e.target() == *callee_node && e.weight().line_number == relation.line_number);
        if is_duplicate {
            return Ok(());  // 跳过重复边
        }

        // 添加到petgraph
        self.graph.add_edge(*caller_node, *callee_node, relation.clone());
        
        // 更新统计信息
        if relation.is_resolved {
            self.stats.resolved_calls += 1;
        } else {
            self.stats.unresolved_calls += 1;
        }

        Ok(())
    }

    /// 删除某个 caller 在某一行的全部出边（Phase P2b 交付物 2 的要求 (v)：**禁止叠边**）。
    ///
    /// ⚠ **不要**用它做重写前的清理：同一行可以有多个**不同**的调用
    /// （`first(); second();`、JS 的一行链式调用），按 `(caller, line)` 全删会连带
    /// 抹掉兄弟调用点的边。实测教训：`simple_js_project` 的边数因此从 746 掉到 52。
    /// 重写前的清理请用 [`PetCodeGraph::remove_call_edges_by_call_site`]。
    ///
    /// 用途：整文件失效（`remove_functions_by_file` / 节点删除）之外的"按行清理"场景。
    ///
    /// 统计口径与 `remove_function` 一致：按被删边的 `is_resolved` 递减
    /// `stats.resolved_calls` / `stats.unresolved_calls`，绝不出现负数（`saturating_sub`）。
    /// 返回删除的边数。
    pub fn remove_call_edges_by_caller_line(&mut self, caller_id: &Uuid, line_number: usize) -> usize {
        self._remove_call_edges_where(caller_id, |relation| relation.line_number == line_number)
    }

    /// Phase P2b：删除**同一个调用点**的旧边（重写之前调用，保证幂等且不叠边）。
    ///
    /// 调用点的身份 = `(caller_id, line_number, callee_name)` 三元组：
    /// - 与 `add_call_relation` 的去重键 `(caller, callee, line)` 同属一个调用点，
    ///   所以重复分析同一行同名的调用不会累积出两条边；
    /// - 只删同名旧边 ⇒ 同一行的**其它**调用点（不同 callee 名）不受影响。
    ///
    /// `callee_name` 用 layered 侧的裸名（`site.callee_name`），它与 legacy 写入时用的
    /// `symbol_ref.name()` 是同一个字符串（都是 AST 的末段名），因此两种模式互相重写
    /// 时也能正确命中旧边。
    pub fn remove_call_edges_by_call_site(
        &mut self,
        caller_id: &Uuid,
        line_number: usize,
        callee_name: &str,
    ) -> usize {
        self._remove_call_edges_where(caller_id, |relation| {
            relation.line_number == line_number && relation.callee_name == callee_name
        })
    }

    /// Phase P2b 收口（任务口径 B）：**唯一的调用边写入入口**。
    ///
    /// 四条产边路径 —— legacy 全量（`parser::_analyze_file_calls_for_petgraph`）、
    /// legacy 刷新/增量（`parser::_analyze_file_calls` 与 `incremental::_analyze_file_calls`）、
    /// layered 全量与增量（`resolution::emit_layered_resolution`）—— 都必须经由本方法写边，
    /// 结构上消灭"两套去重逻辑"：过去 legacy 只靠 `add_call_relation` 的
    /// `(caller, callee, line)` 去重，layered 自己做了"先删后写"，于是同一个调用点被
    /// 重新发射时（增量刷新）legacy 会叠出重复边（P2b 实测 `gamma.rs:6` 2 条 vs 全量 1 条）。
    ///
    /// 语义：把 `(caller_id, line_number, 该调用点的 callee 名族)` 这个**调用点**的旧边全部删除，
    /// 然后写入 `edges`。因此"对同一个调用点重复发射 N 次"与发射 1 次的结果**完全相同**（幂等）。
    ///
    /// 为什么删除谓词是 `callee_name == site_name || callee_name ∈ {新边的 callee_name}`：
    /// - 三种写法对 `callee_name` 的记录口径不同 —— layered 写 `site.callee_name`（AST 末段名）、
    ///   legacy 全量写 `callee_info.name`（解析结果名）、legacy 增量写 `call_name`，
    ///   把"新边自己的名字"也放进名字族，才能保证重写时一定命中上一轮的边；
    /// - 只删同名边 ⇒ 同一行的**其它**调用点不受影响。历史教训：按 `(caller, line)` 全删
    ///   把 `simple_js_project` 的边数从 746 削到 52（见
    ///   [`PetCodeGraph::remove_call_edges_by_caller_line`] 的警告）。
    ///
    /// ⚠ **只删"孤儿同名边"，绝不删仍然连在活节点上的同名边**（P2b 收口实测教训）：
    /// legacy 的 callee 解析会走到 `find_functions_by_name()`，而**幽灵节点本身也在图里、
    /// 名字就是被调裸名** —— 于是同一行同名的第二个调用点会"解析成功"并连到第一个调用点
    /// 造的幽灵节点上，`add_call_relation` 按 `(caller, callee, line)` 把这条边判成重复丢掉
    /// （保留 `is_resolved=false`）。若重写时无条件先删同名边，这条边就"活不过重写"，
    /// 于是 `is_resolved` 被翻成 `true` —— 实测：`simple_js_project` resolved 546→664、
    /// `simple_ts_project` 70→72，**违反红线 1（默认 legacy 必须与 P2a 逐字节等价）**。
    /// 加上"callee 已不在图里"这一道校验之后：真正的重复边（caller 节点被复制出来两份、
    /// 旧边指向已被删掉的节点/上一轮的旧节点）照样被清理，legacy 的既有产出零变化。
    ///
    /// 返回实际写入的边数（`add_call_relation` 原有的 `(caller, callee, line)` 去重仍然生效，
    /// 所以"两条完全相同的候选"只会落一条）。
    ///
    /// **适用范围**：只用于"由 AST 调用点发射的边"。下面三类不属于本入口，
    /// 因此不受"唯一写入入口"约束（把它们塞进来反而会破坏各自的去重语义）：
    ///  1. `petgraph_storage.rs` 从缓存反序列化后的原样重建（边已去重过，重写一次等于丢统计）；
    ///  2. `_create_heuristic_calls` / `_create_test_calls` 这类**非调用点**的合成边
    ///     （`line_number` 填的是 caller 的定义行，不是调用行，套 upsert 会把 caller
    ///     定义行上的真实调用点误删）；
    ///  3. `CodeGraph`（非 `PetCodeGraph`）那条老 `build_code_graph` 链路，签名不同、
    ///     无调用点概念。
    ///
    /// 四条 PetCodeGraph 产边路径与其调用点（改造后全部经由本方法）：
    /// - legacy 全量：`parser.rs:2064`（`_analyze_file_calls_for_petgraph`）
    ///   与 `parser.rs:2220`（同函数的未解析分支）
    /// - legacy 刷新：`parser.rs:543` 与 `parser.rs:753`
    /// - layered 全量/刷新：`resolution/mod.rs:566`（幽灵）与 `resolution/mod.rs:604`（候选），
    ///   由 `parser.rs` 与 `incremental.rs` 的 layered 分支共同调用
    /// - 增量（`IncrementalManager`）：`incremental.rs:315` 与 `incremental.rs:495`
    pub fn upsert_call_site_edges(
        &mut self,
        caller_id: &Uuid,
        line_number: usize,
        site_name: &str,
        edges: Vec<CallRelation>,
    ) -> usize {
        // 名字族：本调用点的裸名 + 新候选各自记录的 callee_name。
        let mut names: std::collections::HashSet<String> =
            edges.iter().map(|e| e.callee_name.clone()).collect();
        names.insert(site_name.to_string());

        // 1) 先删：同一 caller、同一行、同名族里，**不属于本批候选**或**已经悬空**的旧边。
        //
        //    为什么必须把"仍在本批里的同一条边"留下：legacy 的 callee 解析会走到
        //    `find_functions_by_name()`，而幽灵节点本身也在图里、名字就是被调裸名 ——
        //    同一行同名的第二个调用点因此会"解析成功"并连到第一个调用点造的幽灵节点上，
        //    `add_call_relation` 按 `(caller, callee, line)` 把它判成重复丢掉（留下 `is_resolved=false`）。
        //    若无条件先删同名边，这条边就活不过重写、`is_resolved` 被翻成 `true` ——
        //    实测 `simple_js_project` resolved 546→664、`simple_ts_project` 70→72（红线 1 违规）。
        //    留同callee + 靠 add_call_relation 去重 ⇒ legacy 逐字节不变；
        //    删"不在本批里"的旧边 ⇒ layered 的候选集变化时旧候选边被真正替换，不叠加。
        let caller_node = match self.function_to_node.get(caller_id).copied() {
            Some(node) => node,
            None => return 0,
        };
        let new_callees: std::collections::HashSet<Uuid> =
            edges.iter().map(|e| e.callee_id).collect();
        let doomed: Vec<petgraph::graph::EdgeIndex> = self
            .graph
            .edges_directed(caller_node, Direction::Outgoing)
            .filter(|edge| {
                let relation = edge.weight();
                if relation.line_number != line_number || !names.contains(&relation.callee_name) {
                    return false;
                }
                // 旧边的 callee 还活着、且本批还要写同一条 ⇒ 留下（交给 add_call_relation 去重）
                let callee_alive = self
                    .function_to_node
                    .get(&relation.callee_id)
                    .is_some_and(|node| self.graph.node_weight(*node).is_some());
                !(callee_alive && new_callees.contains(&relation.callee_id))
            })
            .map(|edge| edge.id())
            .collect();

        for edge_index in doomed {
            match self.graph.edge_weight(edge_index).map(|r| r.is_resolved) {
                Some(true) => {
                    self.stats.resolved_calls = self.stats.resolved_calls.saturating_sub(1)
                }
                Some(false) => {
                    self.stats.unresolved_calls = self.stats.unresolved_calls.saturating_sub(1)
                }
                None => continue,
            }
            self.graph.remove_edge(edge_index);
        }

        // 2) 再写新候选。统计口径（resolved/unresolved 计数）由 add_call_relation 唯一负责。
        let mut written = 0usize;
        for relation in edges {
            if self.add_call_relation(relation).is_ok() {
                written += 1;
            }
        }
        written
    }

    /// 出边删除的共用内核：按谓词挑出 caller 的出边、逐个删、同步统计。
    fn _remove_call_edges_where(
        &mut self,
        caller_id: &Uuid,
        predicate: impl Fn(&CallRelation) -> bool,
    ) -> usize {
        let Some(caller_node) = self.function_to_node.get(caller_id).copied() else {
            return 0;
        };
        let doomed: Vec<petgraph::graph::EdgeIndex> = self
            .graph
            .edges_directed(caller_node, Direction::Outgoing)
            .filter(|edge| predicate(edge.weight()))
            .map(|edge| edge.id())
            .collect();
        let mut removed = 0usize;
        for edge_index in doomed {
            match self.graph.edge_weight(edge_index).map(|r| r.is_resolved) {
                Some(true) => {
                    self.stats.resolved_calls = self.stats.resolved_calls.saturating_sub(1)
                }
                Some(false) => {
                    self.stats.unresolved_calls = self.stats.unresolved_calls.saturating_sub(1)
                }
                None => continue,
            }
            self.graph.remove_edge(edge_index);
            removed += 1;
        }
        removed
    }

    /// 根据函数ID获取节点索引
    pub fn get_node_index(&self, function_id: &Uuid) -> Option<NodeIndex> {
        self.function_to_node.get(function_id).copied()
    }

    /// 根据节点索引获取函数信息
    pub fn get_function(&self, node_index: NodeIndex) -> Option<&FunctionInfo> {
        self.graph.node_weight(node_index)
    }

    /// 根据函数ID获取函数信息
    pub fn get_function_by_id(&self, function_id: &Uuid) -> Option<&FunctionInfo> {
        self.function_to_node.get(function_id)
            .and_then(|&node_index| self.graph.node_weight(node_index))
    }

    /// 获取函数的调用者
    pub fn get_callers(&self, function_id: &Uuid) -> Vec<(&FunctionInfo, &CallRelation)> {
        let mut callers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(&node_index) = self.function_to_node.get(function_id) {
            for edge in self.graph.edges_directed(node_index, Direction::Incoming) {
                let caller_node = edge.source();
                let caller_function = self.graph.node_weight(caller_node).unwrap();
                let relation = edge.weight();
                // Dedup by (caller_id, callee_id, line_number)
                let key = (relation.caller_id, relation.callee_id, relation.line_number);
                if seen.insert(key) {
                    callers.push((caller_function, relation));
                }
            }
        }
        callers
    }

    /// 获取函数调用的函数
    pub fn get_callees(&self, function_id: &Uuid) -> Vec<(&FunctionInfo, &CallRelation)> {
        let mut callees = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(&node_index) = self.function_to_node.get(function_id) {
            for edge in self.graph.edges_directed(node_index, Direction::Outgoing) {
                let callee_node = edge.target();
                let callee_function = self.graph.node_weight(callee_node).unwrap();
                let relation = edge.weight();
                // Dedup by (caller_id, callee_id, line_number)
                let key = (relation.caller_id, relation.callee_id, relation.line_number);
                if seen.insert(key) {
                    callees.push((callee_function, relation));
                }
            }
        }
        callees
    }

    /// 根据函数名查找函数
    pub fn find_functions_by_name(&self, name: &str) -> Vec<&FunctionInfo> {
        self.function_names
            .get(name)
            .map(|ids| ids.iter().filter_map(|id| self.get_function_by_id(id)).collect())
            .unwrap_or_default()
    }

    /// 根据文件路径查找函数
    pub fn find_functions_by_file(&self, file_path: &PathBuf) -> Vec<&FunctionInfo> {
        self.file_functions
            .get(file_path)
            .map(|ids| ids.iter().filter_map(|id| self.get_function_by_id(id)).collect())
            .unwrap_or_default()
    }

    /// 获取调用链（递归）
    pub fn get_call_chain(&self, function_id: &Uuid, max_depth: usize) -> Vec<Vec<Uuid>> {
        let mut chains = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self._get_call_chain_recursive(function_id, &mut chains, &mut visited, 0, max_depth);
        chains
    }

    fn _get_call_chain_recursive(
        &self,
        function_id: &Uuid,
        chains: &mut Vec<Vec<Uuid>>,
        visited: &mut std::collections::HashSet<Uuid>,
        depth: usize,
        max_depth: usize,
    ) {
        if depth >= max_depth || visited.contains(function_id) {
            return;
        }

        visited.insert(*function_id);
        let callees = self.get_callees(function_id);
        
        if callees.is_empty() {
            chains.push(vec![*function_id]);
        } else {
            for (callee_function, _) in callees {
                let mut sub_chains = Vec::new();
                self._get_call_chain_recursive(&callee_function.id, &mut sub_chains, visited, depth + 1, max_depth);
                
                for mut chain in sub_chains {
                    chain.insert(0, *function_id);
                    chains.push(chain);
                }
            }
        }
    }



    /// 导出为DOT格式
    pub fn to_dot(&self) -> String {
        let mut dot = String::from("digraph CodeGraph {\n");
        dot.push_str("    rankdir=TB;\n");
        dot.push_str("    node [shape=box];\n\n");
        
        // 添加节点
        for node_index in self.graph.node_indices() {
            if let Some(function) = self.graph.node_weight(node_index) {
                let node_id = function.id.to_string().replace("-", "_");
                let label = format!("{}\\n{}", function.name, function.file_path.display());
                dot.push_str(&format!("    {} [label=\"{}\"];\n", node_id, label));
            }
        }
        
        // 添加边
        for edge in self.graph.edge_indices() {
            if let Some((source, target)) = self.graph.edge_endpoints(edge) {
                if let (Some(caller), Some(callee)) = (self.graph.node_weight(source), self.graph.node_weight(target)) {
                    let caller_id = caller.id.to_string().replace("-", "_");
                    let callee_id = callee.id.to_string().replace("-", "_");
                    if let Some(relation) = self.graph.edge_weight(edge) {
                        let style = if relation.is_resolved { "" } else { " [style=dashed]" };
                        dot.push_str(&format!("    {} -> {}{};\n", caller_id, callee_id, style));
                    }
                }
            }
        }
        
        dot.push_str("}\n");
        dot
    }

    /// 导出为JSON格式
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// 从JSON格式加载
    pub fn from_json(json_str: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json_str)
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> &CodeGraphStats {
        &self.stats
    }

    /// 更新统计信息
    ///
    /// 注意：`resolved_calls` / `unresolved_calls` 现在按 `graph.edge_weights()` 全量重算
    /// （一次 O(E) 遍历），因此即使历史持久化文件里存了虚高数字也会被纠正；
    /// 请在增量批次结束时调用，不要放进单条删除/添加的热路径。
    /// `total_files` / `total_languages` 的重算保持原样。
    pub fn update_stats(&mut self) {
        self.stats.total_files = self.file_functions.len();
        self.stats.total_languages = self.stats.languages.len();

        let mut resolved_calls = 0usize;
        let mut unresolved_calls = 0usize;
        for relation in self.graph.edge_weights() {
            if relation.is_resolved {
                resolved_calls += 1;
            } else {
                unresolved_calls += 1;
            }
        }
        self.stats.resolved_calls = resolved_calls;
        self.stats.unresolved_calls = unresolved_calls;
    }

    /// 获取所有函数
    pub fn get_all_functions(&self) -> Vec<&FunctionInfo> {
        self.graph.node_weights().collect()
    }

    /// 获取所有调用关系
    pub fn get_all_call_relations(&self) -> Vec<&CallRelation> {
        self.graph.edge_weights().collect()
    }

    /// 检查是否存在循环依赖
    pub fn has_cycles(&self) -> bool {
        petgraph::algo::is_cyclic_directed(&self.graph)
    }

    /// 获取拓扑排序
    pub fn topological_sort(&self) -> Result<Vec<NodeIndex>, petgraph::algo::Cycle<NodeIndex>> {
        petgraph::algo::toposort(&self.graph, None)
    }

    /// 获取强连通分量
    pub fn strongly_connected_components(&self) -> Vec<Vec<NodeIndex>> {
        petgraph::algo::kosaraju_scc(&self.graph)
    }
}

impl Default for PetCodeGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// 类信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassInfo {
    pub id: Uuid,
    pub name: String,
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub namespace: String,
    pub language: String,
    pub class_type: ClassType,
    pub parent_class: Option<String>,
    pub implemented_interfaces: Vec<String>,
    pub member_functions: Vec<Uuid>,
    pub member_variables: Vec<String>,
}

/// 类类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClassType {
    Class,
    Struct,
    Interface,
    Trait,
    Enum,
}

/// 实体节点（可以是类或函数）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityNode {
    Class(ClassInfo),
    Function(FunctionInfo),
}

/// 实体边类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityEdgeType {
    Contains,      // 类包含函数
    Inherits,      // 类继承类
    Implements,    // 类实现接口
    Imports,       // 导入关系
    DefinesIn,     // 在文件中定义
}

/// 实体边
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityEdge {
    pub source: Uuid,
    pub target: Uuid,
    pub edge_type: EntityEdgeType,
    pub metadata: Option<serde_json::Value>,
}

/// 实体图（类、函数等实体的结构关系图）
///
/// 警告：`entity_to_node` / `node_to_entity` 缓存了 petgraph 的 `NodeIndex`，
/// 整体 serde 落盘会把索引一起序列化，重新加载后与实际图结构耦合而失效；
/// 删除节点请走 `remove_entity`（内部会修正 swap 搬移的映射）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityGraph {
    /// petgraph有向图
    pub graph: DiGraph<EntityNode, EntityEdge>,
    /// 实体ID -> 节点索引映射
    pub entity_to_node: HashMap<Uuid, NodeIndex>,
    /// 节点索引 -> 实体ID映射
    pub node_to_entity: HashMap<NodeIndex, Uuid>,
    /// 类名 -> 类ID列表
    pub class_names: HashMap<String, Vec<Uuid>>,
    /// 文件路径 -> 类ID列表
    pub file_classes: HashMap<PathBuf, Vec<Uuid>>,
    /// 统计信息
    pub stats: EntityGraphStats,
}

/// 实体图统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityGraphStats {
    pub total_classes: usize,
    pub total_functions: usize,
    pub total_files: usize,
    pub total_languages: usize,
    pub languages: HashMap<String, usize>,
}

impl Default for EntityGraphStats {
    fn default() -> Self {
        Self {
            total_classes: 0,
            total_functions: 0,
            total_files: 0,
            total_languages: 0,
            languages: HashMap::new(),
        }
    }
}

impl EntityGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            entity_to_node: HashMap::new(),
            node_to_entity: HashMap::new(),
            class_names: HashMap::new(),
            file_classes: HashMap::new(),
            stats: EntityGraphStats::default(),
        }
    }

    /// 添加类节点
    pub fn add_class(&mut self, class: ClassInfo) -> NodeIndex {
        let id = class.id;
        let name = class.name.clone();
        let file_path = class.file_path.clone();
        let language = class.language.clone();

        // 添加到petgraph
        let node_index = self.graph.add_node(EntityNode::Class(class.clone()));
        
        // 更新映射
        self.entity_to_node.insert(id, node_index);
        self.node_to_entity.insert(node_index, id);
        
        // 添加到类名映射
        self.class_names.entry(name.clone()).or_default().push(id);
        
        // 添加到文件映射
        self.file_classes.entry(file_path).or_default().push(id);
        
        // 更新统计信息
        self.stats.total_classes += 1;
        *self.stats.languages.entry(language).or_default() += 1;

        node_index
    }

    /// 添加函数节点
    pub fn add_function(&mut self, function: FunctionInfo) -> NodeIndex {
        let id = function.id;
        let _file_path = function.file_path.clone();
        let language = function.language.clone();

        // 添加到petgraph
        let node_index = self.graph.add_node(EntityNode::Function(function.clone()));
        
        // 更新映射
        self.entity_to_node.insert(id, node_index);
        self.node_to_entity.insert(node_index, id);
        
        // 更新统计信息
        self.stats.total_functions += 1;
        *self.stats.languages.entry(language).or_default() += 1;

        node_index
    }

    /// 添加实体边
    pub fn add_edge(&mut self, edge: EntityEdge) -> Result<(), String> {
        let source_node = self.entity_to_node.get(&edge.source)
            .ok_or_else(|| format!("Source entity {} not found", edge.source))?;
        let target_node = self.entity_to_node.get(&edge.target)
            .ok_or_else(|| format!("Target entity {} not found", edge.target))?;

        // 添加到petgraph
        self.graph.add_edge(*source_node, *target_node, edge);
        Ok(())
    }

    /// 根据实体ID获取节点索引
    pub fn get_node_index(&self, entity_id: &Uuid) -> Option<NodeIndex> {
        self.entity_to_node.get(entity_id).copied()
    }

    /// 根据节点索引获取实体信息
    pub fn get_entity(&self, node_index: NodeIndex) -> Option<&EntityNode> {
        self.graph.node_weight(node_index)
    }

    /// 根据实体ID获取实体信息
    pub fn get_entity_by_id(&self, entity_id: &Uuid) -> Option<&EntityNode> {
        self.entity_to_node.get(entity_id)
            .and_then(|&node_index| self.graph.node_weight(node_index))
    }

    /// 根据类ID获取类信息
    pub fn get_class_by_id(&self, class_id: &Uuid) -> Option<&ClassInfo> {
        self.get_entity_by_id(class_id).and_then(|entity| {
            if let EntityNode::Class(class) = entity {
                Some(class)
            } else {
                None
            }
        })
    }

    /// 根据类名查找类
    pub fn find_classes_by_name(&self, name: &str) -> Vec<&ClassInfo> {
        self.class_names
            .get(name)
            .map(|ids| ids.iter().filter_map(|id| {
                self.get_entity_by_id(id).and_then(|entity| {
                    if let EntityNode::Class(class) = entity {
                        Some(class)
                    } else {
                        None
                    }
                })
            }).collect())
            .unwrap_or_default()
    }

    /// 根据文件路径查找类
    pub fn find_classes_by_file(&self, file_path: &PathBuf) -> Vec<&ClassInfo> {
        self.file_classes
            .get(file_path)
            .map(|ids| ids.iter().filter_map(|id| {
                self.get_entity_by_id(id).and_then(|entity| {
                    if let EntityNode::Class(class) = entity {
                        Some(class)
                    } else {
                        None
                    }
                })
            }).collect())
            .unwrap_or_default()
    }

    /// 获取类的成员函数
    pub fn get_class_members(&self, class_id: &Uuid) -> Vec<&FunctionInfo> {
        if let Some(EntityNode::Class(class)) = self.get_entity_by_id(class_id) {
            class.member_functions.iter().filter_map(|func_id| {
                self.get_entity_by_id(func_id).and_then(|entity| {
                    if let EntityNode::Function(func) = entity {
                        Some(func)
                    } else {
                        None
                    }
                })
            }).collect()
        } else {
            Vec::new()
        }
    }

    /// 索引安全地从图中移除节点，并修正被 petgraph swap-remove 搬移的映射。
    ///
    /// 同 `PetCodeGraph::remove_function`：**不要**直接对 `self.graph` 调用 `remove_node`，
    /// 否则 `entity_to_node` / `node_to_entity` 会静默失效（末位节点被搬移到被删位置）。
    fn remove_node_index(&mut self, node_index: NodeIndex, entity_id: &Uuid) {
        // DiGraph 节点索引恒为 0..node_count() 连续，故最后一个索引就是 swap 的搬移来源
        let last_index = NodeIndex::new(self.graph.node_count() - 1);

        self.graph.remove_node(node_index);

        if let Some(stale_id) = self.node_to_entity.remove(&node_index) {
            self.entity_to_node.remove(&stale_id);
        }
        self.entity_to_node.remove(entity_id);

        // 修正被搬移的节点：原 last_index 上的实体现在位于 node_index
        if node_index != last_index {
            if let Some(moved_id) = self.node_to_entity.remove(&last_index) {
                self.node_to_entity.insert(node_index, moved_id);
                self.entity_to_node.insert(moved_id, node_index);
            }
        }
    }

    /// 移除实体及其相关边
    ///
    /// 顺序：先取出节点信息并修正统计 → `remove_node` → 清两条映射 → 修正被搬移的映射项。
    pub fn remove_entity(&mut self, entity_id: &Uuid) -> bool {
        let node_index = match self.entity_to_node.get(entity_id).copied() {
            Some(idx) => idx,
            None => return false,
        };

        // 先把要删的节点信息取出来再动手（remove_node 之后就取不到了）
        let class_info = match self.graph.node_weight(node_index) {
            Some(EntityNode::Class(class)) => {
                Some((class.name.clone(), class.file_path.clone()))
            }
            _ => None,
        };
        let node_exists = self.graph.node_weight(node_index).is_some();
        let is_function = matches!(
            self.graph.node_weight(node_index),
            Some(EntityNode::Function(_))
        );

        if let Some((class_name, class_file)) = class_info {
            // 清理类名索引
            if let Some(ids) = self.class_names.get_mut(&class_name) {
                ids.retain(|id| id != entity_id);
                if ids.is_empty() {
                    self.class_names.remove(&class_name);
                }
            }
            // 清理文件索引
            if let Some(ids) = self.file_classes.get_mut(&class_file) {
                ids.retain(|id| id != entity_id);
                if ids.is_empty() {
                    self.file_classes.remove(&class_file);
                }
            }
        }

        if node_exists {
            if is_function {
                self.stats.total_functions = self.stats.total_functions.saturating_sub(1);
            } else {
                self.stats.total_classes = self.stats.total_classes.saturating_sub(1);
            }
        }

        // 从图中移除节点（会自动移除相关边）并修正映射
        self.remove_node_index(node_index, entity_id);

        true
    }

    /// 更新统计信息
    pub fn update_stats(&mut self) {
        self.stats.total_files = self.file_classes.len();
        self.stats.total_languages = self.stats.languages.len();
    }

    /// 获取所有类
    pub fn get_all_classes(&self) -> Vec<&ClassInfo> {
        self.graph.node_weights().filter_map(|entity| {
            if let EntityNode::Class(class) = entity {
                Some(class)
            } else {
                None
            }
        }).collect()
    }

    /// 获取所有函数
    pub fn get_all_functions(&self) -> Vec<&FunctionInfo> {
        self.graph.node_weights().filter_map(|entity| {
            if let EntityNode::Function(func) = entity {
                Some(func)
            } else {
                None
            }
        }).collect()
    }

    /// 导出为JSON格式
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// 从JSON格式加载
    pub fn from_json(json_str: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json_str)
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// 文件元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub md5: String,
    pub last_updated: chrono::DateTime<chrono::Utc>,
    pub file_size: u64,
    pub language: String,
}

/// 文件索引
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIndex {
    /// 文件路径 -> 实体ID列表
    pub file_entities: HashMap<PathBuf, Vec<Uuid>>,
    /// 文件路径 -> 函数ID列表
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,
    /// 文件路径 -> 类ID列表
    pub file_classes: HashMap<PathBuf, Vec<Uuid>>,
}

impl Default for FileIndex {
    fn default() -> Self {
        Self {
            file_entities: HashMap::new(),
            file_functions: HashMap::new(),
            file_classes: HashMap::new(),
        }
    }
}

impl FileIndex {
    /// 为文件添加实体
    pub fn add_entity(&mut self, file_path: &PathBuf, entity_id: Uuid) {
        self.file_entities.entry(file_path.clone()).or_default().push(entity_id);
    }

    /// 为文件添加函数
    pub fn add_function(&mut self, file_path: &PathBuf, function_id: Uuid) {
        self.file_functions.entry(file_path.clone()).or_default().push(function_id);
    }

    /// 为文件添加类
    pub fn add_class(&mut self, file_path: &PathBuf, class_id: Uuid) {
        self.file_classes.entry(file_path.clone()).or_default().push(class_id);
    }

    /// 获取文件的所有实体ID
    pub fn get_all_entity_ids(&self, file_path: &PathBuf) -> Vec<Uuid> {
        self.file_entities.get(file_path).cloned().unwrap_or_default()
    }

    /// 获取文件的所有函数ID
    pub fn get_all_function_ids(&self, file_path: &PathBuf) -> Vec<Uuid> {
        self.file_functions.get(file_path).cloned().unwrap_or_default()
    }

    /// 获取文件的所有类ID
    pub fn get_all_class_ids(&self, file_path: &PathBuf) -> Vec<Uuid> {
        self.file_classes.get(file_path).cloned().unwrap_or_default()
    }

    /// 重建文件的索引
    pub fn rebuild_for_file(&mut self, file_path: &PathBuf, classes: Vec<Uuid>, functions: Vec<Uuid>) {
        // 移除旧的索引
        self.file_entities.remove(file_path);
        self.file_functions.remove(file_path);
        self.file_classes.remove(file_path);

        // 添加新的索引
        for class_id in classes {
            self.add_class(file_path, class_id);
            self.add_entity(file_path, class_id);
        }
        for function_id in functions {
            self.add_function(file_path, function_id);
            self.add_entity(file_path, function_id);
        }
    }

    /// 移除文件的所有索引
    pub fn remove_file(&mut self, file_path: &PathBuf) {
        self.file_entities.remove(file_path);
        self.file_functions.remove(file_path);
        self.file_classes.remove(file_path);
    }
}

/// 代码片段索引
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetIndex {
    /// 实体ID -> 代码片段信息
    pub entity_snippets: HashMap<Uuid, SnippetInfo>,
    /// 文件路径 -> 行范围 -> 代码片段缓存
    pub snippet_cache: HashMap<(PathBuf, usize, usize), String>,
}

/// 代码片段信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnippetInfo {
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub cached_content: Option<String>,
}

impl Default for SnippetIndex {
    fn default() -> Self {
        Self {
            entity_snippets: HashMap::new(),
            snippet_cache: HashMap::new(),
        }
    }
}

impl SnippetIndex {
    /// 添加代码片段信息
    pub fn add_snippet(&mut self, entity_id: Uuid, snippet_info: SnippetInfo) {
        self.entity_snippets.insert(entity_id, snippet_info);
    }

    /// 获取代码片段信息
    pub fn get_snippet_info(&self, entity_id: &Uuid) -> Option<&SnippetInfo> {
        self.entity_snippets.get(entity_id)
    }

    /// 缓存代码片段内容
    pub fn cache_snippet(&mut self, file_path: &PathBuf, line_start: usize, line_end: usize, content: String) {
        self.snippet_cache.insert((file_path.clone(), line_start, line_end), content);
    }

    /// 获取缓存的代码片段
    pub fn get_cached_snippet(&self, file_path: &PathBuf, line_start: usize, line_end: usize) -> Option<&String> {
        self.snippet_cache.get(&(file_path.clone(), line_start, line_end))
    }

    /// 移除实体的代码片段
    pub fn remove_snippet(&mut self, entity_id: &Uuid) {
        if let Some(snippet_info) = self.entity_snippets.remove(entity_id) {
            // 同时移除缓存
            self.snippet_cache.remove(&(snippet_info.file_path, snippet_info.line_start, snippet_info.line_end));
        }
    }

    /// 清理文件相关的缓存
    pub fn clear_file_cache(&mut self, file_path: &PathBuf) {
        self.snippet_cache.retain(|(path, _, _), _| path != file_path);
    }
}