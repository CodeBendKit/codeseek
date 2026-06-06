use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::codegraph::types::{PetCodeGraph, FunctionInfo, CallRelation, CodeGraphStats};

/// petgraph代码图存储格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetGraphStorage {
    /// 函数信息列表
    pub functions: Vec<FunctionInfo>,
    /// 调用关系列表
    pub call_relations: Vec<CallRelation>,
    /// 函数名映射
    pub function_names: HashMap<String, Vec<Uuid>>,
    /// 文件映射
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,
    /// 统计信息
    pub stats: CodeGraphStats,
}

impl PetGraphStorage {
    /// 从PetCodeGraph创建存储格式
    pub fn from_petgraph(code_graph: &PetCodeGraph) -> Self {
        let functions: Vec<FunctionInfo> = code_graph.get_all_functions().into_iter().cloned().collect();
        let call_relations: Vec<CallRelation> = code_graph.get_all_call_relations().into_iter().cloned().collect();
        
        Self {
            functions,
            call_relations,
            function_names: code_graph.function_names.clone(),
            file_functions: code_graph.file_functions.clone(),
            stats: code_graph.stats.clone(),
        }
    }

    /// 转换为PetCodeGraph
    pub fn to_petgraph(&self) -> PetCodeGraph {
        let mut code_graph = PetCodeGraph::new();
        
        // 添加所有函数
        for function in &self.functions {
            code_graph.add_function(function.clone());
        }
        
        // 添加所有调用关系
        for relation in &self.call_relations {
            if let Err(_e) = code_graph.add_call_relation(relation.clone()) {
                // Ignore errors for now
            }
        }
        
        // 恢复映射和统计信息
        code_graph.function_names = self.function_names.clone();
        code_graph.file_functions = self.file_functions.clone();
        code_graph.stats = self.stats.clone();
        
        code_graph
    }
}

/// petgraph代码图存储管理器
pub struct PetGraphStorageManager;

impl PetGraphStorageManager {
    /// 保存代码图到文件
    pub fn save_to_file(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let storage = PetGraphStorage::from_petgraph(code_graph);
        let json = serde_json::to_string_pretty(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))?;
        
        fs::write(file_path, json)
            .map_err(|e| format!("Failed to write file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }

    /// 从文件加载代码图
    pub fn load_from_file(file_path: &Path) -> Result<PetCodeGraph, String> {
        let json = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
        
        let storage: PetGraphStorage = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to deserialize code graph: {}", e))?;
        
        Ok(storage.to_petgraph())
    }

    /// 保存代码图到JSON字符串
    pub fn save_to_json(code_graph: &PetCodeGraph) -> Result<String, String> {
        let storage = PetGraphStorage::from_petgraph(code_graph);
        serde_json::to_string_pretty(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))
    }

    /// 从JSON字符串加载代码图
    pub fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String> {
        let storage: PetGraphStorage = serde_json::from_str(json_str)
            .map_err(|e| format!("Failed to deserialize code graph: {}", e))?;
        
        Ok(storage.to_petgraph())
    }

    /// 保存代码图为二进制格式
    pub fn save_to_binary(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let storage = PetGraphStorage::from_petgraph(code_graph);
        let binary = bincode::serialize(&storage)
            .map_err(|e| format!("Failed to serialize code graph: {}", e))?;
        
        fs::write(file_path, binary)
            .map_err(|e| format!("Failed to write file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }

    /// 从二进制文件加载代码图
    pub fn load_from_binary(file_path: &Path) -> Result<PetCodeGraph, String> {
        let binary = fs::read(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
        
        let storage: PetGraphStorage = bincode::deserialize(&binary)
            .map_err(|e| format!("Failed to deserialize code graph: {}", e))?;
        
        Ok(storage.to_petgraph())
    }

    /// 导出为GraphML格式（用于可视化工具）
    pub fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let mut graphml = String::new();
        graphml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        graphml.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n");
        
        // 定义节点属性
        graphml.push_str("  <key id=\"name\" for=\"node\" attr.name=\"name\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"file\" for=\"node\" attr.name=\"file\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"language\" for=\"node\" attr.name=\"language\" attr.type=\"string\"/>\n");
        graphml.push_str("  <key id=\"line_start\" for=\"node\" attr.name=\"line_start\" attr.type=\"int\"/>\n");
        graphml.push_str("  <key id=\"line_end\" for=\"node\" attr.name=\"line_end\" attr.type=\"int\"/>\n");
        
        // 定义边属性
        graphml.push_str("  <key id=\"line_number\" for=\"edge\" attr.name=\"line_number\" attr.type=\"int\"/>\n");
        graphml.push_str("  <key id=\"is_resolved\" for=\"edge\" attr.name=\"is_resolved\" attr.type=\"boolean\"/>\n");
        
        graphml.push_str("  <graph id=\"codegraph\" edgedefault=\"directed\">\n");
        
        // 添加节点
        for (node_index, function) in code_graph.graph.node_indices().zip(code_graph.graph.node_weights()) {
            graphml.push_str(&format!("    <node id=\"n{}\">\n", node_index.index()));
            graphml.push_str(&format!("      <data key=\"name\">{}</data>\n", function.name));
            graphml.push_str(&format!("      <data key=\"file\">{}</data>\n", function.file_path.display()));
            graphml.push_str(&format!("      <data key=\"language\">{}</data>\n", function.language));
            graphml.push_str(&format!("      <data key=\"line_start\">{}</data>\n", function.line_start));
            graphml.push_str(&format!("      <data key=\"line_end\">{}</data>\n", function.line_end));
            graphml.push_str("    </node>\n");
        }
        
        // 添加边
        for (edge_index, edge) in code_graph.graph.edge_indices().zip(code_graph.graph.edge_weights()) {
            if let Some((source, target)) = code_graph.graph.edge_endpoints(edge_index) {
                graphml.push_str(&format!("    <edge id=\"e{}\" source=\"n{}\" target=\"n{}\">\n", 
                    edge_index.index(), source.index(), target.index()));
                graphml.push_str(&format!("      <data key=\"line_number\">{}</data>\n", edge.line_number));
                graphml.push_str(&format!("      <data key=\"is_resolved\">{}</data>\n", edge.is_resolved));
                graphml.push_str("    </edge>\n");
            }
        }
        
        graphml.push_str("  </graph>\n");
        graphml.push_str("</graphml>\n");
        
        fs::write(file_path, graphml)
            .map_err(|e| format!("Failed to write GraphML file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }

    /// 导出为GEXF格式（用于Gephi等工具）
    pub fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String> {
        let mut gexf = String::new();
        gexf.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        gexf.push_str("<gexf xmlns=\"http://www.gexf.net/1.3\" version=\"1.3\">\n");
        gexf.push_str("  <meta lastmodifieddate=\"2024-01-01\">\n");
        gexf.push_str("    <creator>CodeGraph Exporter</creator>\n");
        gexf.push_str("    <description>Code dependency graph</description>\n");
        gexf.push_str("  </meta>\n");
        gexf.push_str("  <graph mode=\"static\" defaultedgetype=\"directed\">\n");
        
        // 定义节点属性
        gexf.push_str("    <attributes class=\"node\">\n");
        gexf.push_str("      <attribute id=\"0\" title=\"name\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"1\" title=\"file\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"2\" title=\"language\" type=\"string\"/>\n");
        gexf.push_str("      <attribute id=\"3\" title=\"line_start\" type=\"integer\"/>\n");
        gexf.push_str("      <attribute id=\"4\" title=\"line_end\" type=\"integer\"/>\n");
        gexf.push_str("    </attributes>\n");
        
        // 定义边属性
        gexf.push_str("    <attributes class=\"edge\">\n");
        gexf.push_str("      <attribute id=\"0\" title=\"line_number\" type=\"integer\"/>\n");
        gexf.push_str("      <attribute id=\"1\" title=\"is_resolved\" type=\"boolean\"/>\n");
        gexf.push_str("    </attributes>\n");
        
        gexf.push_str("    <nodes>\n");
        
        // 添加节点
        for (node_index, function) in code_graph.graph.node_indices().zip(code_graph.graph.node_weights()) {
            gexf.push_str(&format!("      <node id=\"{}\" label=\"{}\">\n", node_index.index(), function.name));
            gexf.push_str("        <attvalues>\n");
            gexf.push_str(&format!("          <attvalue for=\"0\" value=\"{}\"/>\n", function.name));
            gexf.push_str(&format!("          <attvalue for=\"1\" value=\"{}\"/>\n", function.file_path.display()));
            gexf.push_str(&format!("          <attvalue for=\"2\" value=\"{}\"/>\n", function.language));
            gexf.push_str(&format!("          <attvalue for=\"3\" value=\"{}\"/>\n", function.line_start));
            gexf.push_str(&format!("          <attvalue for=\"4\" value=\"{}\"/>\n", function.line_end));
            gexf.push_str("        </attvalues>\n");
            gexf.push_str("      </node>\n");
        }
        
        gexf.push_str("    </nodes>\n");
        gexf.push_str("    <edges>\n");
        
        // 添加边
        for (edge_index, edge) in code_graph.graph.edge_indices().zip(code_graph.graph.edge_weights()) {
            if let Some((source, target)) = code_graph.graph.edge_endpoints(edge_index) {
                gexf.push_str(&format!("      <edge id=\"{}\" source=\"{}\" target=\"{}\">\n", 
                    edge_index.index(), source.index(), target.index()));
                gexf.push_str("        <attvalues>\n");
                gexf.push_str(&format!("          <attvalue for=\"0\" value=\"{}\"/>\n", edge.line_number));
                gexf.push_str(&format!("          <attvalue for=\"1\" value=\"{}\"/>\n", edge.is_resolved));
                gexf.push_str("        </attvalues>\n");
                gexf.push_str("      </edge>\n");
            }
        }
        
        gexf.push_str("    </edges>\n");
        gexf.push_str("  </graph>\n");
        gexf.push_str("</gexf>\n");
        
        fs::write(file_path, gexf)
            .map_err(|e| format!("Failed to write GEXF file {}: {}", file_path.display(), e))?;
        
        Ok(())
    }
} 

impl crate::storage::traits::GraphSerializer for PetGraphStorageManager {
    fn save_to_file(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::save_to_file(code_graph, file_path)
    }

    fn load_from_file(file_path: &std::path::Path) -> Result<PetCodeGraph, String> {
        Self::load_from_file(file_path)
    }

    fn save_to_json(code_graph: &PetCodeGraph) -> Result<String, String> {
        Self::save_to_json(code_graph)
    }

    fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String> {
        Self::load_from_json(json_str)
    }

    fn save_to_binary(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::save_to_binary(code_graph, file_path)
    }

    fn load_from_binary(file_path: &std::path::Path) -> Result<PetCodeGraph, String> {
        Self::load_from_binary(file_path)
    }

    fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::export_to_graphml(code_graph, file_path)
    }

    fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &std::path::Path) -> Result<(), String> {
        Self::export_to_gexf(code_graph, file_path)
    }
} 