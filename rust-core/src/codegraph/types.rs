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

    /// 添加调用关系边
    pub fn add_call_relation(&mut self, relation: CallRelation) -> Result<(), String> {
        let caller_node = self.function_to_node.get(&relation.caller_id)
            .ok_or_else(|| format!("Caller function {} not found", relation.caller_id))?;
        let callee_node = self.function_to_node.get(&relation.callee_id)
            .ok_or_else(|| format!("Callee function {} not found", relation.callee_id))?;

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
        if let Some(&node_index) = self.function_to_node.get(function_id) {
            for edge in self.graph.edges_directed(node_index, Direction::Incoming) {
                let caller_node = edge.source();
                let caller_function = self.graph.node_weight(caller_node).unwrap();
                let relation = edge.weight();
                callers.push((caller_function, relation));
            }
        }
        callers
    }

    /// 获取函数调用的函数
    pub fn get_callees(&self, function_id: &Uuid) -> Vec<(&FunctionInfo, &CallRelation)> {
        let mut callees = Vec::new();
        if let Some(&node_index) = self.function_to_node.get(function_id) {
            for edge in self.graph.edges_directed(node_index, Direction::Outgoing) {
                let callee_node = edge.target();
                let callee_function = self.graph.node_weight(callee_node).unwrap();
                let relation = edge.weight();
                callees.push((callee_function, relation));
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
    pub fn update_stats(&mut self) {
        self.stats.total_files = self.file_functions.len();
        self.stats.total_languages = self.stats.languages.len();
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

    /// 移除实体及其相关边
    pub fn remove_entity(&mut self, entity_id: &Uuid) -> bool {
        if let Some(&node_index) = self.entity_to_node.get(entity_id) {
            // 获取实体信息用于清理索引
            if let Some(entity) = self.graph.node_weight(node_index) {
                match entity {
                    EntityNode::Class(class) => {
                        // 清理类名索引
                        if let Some(ids) = self.class_names.get_mut(&class.name) {
                            ids.retain(|id| id != entity_id);
                            if ids.is_empty() {
                                self.class_names.remove(&class.name);
                            }
                        }
                        // 清理文件索引
                        if let Some(ids) = self.file_classes.get_mut(&class.file_path) {
                            ids.retain(|id| id != entity_id);
                            if ids.is_empty() {
                                self.file_classes.remove(&class.file_path);
                            }
                        }
                        self.stats.total_classes = self.stats.total_classes.saturating_sub(1);
                    },
                    EntityNode::Function(_) => {
                        self.stats.total_functions = self.stats.total_functions.saturating_sub(1);
                    }
                }
            }

            // 从图中移除节点（会自动移除相关边）
            self.graph.remove_node(node_index);
            
            // 清理映射
            self.entity_to_node.remove(entity_id);
            self.node_to_entity.remove(&node_index);
            
            true
        } else {
            false
        }
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