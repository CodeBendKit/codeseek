use std::fs;
use std::path::PathBuf;
use uuid::Uuid;
use tracing::info;

use crate::codegraph::types::{SnippetIndex, EntityGraph, PetCodeGraph};

/// 代码片段查询服务
pub struct SnippetService {
    snippet_index: SnippetIndex,
}

impl SnippetService {
    pub fn new(snippet_index: SnippetIndex) -> Self {
        Self { snippet_index }
    }

    /// 获取函数的代码片段
    pub fn get_function_snippet(
        &mut self,
        function_id: &Uuid,
        call_graph: &PetCodeGraph,
    ) -> Result<String, String> {
        // 获取函数信息
        let function = call_graph.get_function_by_id(function_id)
            .ok_or_else(|| format!("Function with ID {} not found", function_id))?;

        // 获取代码片段
        self._get_snippet_content(&function.file_path, function.line_start, function.line_end)
    }

    /// 获取类的代码片段
    pub fn get_class_snippet(
        &mut self,
        class_id: &Uuid,
        entity_graph: &EntityGraph,
    ) -> Result<String, String> {
        // 获取类信息
        let entity = entity_graph.get_entity_by_id(class_id)
            .ok_or_else(|| format!("Class with ID {} not found", class_id))?;

        let class = match entity {
            crate::codegraph::types::EntityNode::Class(class) => class,
            _ => return Err("Entity is not a class".to_string()),
        };

        // 获取代码片段
        self._get_snippet_content(&class.file_path, class.line_start, class.line_end)
    }

    /// 获取代码片段内容
    fn _get_snippet_content(
        &mut self,
        file_path: &PathBuf,
        line_start: usize,
        line_end: usize,
    ) -> Result<String, String> {
        // 检查是否有缓存的片段
        if let Some(cached_content) = self.snippet_index.get_cached_snippet(file_path, line_start, line_end) {
            return Ok(cached_content.clone());
        }

        // 从文件读取代码片段
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;

        let lines: Vec<&str> = content.lines().collect();
        
        // 确保行号在有效范围内
        let start = (line_start - 1).min(lines.len().saturating_sub(1));
        let end = line_end.min(lines.len());
        
        if start >= end {
            return Err("Invalid line range".to_string());
        }

        // 提取指定行范围的代码
        let snippet_lines: Vec<&str> = lines[start..end].to_vec();
        let snippet_content = snippet_lines.join("\n");

        // 缓存代码片段
        self.snippet_index.cache_snippet(
            &file_path.clone(),
            line_start,
            line_end,
            snippet_content.clone(),
        );

        Ok(snippet_content)
    }

    /// 获取函数的调用者代码片段
    pub fn get_function_callers_snippets(
        &mut self,
        function_id: &Uuid,
        call_graph: &PetCodeGraph,
    ) -> Result<Vec<(String, String)>, String> {
        let callers = call_graph.get_callers(function_id);
        let mut snippets = Vec::new();

        for (caller_function, _relation) in callers {
            let snippet = self._get_snippet_content(
                &caller_function.file_path,
                caller_function.line_start,
                caller_function.line_end,
            )?;

            snippets.push((caller_function.name.clone(), snippet));
        }

        Ok(snippets)
    }

    /// 获取函数调用的函数代码片段
    pub fn get_function_callees_snippets(
        &mut self,
        function_id: &Uuid,
        call_graph: &PetCodeGraph,
    ) -> Result<Vec<(String, String)>, String> {
        let callees = call_graph.get_callees(function_id);
        let mut snippets = Vec::new();

        for (callee_function, _relation) in callees {
            let snippet = self._get_snippet_content(
                &callee_function.file_path,
                callee_function.line_start,
                callee_function.line_end,
            )?;

            snippets.push((callee_function.name.clone(), snippet));
        }

        Ok(snippets)
    }

    /// 搜索代码片段
    pub fn search_snippets(
        &mut self,
        query: &str,
        call_graph: &PetCodeGraph,
        entity_graph: &EntityGraph,
    ) -> Result<Vec<(String, String, String)>, String> {
        let mut results = Vec::new();

        // 搜索函数
        let functions = call_graph.find_functions_by_name(query);
        for function in functions {
            let snippet = self._get_snippet_content(
                &function.file_path,
                function.line_start,
                function.line_end,
            )?;

            results.push((
                "function".to_string(),
                function.name.clone(),
                snippet,
            ));
        }

        // 搜索类
        let classes = entity_graph.find_classes_by_name(query);
        for class in classes {
            let snippet = self._get_snippet_content(
                &class.file_path,
                class.line_start,
                class.line_end,
            )?;

            results.push((
                "class".to_string(),
                class.name.clone(),
                snippet,
            ));
        }

        Ok(results)
    }

    /// 获取代码片段统计信息
    pub fn get_snippet_stats(&self) -> (usize, usize) {
        let total_snippets = self.snippet_index.entity_snippets.len();
        let cached_snippets = self.snippet_index.snippet_cache.len();
        (total_snippets, cached_snippets)
    }

    /// 清理缓存
    pub fn clear_cache(&mut self) {
        self.snippet_index.snippet_cache.clear();
        info!("Snippet cache cleared");
    }

    /// 预热缓存（预加载常用代码片段）
    pub fn warm_cache(
        &mut self,
        call_graph: &PetCodeGraph,
        entity_graph: &EntityGraph,
    ) -> Result<(), String> {
        info!("Warming up snippet cache...");

        let mut count = 0;
        let max_cache = 1000; // 限制缓存大小

        // 预热函数片段
        for function in call_graph.get_all_functions() {
            if count >= max_cache {
                break;
            }

            if let Ok(_) = self._get_snippet_content(
                &function.file_path,
                function.line_start,
                function.line_end,
            ) {
                count += 1;
            }
        }

        // 预热类片段
        for class in entity_graph.get_all_classes() {
            if count >= max_cache {
                break;
            }

            if let Ok(_) = self._get_snippet_content(
                &class.file_path,
                class.line_start,
                class.line_end,
            ) {
                count += 1;
            }
        }

        info!("Snippet cache warmed up with {} items", count);
        Ok(())
    }
}

impl Default for SnippetService {
    fn default() -> Self {
        Self {
            snippet_index: SnippetIndex::default(),
        }
    }
} 