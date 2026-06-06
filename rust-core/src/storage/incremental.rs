use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use std::io;
use uuid::Uuid;
use tracing::{info, warn, debug};
use md5;
use chrono::Utc;

use crate::codegraph::types::{
    FileMetadata, FileIndex, SnippetIndex, EntityGraph, PetCodeGraph,
    FunctionInfo, ClassInfo, CallRelation
};
use crate::codegraph::treesitter::TreeSitterParser;

/// 增量更新管理器
pub struct IncrementalManager {
    /// 文件元数据存储
    file_metadata: HashMap<PathBuf, FileMetadata>,
    /// 文件索引
    file_index: FileIndex,
    /// 代码片段索引
    snippet_index: SnippetIndex,
    /// TreeSitter解析器
    ts_parser: TreeSitterParser,
}

impl IncrementalManager {
    pub fn new() -> Self {
        Self {
            file_metadata: HashMap::new(),
            file_index: FileIndex::default(),
            snippet_index: SnippetIndex::default(),
            ts_parser: TreeSitterParser::new(),
        }
    }

    /// 计算文件的MD5哈希值
    pub fn compute_file_md5(&self, file_path: &Path) -> Result<String, io::Error> {
        let content = fs::read(file_path)?;
        let hash = md5::compute(&content);
        Ok(format!("{:x}", hash))
    }

    /// 检查文件是否需要更新
    pub fn needs_update(&self, file_path: &Path) -> Result<bool, io::Error> {
        let current_md5 = self.compute_file_md5(file_path)?;
        
        if let Some(metadata) = self.file_metadata.get(file_path) {
            Ok(metadata.md5 != current_md5)
        } else {
            // 新文件，需要更新
            Ok(true)
        }
    }

    /// 获取文件元数据
    pub fn get_file_metadata(&self, file_path: &Path) -> Option<&FileMetadata> {
        self.file_metadata.get(file_path)
    }

    /// 更新单个文件
    pub fn refresh_file(
        &mut self,
        file_path: &PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        info!("Refreshing file: {}", file_path.display());

        // 检查文件是否存在
        if !file_path.exists() {
            // 文件被删除，清理相关索引
            self._remove_file_entities(file_path, entity_graph, call_graph);
            return Ok(());
        }

        // 计算当前MD5
        let current_md5 = self.compute_file_md5(file_path)
            .map_err(|e| format!("Failed to compute MD5 for {}: {}", file_path.display(), e))?;

        // 检查是否需要更新
        if let Some(metadata) = self.file_metadata.get(file_path) {
            if metadata.md5 == current_md5 {
                debug!("File {} unchanged, skipping", file_path.display());
                return Ok(());
            }
        }

        // 文件需要更新，执行增量更新
        self._update_file(file_path, &current_md5, entity_graph, call_graph)?;

        Ok(())
    }

    /// 更新文件元数据
    fn _update_file(
        &mut self,
        file_path: &PathBuf,
        current_md5: &str,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        // 1. 移除旧的实体和函数
        self._remove_file_entities(file_path, entity_graph, call_graph);

        // 2. 解析文件，提取新的实体和函数
        let (classes, functions) = self._extract_entities_from_file(file_path)?;

        // 3. 添加到图中
        let class_ids: Vec<Uuid> = classes.iter().map(|c| c.id).collect();
        let function_ids: Vec<Uuid> = functions.iter().map(|f| f.id).collect();

        for class in classes {
            entity_graph.add_class(class);
        }

        for function in functions {
            call_graph.add_function(function);
        }

        // 4. 分析调用关系
        self._analyze_file_calls(file_path, &function_ids, call_graph)?;

        // 5. 更新索引
        self.file_index.rebuild_for_file(file_path, class_ids.clone(), function_ids.clone());

        // 6. 更新代码片段索引
        self._update_snippet_index(file_path, &class_ids, &function_ids)?;

        // 7. 更新文件元数据
        let metadata = FileMetadata {
            path: file_path.clone(),
            md5: current_md5.to_string(),
            last_updated: Utc::now(),
            file_size: fs::metadata(file_path)
                .map(|m| m.len())
                .unwrap_or(0),
            language: self._detect_language(file_path),
        };
        self.file_metadata.insert(file_path.clone(), metadata);

        info!("Successfully updated file: {}", file_path.display());
        Ok(())
    }

    /// 从文件提取实体
    fn _extract_entities_from_file(&self, file_path: &PathBuf) -> Result<(Vec<ClassInfo>, Vec<FunctionInfo>), String> {
        let mut classes = Vec::new();
        let mut functions = Vec::new();

        // 使用TreeSitter解析器解析文件
        let symbols = self.ts_parser.parse_file(file_path)
            .map_err(|e| format!("Failed to parse file {}: {:?}", file_path.display(), e))?;

        let language = self._detect_language(file_path);
        let namespace = self._extract_namespace(file_path);

        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();

            match symbol_ref.symbol_type() {
                crate::codegraph::treesitter::structs::SymbolType::FunctionDeclaration => {
                    let function = FunctionInfo {
                        id: Uuid::new_v4(),
                        name: symbol_ref.name().to_string(),
                        file_path: file_path.clone(),
                        line_start: symbol_ref.full_range().start_point.row + 1,
                        line_end: symbol_ref.full_range().end_point.row + 1,
                        namespace: namespace.clone(),
                        language: language.clone(),
                        signature: Some(symbol_ref.name().to_string()),
                    };
                    functions.push(function);
                },
                crate::codegraph::treesitter::structs::SymbolType::StructDeclaration => {
                    let class = ClassInfo {
                        id: Uuid::new_v4(),
                        name: symbol_ref.name().to_string(),
                        file_path: file_path.clone(),
                        line_start: symbol_ref.full_range().start_point.row + 1,
                        line_end: symbol_ref.full_range().end_point.row + 1,
                        namespace: namespace.clone(),
                        language: language.clone(),
                        class_type: crate::codegraph::types::ClassType::Struct,
                        parent_class: None,
                        implemented_interfaces: vec![],
                        member_functions: vec![],
                        member_variables: vec![],
                    };
                    classes.push(class);
                },
                _ => {}
            }
        }

        Ok((classes, functions))
    }

    /// 分析文件的函数调用
    fn _analyze_file_calls(
        &self,
        file_path: &PathBuf,
        function_ids: &[Uuid],
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        let symbols = self.ts_parser.parse_file(file_path)
            .map_err(|e| format!("Failed to parse file for call analysis: {:?}", e))?;

        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();

            if symbol_ref.symbol_type() == crate::codegraph::treesitter::structs::SymbolType::FunctionCall {
                let call_name = symbol_ref.name();
                let call_line = symbol_ref.full_range().start_point.row + 1;

                // 查找调用者函数
                if let Some(caller_id) = self._find_caller_function(file_path, call_line, function_ids, call_graph) {
                    // 查找被调用函数（先在本文件，再全局）
                    if let Some(callee_id) = self._find_callee_function(call_name, function_ids, call_graph) {
                        let relation = CallRelation {
                            caller_id: *caller_id,
                            callee_id,
                            caller_name: "".to_string(), // 会在add_call_relation中填充
                            callee_name: call_name.to_string(),
                            caller_file: file_path.clone(),
                            callee_file: file_path.clone(),
                            line_number: call_line,
                            is_resolved: true,
                        };
                        if let Err(e) = call_graph.add_call_relation(relation) {
                            warn!("Failed to add call relation: {}", e);
                        }
                    } else {
                        // 未解析的调用
                        self._handle_unresolved_call(caller_id, call_name, file_path, call_line, call_graph);
                    }
                }
            }
        }

        Ok(())
    }

    /// 查找调用者函数
    fn _find_caller_function<'a>(&self, file_path: &PathBuf, call_line: usize, function_ids: &'a [Uuid], call_graph: &PetCodeGraph) -> Option<&'a Uuid> {
        // 根据行号范围查找包含调用行的函数
        for function_id in function_ids {
            if let Some(function) = call_graph.get_function_by_id(function_id) {
                if function.file_path == *file_path && 
                   call_line >= function.line_start && 
                   call_line <= function.line_end {
                    return Some(function_id);
                }
            }
        }
        None
    }

    /// 查找被调用函数
    fn _find_callee_function(&self, call_name: &str, function_ids: &[Uuid], call_graph: &PetCodeGraph) -> Option<Uuid> {
        // 先在本文件查找
        for &func_id in function_ids {
            if let Some(func) = call_graph.get_function_by_id(&func_id) {
                if func.name == call_name {
                    return Some(func_id);
                }
            }
        }

        // 再全局查找
        let global_functions = call_graph.find_functions_by_name(call_name);
        global_functions.first().map(|f| f.id)
    }

    /// 处理未解析的调用
    fn _handle_unresolved_call(
        &self,
        caller_id: &Uuid,
        call_name: &str,
        file_path: &PathBuf,
        call_line: usize,
        call_graph: &mut PetCodeGraph,
    ) {
        // 创建未解析的调用关系
        let relation = CallRelation {
            caller_id: *caller_id,
            callee_id: Uuid::new_v4(), // 临时ID
            caller_name: "".to_string(),
            callee_name: call_name.to_string(),
            caller_file: file_path.clone(),
            callee_file: file_path.clone(),
            line_number: call_line,
            is_resolved: false,
        };

        if let Err(e) = call_graph.add_call_relation(relation) {
            warn!("Failed to add unresolved call relation: {}", e);
        }
    }

    /// 移除文件相关的所有实体
    fn _remove_file_entities(
        &mut self,
        file_path: &PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) {
        // 获取文件的所有实体ID
        let entity_ids = self.file_index.get_all_entity_ids(file_path);
        let function_ids = self.file_index.get_all_function_ids(file_path);
        let _class_ids = self.file_index.get_all_class_ids(file_path);

        // 从图中移除
        for entity_id in entity_ids {
            entity_graph.remove_entity(&entity_id);
        }

        for function_id in function_ids {
            if let Some(node_index) = call_graph.get_node_index(&function_id) {
                call_graph.graph.remove_node(node_index);
                call_graph.function_to_node.remove(&function_id);
                call_graph.node_to_function.remove(&node_index);
            }
        }

        // 清理索引
        self.file_index.remove_file(file_path);
        self.snippet_index.clear_file_cache(file_path);

        // 从元数据中移除
        self.file_metadata.remove(file_path);
    }

    /// 更新代码片段索引
    fn _update_snippet_index(
        &mut self,
        file_path: &PathBuf,
        class_ids: &[Uuid],
        function_ids: &[Uuid],
    ) -> Result<(), String> {
        // 读取文件内容
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file for snippet indexing: {}", e))?;

        let _lines: Vec<&str> = content.lines().collect();

        // 为类添加代码片段
        for &class_id in class_ids {
            if let Some(entity) = self.snippet_index.get_snippet_info(&class_id) {
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: entity.line_start,
                    line_end: entity.line_end,
                    cached_content: None,
                };
                self.snippet_index.add_snippet(class_id, snippet_info);
            }
        }

        // 为函数添加代码片段
        for &function_id in function_ids {
            if let Some(entity) = self.snippet_index.get_snippet_info(&function_id) {
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: entity.line_start,
                    line_end: entity.line_end,
                    cached_content: None,
                };
                self.snippet_index.add_snippet(function_id, snippet_info);
            }
        }

        Ok(())
    }

    /// 检测文件语言
    fn _detect_language(&self, file_path: &Path) -> String {
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            match ext.to_lowercase().as_str() {
                "rs" => "rust".to_string(),
                "py" | "py3" | "pyx" => "python".to_string(),
                "js" | "jsx" => "javascript".to_string(),
                "ts" | "tsx" => "typescript".to_string(),
                "java" => "java".to_string(),
                "cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "hh" => "cpp".to_string(),
                "go" => "go".to_string(),
                _ => "unknown".to_string(),
            }
        } else {
            "unknown".to_string()
        }
    }

    /// 提取命名空间
    fn _extract_namespace(&self, file_path: &Path) -> String {
        // 从文件内容解析命名空间
        if let Ok(content) = fs::read_to_string(file_path) {
            return self._extract_namespace_from_content(&content, file_path);
        }
        "global".to_string()
    }

    /// 从文件内容提取命名空间
    fn _extract_namespace_from_content(&self, content: &str, file_path: &Path) -> String {
        let language = self._detect_language(file_path);
        
        match language.as_str() {
            "rust" => {
                // 查找mod声明
                for line in content.lines() {
                    if line.trim().starts_with("mod ") {
                        if let Some(name) = line.trim().split_whitespace().nth(1) {
                            return name.to_string();
                        }
                    }
                }
                "crate".to_string()
            },
            "python" => {
                // 查找包名或模块名
                for line in content.lines() {
                    if line.trim().starts_with("__package__") {
                        if let Some(name) = line.split('=').nth(1) {
                            return name.trim().trim_matches('"').trim_matches('\'').to_string();
                        }
                    }
                }
                "global".to_string()
            },
            "java" => {
                // 查找package声明
                for line in content.lines() {
                    if line.trim().starts_with("package ") {
                        if let Some(package) = line.trim().split_whitespace().nth(1) {
                            return package.trim_end_matches(';').to_string();
                        }
                    }
                }
                "default".to_string()
            },
            "cpp" => {
                // 查找namespace声明
                for line in content.lines() {
                    if line.trim().starts_with("namespace ") {
                        if let Some(name) = line.trim().split_whitespace().nth(1) {
                            return name.to_string();
                        }
                    }
                }
                "global".to_string()
            },
            "go" => {
                // 查找package声明
                for line in content.lines() {
                    if line.trim().starts_with("package ") {
                        if let Some(package) = line.trim().split_whitespace().nth(1) {
                            return package.trim_end_matches(';').to_string();
                        }
                    }
                }
                "global".to_string()
            },
            _ => "global".to_string(),
        }
    }

    /// 获取文件索引
    pub fn get_file_index(&self) -> &FileIndex {
        &self.file_index
    }

    /// 获取代码片段索引
    pub fn get_snippet_index(&self) -> &SnippetIndex {
        &self.snippet_index
    }

    /// 获取所有文件元数据
    pub fn get_all_file_metadata(&self) -> &HashMap<PathBuf, FileMetadata> {
        &self.file_metadata
    }

    /// 保存状态到文件
    pub fn save_state(&self, path: &Path) -> Result<(), String> {
        let state = serde_json::json!({
            "file_metadata": self.file_metadata,
            "file_index": self.file_index,
            "snippet_index": self.snippet_index,
        });

        fs::write(path, serde_json::to_string_pretty(&state)
            .map_err(|e| format!("Failed to serialize state: {}", e))?)
            .map_err(|e| format!("Failed to save state: {}", e))?;

        Ok(())
    }

    /// 从文件加载状态
    pub fn load_state(&mut self, path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse state: {}", e))?;

        if let Some(metadata) = state.get("file_metadata") {
            self.file_metadata = serde_json::from_value(metadata.clone())
                .map_err(|e| format!("Failed to deserialize file metadata: {}", e))?;
        }

        if let Some(index) = state.get("file_index") {
            self.file_index = serde_json::from_value(index.clone())
                .map_err(|e| format!("Failed to deserialize file index: {}", e))?;
        }

        if let Some(snippets) = state.get("snippet_index") {
            self.snippet_index = serde_json::from_value(snippets.clone())
                .map_err(|e| format!("Failed to deserialize snippet index: {}", e))?;
        }

        Ok(())
    }
}

impl Default for IncrementalManager {
    fn default() -> Self {
        Self::new()
    }
} 

impl crate::storage::traits::IncrementalUpdater for IncrementalManager {
    fn compute_file_md5(&self, file_path: &std::path::Path) -> Result<String, std::io::Error> {
        Self::compute_file_md5(self, file_path)
    }

    fn needs_update(&self, file_path: &std::path::Path) -> Result<bool, std::io::Error> {
        Self::needs_update(self, file_path)
    }

    fn refresh_file(
        &mut self,
        file_path: &std::path::PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        Self::refresh_file(self, file_path, entity_graph, call_graph)
    }

    fn get_file_index(&self) -> &FileIndex { Self::get_file_index(self) }
    fn get_snippet_index(&self) -> &SnippetIndex { Self::get_snippet_index(self) }
    fn get_all_file_metadata(&self) -> &HashMap<std::path::PathBuf, FileMetadata> { Self::get_all_file_metadata(self) }

    fn save_state(&self, path: &std::path::Path) -> Result<(), String> { Self::save_state(self, path) }
    fn load_state(&mut self, path: &std::path::Path) -> Result<(), String> { Self::load_state(self, path) }
} 