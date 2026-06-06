use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;
use tracing::info;

use crate::codegraph::graph::CodeGraph;
use crate::codegraph::types::{FunctionInfo, CodeGraphStats};
use crate::codegraph::parser::CodeParser;

/// 代码图分析器，提供高级分析功能
pub struct CodeAnalyzer {
    parser: CodeParser,
    code_graph: Option<CodeGraph>,
}

impl CodeAnalyzer {
    pub fn new() -> Self {
        Self {
            parser: CodeParser::new(),
            code_graph: None,
        }
    }

    /// 分析目录并构建代码图
    pub fn analyze_directory(&mut self, dir: &Path) -> Result<&CodeGraph, String> {
        info!("Starting code graph analysis for directory: {}", dir.display());
        
        let code_graph = self.parser.build_code_graph(dir)?;
        self.code_graph = Some(code_graph);
        
        info!("Code graph analysis completed");
        Ok(self.code_graph.as_ref().unwrap())
    }

    /// 获取代码图
    pub fn get_code_graph(&self) -> Option<&CodeGraph> {
        self.code_graph.as_ref()
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> Option<&CodeGraphStats> {
        self.code_graph.as_ref().map(|cg| cg.get_stats())
    }

    /// 查找函数的所有调用者
    pub fn find_callers(&self, function_name: &str) -> Vec<&FunctionInfo> {
        if let Some(code_graph) = &self.code_graph {
            let functions = code_graph.find_functions_by_name(function_name);
            let mut all_callers = Vec::new();
            for function in functions {
                let callers = code_graph.get_callers(&function.id);
                for rel in callers {
                    if let Some(caller) = code_graph.functions.get(&rel.caller_id) {
                        all_callers.push(caller);
                    }
                }
            }
            all_callers
        } else {
            Vec::new()
        }
    }

    /// 查找函数调用的所有函数
    pub fn find_callees(&self, function_name: &str) -> Vec<&FunctionInfo> {
        if let Some(code_graph) = &self.code_graph {
            let functions = code_graph.find_functions_by_name(function_name);
            let mut all_callees = Vec::new();
            for function in functions {
                let callees = code_graph.get_callees(&function.id);
                for rel in callees {
                    if let Some(callee) = code_graph.functions.get(&rel.callee_id) {
                        all_callees.push(callee);
                    }
                }
            }
            all_callees
        } else {
            Vec::new()
        }
    }

    /// 查找调用链
    pub fn find_call_chains(&self, function_name: &str, max_depth: usize) -> Vec<Vec<&FunctionInfo>> {
        if let Some(code_graph) = &self.code_graph {
            if let Some(function) = code_graph.find_functions_by_name(function_name).first() {
                let chains = code_graph.get_call_chain(&function.id, max_depth);
                chains.into_iter().map(|chain| {
                    chain.iter().filter_map(|id| code_graph.functions.get(id)).collect()
                }).collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }

    /// 查找循环依赖
    pub fn find_circular_dependencies(&self) -> Vec<Vec<&FunctionInfo>> {
        if let Some(code_graph) = &self.code_graph {
            let mut cycles = Vec::new();
            let mut visited = HashSet::new();
            let mut rec_stack = HashSet::new();

            for function in code_graph.functions.values() {
                if !visited.contains(&function.id) {
                    let mut cycle = Vec::new();
                    self._dfs_cycle_detection(
                        function,
                        code_graph,
                        &mut visited,
                        &mut rec_stack,
                        &mut cycle,
                        &mut cycles,
                    );
                }
            }

            cycles
        } else {
            Vec::new()
        }
    }

    fn _dfs_cycle_detection<'a>(
        &self,
        function: &'a FunctionInfo,
        code_graph: &'a CodeGraph,
        visited: &mut HashSet<Uuid>,
        rec_stack: &mut HashSet<Uuid>,
        cycle: &mut Vec<&'a FunctionInfo>,
        cycles: &mut Vec<Vec<&'a FunctionInfo>>,
    ) {
        visited.insert(function.id);
        rec_stack.insert(function.id);
        cycle.push(function);

        let callees = code_graph.get_callees(&function.id);
        for callee_rel in callees {
            if let Some(callee) = code_graph.functions.get(&callee_rel.callee_id) {
                if !visited.contains(&callee.id) {
                    self._dfs_cycle_detection(callee, code_graph, visited, rec_stack, cycle, cycles);
                } else if rec_stack.contains(&callee.id) {
                    // 找到循环
                    if let Some(start_idx) = cycle.iter().position(|f| f.id == callee.id) {
                        let mut new_cycle = Vec::new();
                        for i in start_idx..cycle.len() {
                            new_cycle.push(cycle[i]);
                        }
                        new_cycle.push(callee);
                        cycles.push(new_cycle);
                    }
                }
            }
        }

        rec_stack.remove(&function.id);
        cycle.pop();
    }

    /// 查找最复杂的函数（调用关系最多）
    pub fn find_most_complex_functions(&self, limit: usize) -> Vec<(&FunctionInfo, usize)> {
        if let Some(code_graph) = &self.code_graph {
            let mut complexity_scores: Vec<(&FunctionInfo, usize)> = code_graph.functions.values()
                .map(|f| {
                    let in_degree = code_graph.get_callers(&f.id).len();
                    let out_degree = code_graph.get_callees(&f.id).len();
                    (f, in_degree + out_degree)
                })
                .collect();

            complexity_scores.sort_by(|a, b| b.1.cmp(&a.1));
            complexity_scores.truncate(limit);
            complexity_scores
        } else {
            Vec::new()
        }
    }

    /// 查找叶子函数（没有被调用的函数）
    pub fn find_leaf_functions(&self) -> Vec<&FunctionInfo> {
        if let Some(code_graph) = &self.code_graph {
            code_graph.functions.values()
                .filter(|f| code_graph.get_callers(&f.id).is_empty())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// 查找根函数（不调用其他函数的函数）
    pub fn find_root_functions(&self) -> Vec<&FunctionInfo> {
        if let Some(code_graph) = &self.code_graph {
            code_graph.functions.values()
                .filter(|f| code_graph.get_callees(&f.id).is_empty())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// 按语言统计函数分布
    pub fn get_language_distribution(&self) -> HashMap<String, usize> {
        if let Some(code_graph) = &self.code_graph {
            let mut distribution = HashMap::new();
            for function in code_graph.functions.values() {
                *distribution.entry(function.language.clone()).or_default() += 1;
            }
            distribution
        } else {
            HashMap::new()
        }
    }

    /// 按文件统计函数分布
    pub fn get_file_distribution(&self) -> HashMap<String, usize> {
        if let Some(code_graph) = &self.code_graph {
            let mut distribution = HashMap::new();
            for function in code_graph.functions.values() {
                let file_name = function.file_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                *distribution.entry(file_name.to_string()).or_default() += 1;
            }
            distribution
        } else {
            HashMap::new()
        }
    }

    /// 生成调用关系报告
    pub fn generate_call_report(&self) -> String {
        if let Some(code_graph) = &self.code_graph {
            let mut report = String::new();
            report.push_str("=== Code Graph Call Report ===\n\n");
            
            // 统计信息
            let stats = code_graph.get_stats();
            report.push_str(&format!("Total Functions: {}\n", stats.total_functions));
            report.push_str(&format!("Total Files: {}\n", stats.total_files));
            report.push_str(&format!("Resolved Calls: {}\n", stats.resolved_calls));
            report.push_str(&format!("Unresolved Calls: {}\n", stats.unresolved_calls));
            report.push_str(&format!("Languages: {}\n", stats.total_languages));
            
            // 语言分布
            report.push_str("\nLanguage Distribution:\n");
            for (lang, count) in &stats.languages {
                report.push_str(&format!("  {}: {}\n", lang, count));
            }
            
            // 文件分布
            report.push_str("\nFile Distribution:\n");
            let file_dist = self.get_file_distribution();
            for (file, count) in file_dist {
                report.push_str(&format!("  {}: {} functions\n", file, count));
            }
            
            // 最复杂的函数
            report.push_str("\nMost Complex Functions:\n");
            let complex_functions = self.find_most_complex_functions(10);
            for (func, score) in complex_functions {
                report.push_str(&format!("  {} ({}): {} calls\n", 
                    func.name, func.file_path.display(), score));
            }
            
            // 循环依赖
            report.push_str("\nCircular Dependencies:\n");
            let cycles = self.find_circular_dependencies();
            if cycles.is_empty() {
                report.push_str("  No circular dependencies found\n");
            } else {
                for (i, cycle) in cycles.iter().enumerate() {
                    report.push_str(&format!("  Cycle {}: ", i + 1));
                    let names: Vec<String> = cycle.iter().map(|f| f.name.clone()).collect();
                    report.push_str(&names.join(" -> "));
                    report.push_str("\n");
                }
            }
            
            report
        } else {
            "No code graph available".to_string()
        }
    }



    /// 导出为DOT格式
    pub fn export_dot(&self) -> Option<String> {
        self.code_graph.as_ref().map(|cg| cg.to_dot())
    }

    /// 导出为JSON格式
    pub fn export_json(&self) -> Option<Result<String, serde_json::Error>> {
        self.code_graph.as_ref().map(|cg| cg.to_json())
    }
}

impl Default for CodeAnalyzer {
    fn default() -> Self {
        Self::new()
    }
} 