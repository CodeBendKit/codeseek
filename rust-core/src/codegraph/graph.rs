use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;
use crate::codegraph::types::{FunctionInfo, CallRelation, GraphRelation, CodeGraphStats};

/// 代码图核心结构
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeGraph {
    /// 函数ID -> 函数信息
    pub functions: HashMap<Uuid, FunctionInfo>,
    /// 函数名 -> 函数ID列表（支持重载）
    pub function_names: HashMap<String, Vec<Uuid>>,
    /// 文件路径 -> 函数ID列表
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,
    /// 调用关系
    pub call_relations: Vec<CallRelation>,
    /// 图关系
    pub graph_relations: Vec<GraphRelation>,
    /// 统计信息
    pub stats: CodeGraphStats,
}

impl CodeGraph {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            function_names: HashMap::new(),
            file_functions: HashMap::new(),
            call_relations: Vec::new(),
            graph_relations: Vec::new(),
            stats: CodeGraphStats::default(),
        }
    }

    /// 添加函数
    pub fn add_function(&mut self, function: FunctionInfo) {
        let id = function.id;
        let name = function.name.clone();
        let file_path = function.file_path.clone();
        let language = function.language.clone();

        // 添加到函数映射
        self.functions.insert(id, function);
        
        // 添加到函数名映射
        self.function_names.entry(name.clone()).or_default().push(id);
        
        // 添加到文件映射
        self.file_functions.entry(file_path).or_default().push(id);
        
        // 更新统计信息
        self.stats.total_functions += 1;
        *self.stats.languages.entry(language).or_default() += 1;
        
        // 更新文件统计
        self.stats.total_files = self.file_functions.len();
        self.stats.total_languages = self.stats.languages.len();
    }

    /// 添加调用关系
    pub fn add_call_relation(&mut self, relation: CallRelation) {
        let is_resolved = relation.is_resolved;
        self.call_relations.push(relation);
        
        // 更新统计信息
        if is_resolved {
            self.stats.resolved_calls += 1;
        } else {
            self.stats.unresolved_calls += 1;
        }
    }

    /// 添加图关系
    pub fn add_graph_relation(&mut self, relation: GraphRelation) {
        self.graph_relations.push(relation);
    }

    /// 根据函数名查找函数
    pub fn find_functions_by_name(&self, name: &str) -> Vec<&FunctionInfo> {
        self.function_names
            .get(name)
            .map(|ids| ids.iter().filter_map(|id| self.functions.get(id)).collect())
            .unwrap_or_default()
    }

    /// 根据文件路径查找函数
    pub fn find_functions_by_file(&self, file_path: &PathBuf) -> Vec<&FunctionInfo> {
        self.file_functions
            .get(file_path)
            .map(|ids| ids.iter().filter_map(|id| self.functions.get(id)).collect())
            .unwrap_or_default()
    }

    /// 获取函数的调用者
    pub fn get_callers(&self, function_id: &Uuid) -> Vec<&CallRelation> {
        self.call_relations
            .iter()
            .filter(|rel| rel.callee_id == *function_id)
            .collect()
    }

    /// 获取函数调用的函数
    pub fn get_callees(&self, function_id: &Uuid) -> Vec<&CallRelation> {
        self.call_relations
            .iter()
            .filter(|rel| rel.caller_id == *function_id)
            .collect()
    }

    /// 获取调用链（递归）
    pub fn get_call_chain(&self, function_id: &Uuid, max_depth: usize) -> Vec<Vec<Uuid>> {
        let mut chains = Vec::new();
        let mut visited = HashSet::new();
        self._get_call_chain_recursive(function_id, &mut chains, &mut visited, 0, max_depth);
        
        // 如果没有找到任何调用链，至少返回包含函数本身的链
        if chains.is_empty() {
            chains.push(vec![*function_id]);
        }
        
        chains
    }

    fn _get_call_chain_recursive(
        &self,
        function_id: &Uuid,
        chains: &mut Vec<Vec<Uuid>>,
        visited: &mut HashSet<Uuid>,
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
            for callee in callees {
                let mut sub_chains = Vec::new();
                self._get_call_chain_recursive(&callee.callee_id, &mut sub_chains, visited, depth + 1, max_depth);
                
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
        for function in self.functions.values() {
            let node_id = function.name.replace(" ", "_").replace("-", "_");
            let label = format!("{}\\n{}", function.name, function.file_path.display());
            dot.push_str(&format!("    {} [label=\"{}\"];\n", node_id, label));
        }
        
        // 添加边
        for relation in &self.call_relations {
            let caller_name = relation.caller_name.replace(" ", "_").replace("-", "_");
            let callee_name = relation.callee_name.replace(" ", "_").replace("-", "_");
            let style = if relation.is_resolved { "" } else { " [style=dashed]" };
            dot.push_str(&format!("    {} -> {}{};\n", caller_name, callee_name, style));
        }
        
        dot.push_str("}\n");
        dot
    }

    /// 导出为JSON格式
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
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
}

impl Default for CodeGraph {
    fn default() -> Self {
        Self::new()
    }
}