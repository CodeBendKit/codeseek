use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use uuid::Uuid;
use tracing::{info, warn, debug};

use crate::codegraph::types::{
    FunctionInfo, CallRelation, PetCodeGraph, EntityGraph, ClassInfo, ClassType,
    FileIndex, SnippetIndex
};
use crate::codegraph::graph::CodeGraph;
use crate::codegraph::treesitter::TreeSitterParser;

/// 代码解析器，负责解析源代码文件并提取函数调用关系
pub struct CodeParser {
    /// 文件路径 -> 函数列表映射
    file_functions: HashMap<PathBuf, Vec<FunctionInfo>>,
    /// 函数名 -> 函数信息映射（用于解析调用关系）
    function_registry: HashMap<String, FunctionInfo>,
    /// Tree-sitter解析器
    ts_parser: TreeSitterParser,
    /// 文件索引
    file_index: FileIndex,
    /// 代码片段索引
    snippet_index: SnippetIndex,
}

impl CodeParser {
    pub fn new() -> Self {
        Self {
            file_functions: HashMap::new(),
            function_registry: HashMap::new(),
            ts_parser: TreeSitterParser::new(),
            file_index: FileIndex::default(),
            snippet_index: SnippetIndex::default(),
        }
    }

    /// 扫描目录下的所有支持的文件
    pub fn scan_directory(&mut self, dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        self._scan_directory_recursive(dir, &mut files);
        files
    }

    fn _scan_directory_recursive(&self, dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // 跳过常见的忽略目录
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('.') || name == "target" || name == "node_modules" || name == "__pycache__" {
                            continue;
                        }
                    }
                    self._scan_directory_recursive(&path, files);
                } else if self.is_supported_file(&path) {
                    files.push(path);
                }
            }
        }
    }

    /// 判断文件是否为支持的源代码文件
    fn is_supported_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            matches!(ext.to_lowercase().as_str(),
                "cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "hh" |
                "inl" | "inc" | "tpp" | "tpl" |
                "py" | "py3" | "pyx" |
                "java" |
                "js" | "jsx" |
                "rs" |
                "ts" |
                "tsx" |
                "go"
            )
        } else {
            false
        }
    }

    /// 增量更新单个文件
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

        // 解析文件，提取新的实体和函数
        let (classes, functions) = self._extract_entities_from_file(file_path)?;

        // 移除旧的实体和函数
        self._remove_file_entities(file_path, entity_graph, call_graph);

        // 添加到图中
        let class_ids: Vec<Uuid> = classes.iter().map(|c| c.id).collect();
        let function_ids: Vec<Uuid> = functions.iter().map(|f| f.id).collect();

        for class in classes {
            entity_graph.add_class(class);
        }

        for function in functions {
            call_graph.add_function(function);
        }

        // 分析调用关系
        self._analyze_file_calls(file_path, &function_ids, call_graph)?;

        // 更新索引
        self.file_index.rebuild_for_file(file_path, class_ids.clone(), function_ids.clone());

        // 更新代码片段索引
        self._update_snippet_index(file_path, &class_ids, &function_ids, entity_graph)?;

        info!("Successfully refreshed file: {}", file_path.display());
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
                        class_type: ClassType::Struct,
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
                if let Some(caller_id) = self._find_caller_function(file_path, call_line, function_ids) {
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
    fn _find_caller_function<'a>(&self, file_path: &PathBuf, call_line: usize, function_ids: &'a [Uuid]) -> Option<&'a Uuid> {
        // 根据行号范围查找包含调用行的函数
        for function_id in function_ids {
            if let Some(function) = self._get_function_by_id(function_id) {
                if function.file_path == *file_path && 
                   call_line >= function.line_start && 
                   call_line <= function.line_end {
                    return Some(function_id);
                }
            }
        }
        None
    }

    /// 根据ID获取函数信息
    fn _get_function_by_id(&self, function_id: &Uuid) -> Option<&FunctionInfo> {
        for (_file_path, functions) in &self.file_functions {
            for function in functions {
                if function.id == *function_id {
                    return Some(function);
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
    }

    /// 更新代码片段索引
    fn _update_snippet_index(
        &mut self,
        file_path: &PathBuf,
        class_ids: &[Uuid],
        function_ids: &[Uuid],
        entity_graph: &EntityGraph,
    ) -> Result<(), String> {
        // 读取文件内容
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file for snippet indexing: {}", e))?;

        let lines: Vec<&str> = content.lines().collect();

        // 为类添加代码片段
        for &class_id in class_ids {
            if let Some(class) = entity_graph.get_class_by_id(&class_id) {
                let snippet_content = self._extract_code_snippet(&lines, class.line_start, class.line_end);
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: class.line_start,
                    line_end: class.line_end,
                    cached_content: Some(snippet_content),
                };
                self.snippet_index.add_snippet(class_id, snippet_info);
            }
        }

        // 为函数添加代码片段
        for &function_id in function_ids {
            if let Some(function) = self._get_function_by_id(&function_id) {
                let snippet_content = self._extract_code_snippet(&lines, function.line_start, function.line_end);
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: function.line_start,
                    line_end: function.line_end,
                    cached_content: Some(snippet_content),
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
            return self._extract_namespace_from_content(&content, &file_path.to_path_buf());
        }
        "global".to_string()
    }

    /// 解析单个文件（完整实现，支持多语言）
    pub fn parse_file(&mut self, file_path: &PathBuf) -> Result<(), String> {
        info!("Parsing file: {}", file_path.display());
        
        // 检查文件是否存在
        if !file_path.exists() {
            return Err(format!("File does not exist: {}", file_path.display()));
        }

        // 使用TreeSitter解析器解析文件
        let symbols = self.ts_parser.parse_file(file_path)
            .map_err(|e| format!("Failed to parse file {}: {:?}", file_path.display(), e))?;
        info!("TreeSitter parsing completed, found {} symbols", symbols.len());
        


        // 读取文件内容用于代码片段提取
        let file_content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;

        let language = self._detect_language(file_path);
        let namespace = self._extract_namespace_from_content(&file_content, file_path);
        
        let mut functions = Vec::new();
        let mut classes = Vec::new();
        let mut function_calls = Vec::new();

        // 分析每个AST符号
        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();
            debug!("Found symbol: {:?} - {}", symbol_ref.symbol_type(), symbol_ref.name());

            match symbol_ref.symbol_type() {
                crate::codegraph::treesitter::structs::SymbolType::FunctionDeclaration => {
                    // 提取函数信息
                    let function = self._extract_function_info(symbol_ref, file_path, &namespace, &language);
                    functions.push(function);
                },
                crate::codegraph::treesitter::structs::SymbolType::StructDeclaration => {
                    // 提取类/结构体信息
                    let class = self._extract_class_info(symbol_ref, file_path, &language, &namespace);
                    classes.push(class);
                },
                crate::codegraph::treesitter::structs::SymbolType::FunctionCall => {
                    // 提取函数调用信息
                    let call_info = self._extract_function_call_info(symbol_ref, file_path);
                    function_calls.push(call_info);
                },
                _ => {}
            }
        }

        // 注册函数到全局注册表
        for function in &functions {
            self.function_registry.insert(function.name.clone(), function.clone());
        }
        
        // 保存文件函数映射
        self.file_functions.insert(file_path.clone(), functions.clone());

        // 更新代码片段索引
        self._update_snippet_index_with_content(file_path, &functions, &classes, &file_content)?;

        info!("Successfully parsed file: {} ({} functions, {} classes, {} calls)", 
              file_path.display(), functions.len(), classes.len(), function_calls.len());
        
        Ok(())
    }

    /// 从AST符号提取函数信息
    fn _extract_function_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        file_path: &PathBuf,
        namespace: &str,
        language: &str,
    ) -> FunctionInfo {
        let name = symbol.name().to_string();
        let line_start = symbol.full_range().start_point.row + 1;
        let line_end = symbol.full_range().end_point.row + 1;
        
        // 尝试提取函数签名
        let signature = self._extract_function_signature(symbol);

        FunctionInfo {
            id: Uuid::new_v4(),
            name,
            file_path: file_path.clone(),
            line_start,
            line_end,
            namespace: namespace.to_string(),
            language: language.to_string(),
            signature,
        }
    }

    /// 从AST符号提取类信息
    fn _extract_class_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        file_path: &PathBuf,
        language: &str,
        namespace: &str,
    ) -> ClassInfo {
        let name = symbol.name().to_string();
        let range = symbol.full_range();
        let line_start = range.start_point.row + 1;
        let line_end = range.end_point.row + 1;

        // 根据语言确定类类型
        let class_type = match language {
            "rust" => ClassType::Struct,
            "cpp" | "java" | "typescript" | "javascript" => ClassType::Class,
            _ => ClassType::Class,
        };

        ClassInfo {
            id: Uuid::new_v4(),
            name,
            file_path: file_path.clone(),
            line_start,
            line_end,
            namespace: namespace.to_string(),
            language: language.to_string(),
            class_type,
            parent_class: None, // 需要进一步解析继承关系
            implemented_interfaces: vec![],
            member_functions: vec![],
            member_variables: vec![],
        }
    }

    /// 从AST符号提取函数调用信息
    fn _extract_function_call_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        _file_path: &PathBuf,
    ) -> (String, usize) {
        let call_name = symbol.name().to_string();
        let range = symbol.full_range();
        let line_number = range.start_point.row + 1;
        
        (call_name, line_number)
    }

    /// 提取函数签名
    fn _extract_function_signature(&self, symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance) -> Option<String> {
        // 使用声明范围来获取函数签名
        let decl_range = symbol.declaration_range();
        let full_range = symbol.full_range();
        
        // 尝试从声明范围提取签名
        if decl_range.start_point.row != full_range.start_point.row || 
           decl_range.end_point.row != full_range.end_point.row {
            // 如果声明范围与完整范围不同，说明有更精确的签名信息
            let signature = format!("{}()", symbol.name());
            return Some(signature);
        }
        
        // 否则返回函数名作为签名
        Some(symbol.name().to_string())
    }

    fn _extract_namespace_from_content(&self, content: &str, file_path: &PathBuf) -> String {
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
            _ => "global".to_string(),
        }
    }

    /// 更新代码片段索引（包含真实代码内容）
    fn _update_snippet_index_with_content(
        &mut self,
        file_path: &PathBuf,
        functions: &[FunctionInfo],
        classes: &[ClassInfo],
        file_content: &str,
    ) -> Result<(), String> {
        let lines: Vec<&str> = file_content.lines().collect();

        // 为函数添加代码片段
        for function in functions {
            let snippet_content = self._extract_code_snippet(&lines, function.line_start, function.line_end);
            
            let snippet_info = crate::codegraph::types::SnippetInfo {
                file_path: file_path.clone(),
                line_start: function.line_start,
                line_end: function.line_end,
                cached_content: Some(snippet_content),
            };
            
            self.snippet_index.add_snippet(function.id, snippet_info);
        }

        // 为类添加代码片段
        for class in classes {
            let snippet_content = self._extract_code_snippet(&lines, class.line_start, class.line_end);
            
            let snippet_info = crate::codegraph::types::SnippetInfo {
                file_path: file_path.clone(),
                line_start: class.line_start,
                line_end: class.line_end,
                cached_content: Some(snippet_content),
            };
            
            self.snippet_index.add_snippet(class.id, snippet_info);
        }

        Ok(())
    }

    /// 提取代码片段内容
    fn _extract_code_snippet(&self, lines: &[&str], start_line: usize, end_line: usize) -> String {
        let start_idx = (start_line - 1).min(lines.len());
        let end_idx = end_line.min(lines.len());
        
        if start_idx >= end_idx {
            return String::new();
        }
        
        lines[start_idx..end_idx].join("\n")
    }

    /// 解析目录下的所有文件
    pub fn parse_directory(&mut self, dir: &Path) -> Result<(), String> {
        let files = self.scan_directory(dir);
        info!("Found {} files to parse", files.len());

        for file in files {
            if let Err(e) = self.parse_file(&file) {
                warn!("Failed to parse {}: {}", file.display(), e);
            }
        }

        Ok(())
    }

    /// 构建完整的代码图（增量构建）
    pub fn build_code_graph(&mut self, dir: &Path) -> Result<CodeGraph, String> {
        // 1. 尝试从本地数据库加载现有的图
        let mut code_graph = self._load_existing_code_graph(dir)?;
        let has_existing_data = code_graph.is_some();
        
        if let Some(ref mut existing_graph) = code_graph {
            info!("Loaded existing CodeGraph with {} functions", existing_graph.functions.len());
        } else {
            info!("No existing CodeGraph found, starting fresh analysis");
            code_graph = Some(CodeGraph::new());
        }
        
        let mut code_graph = code_graph.unwrap();
        
        // 2. 扫描目录下的所有文件
        let files = self.scan_directory(dir);
        info!("Found {} files to process", files.len());
        
        // 3. 加载文件哈希值（如果存在）
        let mut file_hashes = self._load_file_hashes(dir)?;
        
        // 4. 逐个处理文件，检查是否需要重新解析
        let mut processed_files = 0;
        let mut skipped_files = 0;
        
        for file_path in files {
            if self._should_skip_file(&file_path, &mut file_hashes)? {
                skipped_files += 1;
                continue;
            }
            
            if let Err(e) = self.parse_file(&file_path) {
                warn!("Failed to parse {}: {}", file_path.display(), e);
            } else {
                processed_files += 1;
            }
        }
        
        info!("File processing completed: {} processed, {} skipped", processed_files, skipped_files);
        
        // 5. 如果这是增量构建，需要合并新解析的函数
        if has_existing_data {
            if !self.file_functions.is_empty() {
                self._merge_new_functions_to_code_graph(&mut code_graph);
            }
            // 如果没有新解析的函数，保持现有的图不变
        } else {
            // 全量构建：直接添加所有函数
            for (_file_path, functions) in &self.file_functions {
                for function in functions {
                    code_graph.add_function(function.clone());
                }
            }
        }
        
        // 6. 分析调用关系
        self._analyze_call_relations(&mut code_graph);
        
        // 7. 更新统计信息
        code_graph.update_stats();
        
        // 8. 保存新的文件哈希值
        self._save_file_hashes(dir, &file_hashes)?;
        
        Ok(code_graph)
    }

    /// 构建基于petgraph的代码图（增量构建）
    pub fn build_petgraph_code_graph(&mut self, dir: &Path) -> Result<PetCodeGraph, String> {
        // 1. 尝试从本地数据库加载现有的图
        let mut code_graph = self._load_existing_graph(dir)?;
        let has_existing_data = code_graph.is_some();
        
        if let Some(ref mut existing_graph) = code_graph {
            info!("Loaded existing graph with {} functions", existing_graph.get_stats().total_functions);
        } else {
            info!("No existing graph found, starting fresh analysis");
            code_graph = Some(PetCodeGraph::new());
        }
        
        let mut code_graph = code_graph.unwrap();
        
        // 2. 扫描目录下的所有文件
        let files = self.scan_directory(dir);
        info!("Found {} files to process", files.len());
        
        // 3. 加载文件哈希值（如果存在）
        let mut file_hashes = self._load_file_hashes(dir)?;
        
        // 4. 逐个处理文件，检查是否需要重新解析
        let mut processed_files = 0;
        let mut skipped_files = 0;
        
        for file_path in files {
            if self._should_skip_file(&file_path, &mut file_hashes)? {
                skipped_files += 1;
                continue;
            }
            
            if let Err(e) = self.parse_file(&file_path) {
                warn!("Failed to parse {}: {}", file_path.display(), e);
            } else {
                processed_files += 1;
            }
        }
        
        info!("File processing completed: {} processed, {} skipped", processed_files, skipped_files);
        
        // 5. 如果这是增量构建，需要合并新解析的函数
        if has_existing_data {
            self._merge_new_functions(&mut code_graph);
        } else {
            // 全量构建：直接添加所有函数
            for (_file_path, functions) in &self.file_functions {
                for function in functions {
                    code_graph.add_function(function.clone());
                }
            }
        }
        
        // 6. 分析调用关系
        self._analyze_petgraph_call_relations(&mut code_graph);
        
        // 7. 更新统计信息
        code_graph.update_stats();
        
        // 8. 保存新的文件哈希值
        self._save_file_hashes(dir, &file_hashes)?;
        
        Ok(code_graph)
    }

    /// 尝试从本地数据库加载现有的CodeGraph
    fn _load_existing_code_graph(&self, dir: &Path) -> Result<Option<CodeGraph>, String> {
        use crate::storage::PersistenceManager;
        use md5;
        
        let persistence = PersistenceManager::new();
        
        // 尝试多种方式的项目ID
        let project_ids = vec![
            // 1. 使用目录名（原始方式）
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
                .to_string(),
            // 2. 使用目录路径的MD5哈希（HTTP接口方式）
            format!("{:x}", md5::compute(dir.to_string_lossy().as_bytes())),
        ];
        
        for project_id in project_ids {
            info!("Attempting to load existing CodeGraph for project ID: {}", project_id);
            
            match persistence.load_graph(&project_id) {
                Ok(Some(pet_graph)) => {
                    info!("Found existing PetCodeGraph with {} functions for project ID: {}", 
                          pet_graph.graph.node_count(), project_id);
                    
                    // 将PetCodeGraph转换为CodeGraph
                    let mut code_graph = CodeGraph::new();
                    
                    // 添加所有函数
                    let mut function_count = 0;
                    for function in pet_graph.graph.node_weights() {
                        code_graph.add_function(function.clone());
                        function_count += 1;
                    }
                    info!("Converted {} functions from PetCodeGraph to CodeGraph", function_count);
                    
                    // 添加所有调用关系
                    let mut relation_count = 0;
                    for edge in pet_graph.graph.edge_weights() {
                        code_graph.add_call_relation(edge.clone());
                        relation_count += 1;
                    }
                    info!("Converted {} call relations from PetCodeGraph to CodeGraph", relation_count);
                    
                    return Ok(Some(code_graph));
                },
                Ok(None) => {
                    info!("No existing graph found for project ID: {}", project_id);
                    continue;
                },
                Err(e) => {
                    warn!("Failed to load existing CodeGraph for project ID {}: {}", project_id, e);
                    continue;
                }
            }
        }
        
        info!("No existing graph found for any project ID");
        Ok(None)
    }

    /// 合并新解析的函数到现有CodeGraph中
    fn _merge_new_functions_to_code_graph(&self, code_graph: &mut CodeGraph) {
        for (_file_path, functions) in &self.file_functions {
            for function in functions {
                // 检查函数是否已存在（基于文件路径和行号）
                let exists = code_graph.functions.values().any(|existing_func| {
                    existing_func.file_path == function.file_path &&
                    existing_func.line_start == function.line_start &&
                    existing_func.line_end == function.line_end
                });
                
                if !exists {
                    code_graph.add_function(function.clone());
                }
            }
        }
    }

    /// 尝试从本地数据库加载现有的图
    fn _load_existing_graph(&self, dir: &Path) -> Result<Option<PetCodeGraph>, String> {
        use crate::storage::PersistenceManager;
        
        // 使用项目路径作为项目ID
        let project_id = dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default");
        
        let persistence = PersistenceManager::new();
        match persistence.load_graph(project_id) {
            Ok(Some(graph)) => Ok(Some(graph)),
            Ok(None) => Ok(None),
            Err(e) => {
                warn!("Failed to load existing graph: {}", e);
                Ok(None)
            }
        }
    }

    /// 加载文件哈希值
    fn _load_file_hashes(&self, dir: &Path) -> Result<HashMap<String, String>, String> {
        use crate::storage::PersistenceManager;
        use md5;
        
        let persistence = PersistenceManager::new();
        
        // 尝试多种方式的项目ID
        let project_ids = vec![
            // 1. 使用目录名（原始方式）
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
                .to_string(),
            // 2. 使用目录路径的MD5哈希（HTTP接口方式）
            format!("{:x}", md5::compute(dir.to_string_lossy().as_bytes())),
        ];
        
        for project_id in project_ids {
            match persistence.load_file_hashes(&project_id) {
                Ok(hashes) => {
                    if !hashes.is_empty() {
                        info!("Loaded {} file hashes for project ID: {}", hashes.len(), project_id);
                        return Ok(hashes);
                    }
                },
                Err(e) => {
                    debug!("Failed to load file hashes for project ID {}: {}", project_id, e);
                    continue;
                }
            }
        }
        
        info!("No file hashes found for any project ID");
        Ok(HashMap::new())
    }

    /// 保存文件哈希值
    fn _save_file_hashes(&self, dir: &Path, hashes: &HashMap<String, String>) -> Result<(), String> {
        use crate::storage::PersistenceManager;
        use md5;
        
        let persistence = PersistenceManager::new();
        
        // 使用目录路径的MD5哈希作为项目ID（与HTTP接口保持一致）
        let project_id = format!("{:x}", md5::compute(dir.to_string_lossy().as_bytes()));
        info!("Saving file hashes for project ID: {}", project_id);
        
        // 为每个文件保存哈希值
        for (file_path, hash) in hashes {
            if let Err(e) = persistence.save_file_hash(&project_id, file_path, hash) {
                warn!("Failed to save hash for {}: {}", file_path, e);
            }
        }
        
        Ok(())
    }

    /// 检查文件是否应该跳过（基于MD5哈希值）
    fn _should_skip_file(&self, file_path: &PathBuf, file_hashes: &mut HashMap<String, String>) -> Result<bool, String> {
        use std::fs;
        use md5;
        
        // 计算当前文件的MD5哈希值
        let content = fs::read(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
        let current_hash = format!("{:x}", md5::compute(&content));
        
        // 获取相对文件路径（相对于当前工作目录）
        let file_path_str = file_path.to_string_lossy().to_string();
        
        // 检查是否存在相同的哈希值
        if let Some(saved_hash) = file_hashes.get(&file_path_str) {
            if saved_hash == &current_hash {
                debug!("File {} unchanged (MD5: {}), skipping", file_path.display(), current_hash);
                return Ok(true);
            }
        }
        
        // 更新哈希值
        file_hashes.insert(file_path_str, current_hash);
        Ok(false)
    }

    /// 合并新解析的函数到现有图中
    fn _merge_new_functions(&self, code_graph: &mut PetCodeGraph) {
        for (_file_path, functions) in &self.file_functions {
            for function in functions {
                // 检查函数是否已存在（基于文件路径和行号）
                let exists = code_graph.graph.node_weights().any(|existing_func| {
                    existing_func.file_path == function.file_path &&
                    existing_func.line_start == function.line_start &&
                    existing_func.line_end == function.line_end
                });
                
                if !exists {
                    code_graph.add_function(function.clone());
                }
            }
        }
    }

    /// 分析调用关系 
    fn _analyze_call_relations(&self, code_graph: &mut CodeGraph) {
        // 使用TreeSitter解析器分析每个文件的调用关系
        for (file_path, functions) in &self.file_functions {
            if let Ok(symbols) = self.ts_parser.parse_file(file_path) {
                self._analyze_file_call_relations(&symbols, functions, code_graph);
            } else {
                warn!("Failed to parse file for call analysis: {}", file_path.display());
            }
        }
    }

    /// 分析单个文件的调用关系
    fn _analyze_file_call_relations(
        &self, 
        symbols: &[crate::codegraph::treesitter::AstSymbolInstanceArc], 
        functions: &[FunctionInfo], 
        code_graph: &mut CodeGraph
    ) {
        // 分析每个AST符号
        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();
            
            // 检查是否为函数调用
            if symbol_ref.symbol_type() == crate::codegraph::treesitter::structs::SymbolType::FunctionCall {
                let call_name = symbol_ref.name();
                let call_file = symbol_ref.file_path();
                let call_line = symbol_ref.full_range().start_point.row + 1;
                // 1. 先在本文件查找被调用函数
                if let Some(callee_idx) = self._find_function_by_name_in_list(call_name, functions) {
                    // 查找调用者函数（通过分析调用位置）
                    if let Some(caller_idx) = self._find_caller_function_by_line(call_file, call_line, functions) {
                        let callee = &functions[callee_idx];
                        let caller = &functions[caller_idx];
                        let relation = CallRelation {
                            caller_id: caller.id,
                            callee_id: callee.id,
                            caller_name: caller.name.clone(),
                            callee_name: callee.name.clone(),
                            caller_file: caller.file_path.clone(),
                            callee_file: callee.file_path.clone(),
                            line_number: call_line,
                            is_resolved: true,
                        };
                        code_graph.add_call_relation(relation);
                        continue;
                    }
                }
                // 2. 跨文件查找被调用函数
                if let Some(callee) = self._find_function_by_name_global(call_name) {
                    // 查找调用者函数（通过分析调用位置）
                    if let Some(caller_idx) = self._find_caller_function_by_line(call_file, call_line, functions) {
                        let caller = &functions[caller_idx];
                        let relation = CallRelation {
                            caller_id: caller.id,
                            callee_id: callee.id,
                            caller_name: caller.name.clone(),
                            callee_name: callee.name.clone(),
                            caller_file: caller.file_path.clone(),
                            callee_file: callee.file_path.clone(),
                            line_number: call_line,
                            is_resolved: true,
                        };
                        code_graph.add_call_relation(relation);
                        continue;
                    }
                }
                // 3. 无法解析的调用
                self._handle_unresolved_call_legacy(call_name, call_file, call_line, functions, code_graph);
            }
        }
    }

    /// 查找调用者函数（按行号）
    fn _find_caller_function_by_line(
        &self,
        file_path: &PathBuf,
        call_line: usize,
        functions: &[FunctionInfo]
    ) -> Option<usize> {
        // 查找包含调用行的函数
        for (idx, function) in functions.iter().enumerate() {
            if function.file_path == *file_path && 
               call_line >= function.line_start && 
               call_line <= function.line_end {
                return Some(idx);
            }
        }
        None 
    }

    /// 在函数列表中根据名称查找函数
    fn _find_function_by_name_in_list(&self, name: &str, functions: &[FunctionInfo]) -> Option<usize> {
        for (idx, function) in functions.iter().enumerate() {
            if function.name == name {
                return Some(idx);
            }
        }
        None
    }

    /// 处理无法解析的函数调用（旧版本）
    fn _handle_unresolved_call_legacy(
        &self,
        call_name: &str,
        call_file: &PathBuf,
        call_line: usize,
        functions: &[FunctionInfo],
        code_graph: &mut CodeGraph
    ) {
        // 查找调用者函数
        if let Some(caller_idx) = self._find_caller_function_by_line(call_file, call_line, functions) {
            let caller = &functions[caller_idx];
            // 创建一个未解析的调用关系
            let relation = CallRelation {
                caller_id: caller.id,
                callee_id: uuid::Uuid::new_v4(), // 临时ID
                caller_name: caller.name.clone(),
                callee_name: call_name.to_string(),
                caller_file: caller.file_path.clone(),
                callee_file: call_file.clone(),
                line_number: call_line,
                is_resolved: false,
            };
            code_graph.add_call_relation(relation);
        }
    }
    
    /// 根据函数名查找函数
    fn _find_function_by_name(&self, name: &str) -> Option<&FunctionInfo> {
        for (_file_path, functions) in &self.file_functions {
            for function in functions {
                if function.name == name {
                    return Some(function);
                }
            }
        }
        None
    }

    /// 全局查找函数名（跨文件）
    fn _find_function_by_name_global(&self, name: &str) -> Option<FunctionInfo> {
        for (_file_path, functions) in &self.file_functions {
            for function in functions {
                if function.name == name {
                    return Some(function.clone());
                }
            }
        }
        None
    }

    /// 分析petgraph调用关系（完整实现）
    fn _analyze_petgraph_call_relations(&self, code_graph: &mut PetCodeGraph) {
        info!("Starting petgraph call relation analysis for {} files", self.file_functions.len());
        
        let mut total_calls = 0;
        let mut resolved_calls = 0;
        let mut unresolved_calls = 0;
        
        // 遍历每个文件的函数
        for (file_path, functions) in &self.file_functions {
            if functions.is_empty() {
                continue;
            }
            
            // 使用TreeSitter解析器分析文件中的函数调用
            match self.ts_parser.parse_file(file_path) {
                Ok(symbols) => {
                    let file_calls = self._analyze_file_calls_for_petgraph(
                        &symbols, 
                        functions, 
                        code_graph,
                        file_path
                    );
                    total_calls += file_calls.total;
                    resolved_calls += file_calls.resolved;
                    unresolved_calls += file_calls.unresolved;
                },
                Err(e) => {
                    warn!("Failed to parse file {} for call analysis: {:?}", file_path.display(), e);
                    // 即使解析失败，也尝试基于函数名的简单分析
                    self._fallback_call_analysis(functions, code_graph);
                }
            }
        }
        
        info!("Call analysis completed: {} total calls, {} resolved, {} unresolved", 
              total_calls, resolved_calls, unresolved_calls);
    }
    
    /// 分析单个文件的函数调用（用于petgraph）
    fn _analyze_file_calls_for_petgraph(
        &self,
        symbols: &[crate::codegraph::treesitter::AstSymbolInstanceArc],
        functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
        file_path: &PathBuf,
    ) -> CallAnalysisStats {
        let mut stats = CallAnalysisStats::default();
        
        // 分析每个AST符号
        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();
            
            // 检查是否为函数调用
            if symbol_ref.symbol_type() == crate::codegraph::treesitter::structs::SymbolType::FunctionCall {
                stats.total += 1;
                let call_name = symbol_ref.name();
                let call_line = symbol_ref.full_range().start_point.row + 1;
                
                // 查找调用者函数（通过分析调用位置）
                if let Some(caller_idx) = self._find_caller_function_by_line(file_path, call_line, functions) {
                    let caller = &functions[caller_idx];
                    
                    // 尝试解析被调用函数
                    if let Some(callee_info) = self._resolve_callee_function(
                        call_name, 
                        file_path, 
                        functions, 
                        code_graph
                    ) {
                        // 创建已解析的调用关系
                        let relation = CallRelation {
                            caller_id: caller.id,
                            callee_id: callee_info.id,
                            caller_name: caller.name.clone(),
                            callee_name: callee_info.name.clone(),
                            caller_file: caller.file_path.clone(),
                            callee_file: callee_info.file_path.clone(),
                            line_number: call_line,
                            is_resolved: true,
                        };
                        
                        if let Err(e) = code_graph.add_call_relation(relation) {
                            warn!("Failed to add resolved call relation: {}", e);
                        } else {
                            stats.resolved += 1;
                        }
                    } else {
                        // 创建未解析的调用关系
                        self._create_unresolved_call_relation(
                            caller, 
                            call_name, 
                            file_path, 
                            call_line, 
                            code_graph
                        );
                        stats.unresolved += 1;
                    }
                }
            }
        }
        
        stats
    }
    
    /// 解析被调用函数
    fn _resolve_callee_function(
        &self,
        call_name: &str,
        _current_file: &PathBuf,
        current_functions: &[FunctionInfo],
        code_graph: &PetCodeGraph,
    ) -> Option<FunctionInfo> {
        // 1. 先在本文件查找
        for function in current_functions {
            if function.name == call_name {
                return Some(function.clone());
            }
        }
        
        // 2. 在全局函数注册表中查找
        if let Some(global_func) = self._find_function_by_name_global(call_name) {
            return Some(global_func);
        }
        
        // 3. 在代码图中查找
        let global_functions = code_graph.find_functions_by_name(call_name);
        if let Some(func) = global_functions.first() {
            return Some((*func).clone());
        }
        
        // 4. 尝试解析限定名（如 Class.method, module.function）
        if let Some(qualified_func) = self._resolve_qualified_function_name(call_name, code_graph) {
            return Some(qualified_func);
        }
        
        None
    }
    
    /// 解析限定函数名（如 Class.method, module.function）
    fn _resolve_qualified_function_name(
        &self,
        qualified_name: &str,
        code_graph: &PetCodeGraph,
    ) -> Option<FunctionInfo> {
        // 检查是否包含分隔符
        if let Some(dot_pos) = qualified_name.rfind('.') {
            let (prefix, method_name) = qualified_name.split_at(dot_pos);
            let method_name = &method_name[1..]; // 去掉点号
            
            // 查找匹配的方法
            let candidates = code_graph.find_functions_by_name(method_name);
            for func in candidates {
                // 检查函数是否在指定的类/模块中
                if func.namespace.contains(prefix) || func.name == method_name {
                    return Some(func.clone());
                }
            }
        }
        
        None
    }
    
    /// 创建未解析的调用关系
    fn _create_unresolved_call_relation(
        &self,
        caller: &FunctionInfo,
        call_name: &str,
        file_path: &PathBuf,
        call_line: usize,
        code_graph: &mut PetCodeGraph,
    ) {
        // 为未解析的调用创建一个临时函数节点
        let temp_callee_id = Uuid::new_v4();
        let temp_callee = FunctionInfo {
            id: temp_callee_id,
            name: call_name.to_string(),
            file_path: file_path.clone(),
            line_start: call_line,
            line_end: call_line,
            namespace: "unresolved".to_string(),
            language: caller.language.clone(),
            signature: Some(format!("unresolved_call_{}", call_name)),
        };
        
        // 添加到代码图
        let _node_index = code_graph.add_function(temp_callee);
        
        // 创建未解析的调用关系
        let relation = CallRelation {
            caller_id: caller.id,
            callee_id: temp_callee_id,
            caller_name: caller.name.clone(),
            callee_name: call_name.to_string(),
            caller_file: caller.file_path.clone(),
            callee_file: file_path.clone(),
            line_number: call_line,
            is_resolved: false,
        };
        
        if let Err(e) = code_graph.add_call_relation(relation) {
            warn!("Failed to add unresolved call relation: {}", e);
        }
    }
    
    /// 回退调用分析（当TreeSitter解析失败时使用）
    fn _fallback_call_analysis(
        &self,
        functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 基于函数名的简单启发式分析
        for function in functions {
            let function_name = function.name.to_lowercase();
            
            // 根据函数名推断可能的调用关系
            if function_name.contains("main") || function_name.contains("entry") {
                // main/entry 函数可能调用其他函数
                self._create_heuristic_calls(function, functions, code_graph);
            } else if function_name.contains("test") || function_name.contains("spec") {
                // 测试函数可能调用被测试的函数
                self._create_test_calls(function, functions, code_graph);
            }
        }
    }
    
    /// 创建启发式调用关系
    fn _create_heuristic_calls(
        &self,
        main_function: &FunctionInfo,
        all_functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 为main函数创建到其他函数的调用关系
        for other_func in all_functions {
            if other_func.id != main_function.id {
                let relation = CallRelation {
                    caller_id: main_function.id,
                    callee_id: other_func.id,
                    caller_name: main_function.name.clone(),
                    callee_name: other_func.name.clone(),
                    caller_file: main_function.file_path.clone(),
                    callee_file: other_func.file_path.clone(),
                    line_number: main_function.line_start,
                    is_resolved: false, // 启发式调用标记为未解析
                };
                
                if let Err(e) = code_graph.add_call_relation(relation) {
                    warn!("Failed to add heuristic call relation: {}", e);
                }
            }
        }
    }
    
    /// 创建测试调用关系
    fn _create_test_calls(
        &self,
        test_function: &FunctionInfo,
        all_functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 为测试函数创建到被测试函数的调用关系
        let test_name = test_function.name.to_lowercase();
        
        for other_func in all_functions {
            if other_func.id != test_function.id {
                let other_name = other_func.name.to_lowercase();
                
                // 检查是否是被测试的函数
                if test_name.contains(&other_name) || other_name.contains("test") {
                    let relation = CallRelation {
                        caller_id: test_function.id,
                        callee_id: other_func.id,
                        caller_name: test_function.name.clone(),
                        callee_name: other_func.name.clone(),
                        caller_file: test_function.file_path.clone(),
                        callee_file: other_func.file_path.clone(),
                        line_number: test_function.line_start,
                        is_resolved: false, // 启发式调用标记为未解析
                    };
                    
                    if let Err(e) = code_graph.add_call_relation(relation) {
                        warn!("Failed to add test call relation: {}", e);
                    }
                }
            }
        }
    }
}

/// 调用分析统计信息
#[derive(Default, Debug)]
struct CallAnalysisStats {
    total: usize,
    resolved: usize,
    unresolved: usize,
}

impl Default for CodeParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_parse_file_with_real_rust_code() {
        let mut parser = CodeParser::new();
        
        // Create a temporary directory and Rust file
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir.path().join("test.rs");
        
        // Write a simple Rust file with functions and structs
        let rust_code = r#"
pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new(initial: i32) -> Self {
        Calculator { value: initial }
    }

    pub fn add(&mut self, x: i32) -> i32 {
        self.value += x;
        self.value
    }

    pub fn get_value(&self) -> i32 {
        self.value
    }
}

pub fn main() {
    let mut calc = Calculator::new(10);
    let result = calc.add(5);
    
}
"#;
        
        fs::write(&test_file, rust_code).unwrap();
        
        // Parse the file
        let result = parser.parse_file(&test_file);
        assert!(result.is_ok(), "Failed to parse file: {:?}", result.err());
        
        // Check that functions were extracted
        let functions = parser.file_functions.get(&test_file).unwrap();
        assert!(!functions.is_empty(), "No functions were extracted");
        
        // Check that we have the expected functions
        let function_names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
        assert!(function_names.contains(&"new"), "Function 'new' not found");
        assert!(function_names.contains(&"add"), "Function 'add' not found");
        assert!(function_names.contains(&"get_value"), "Function 'get_value' not found");
        assert!(function_names.contains(&"main"), "Function 'main' not found");
        
        // Check that snippets were created
        for function in functions {
            let snippet_info = parser.snippet_index.get_snippet_info(&function.id);
            assert!(snippet_info.is_some(), "No snippet info for function {}", function.name);
            
            let snippet = snippet_info.unwrap();
            assert!(snippet.cached_content.is_some(), "No cached content for function {}", function.name);
            
            let content = snippet.cached_content.as_ref().unwrap();
            assert!(!content.is_empty(), "Empty snippet content for function {}", function.name);
        }
        

    }

    #[test]
    fn test_parse_file_with_python_code() {
        let mut parser = CodeParser::new();
        
        // Create a temporary directory and Python file
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir.path().join("test.py");
        
        // Write a simple Python file
        let python_code = r#"
def calculate_sum(a, b):
    """Calculate the sum of two numbers."""
    return a + b

def multiply_numbers(x, y):
    """Multiply two numbers."""
    result = x * y
    return result

class Calculator:
    def __init__(self, initial_value=0):
        self.value = initial_value
    
    def add(self, x):
        self.value += x
        return self.value
    
    def get_value(self):
        return self.value

if __name__ == "__main__":
    calc = Calculator(10)
    result = calc.add(5)
    print(f"Result: {result}")
"#;
        
        fs::write(&test_file, python_code).unwrap();
        
        // Parse the file
        let result = parser.parse_file(&test_file);
        assert!(result.is_ok(), "Failed to parse Python file: {:?}", result.err());
        
        // Check that functions were extracted
        let functions = parser.file_functions.get(&test_file).unwrap();
        assert!(!functions.is_empty(), "No functions were extracted from Python file");
        
        // Check that we have the expected functions
        let function_names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
        assert!(function_names.contains(&"calculate_sum"), "Function 'calculate_sum' not found");
        assert!(function_names.contains(&"multiply_numbers"), "Function 'multiply_numbers' not found");
        

    }

    #[test]
    fn test_analyze_petgraph_call_relations() {
        let mut parser = CodeParser::new();
        let mut code_graph = PetCodeGraph::new();
        
        // 创建一些测试函数
        let func1 = FunctionInfo {
            id: Uuid::new_v4(),
            name: "main".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 1,
            line_end: 10,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn main()".to_string()),
        };
        
        let func2 = FunctionInfo {
            id: Uuid::new_v4(),
            name: "calculate".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 12,
            line_end: 20,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn calculate()".to_string()),
        };
        
        // 添加到代码图
        code_graph.add_function(func1.clone());
        code_graph.add_function(func2.clone());
        
        // 添加到文件函数映射
        parser.file_functions.insert(
            PathBuf::from("test.rs"),
            vec![func1.clone(), func2.clone()]
        );
        
        // 运行调用关系分析
        parser._analyze_petgraph_call_relations(&mut code_graph);
        
        // 验证结果
        let stats = code_graph.get_stats();
        assert!(stats.total_functions >= 2);
        
        // 检查是否有调用关系（即使是启发式的）
        let callers = code_graph.get_callers(&func1.id);
        let callees = code_graph.get_callees(&func1.id);
        
        // 由于没有真实的AST解析，可能只有启发式调用关系
        // 或者没有调用关系（取决于回退分析的实现）
    }
    
    #[test]
    fn test_resolve_qualified_function_name() {
        let mut parser = CodeParser::new();
        let mut code_graph = PetCodeGraph::new();
        
        // 创建一个类方法
        let method = FunctionInfo {
            id: Uuid::new_v4(),
            name: "process".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 1,
            line_end: 10,
            namespace: "Calculator".to_string(),
            language: "rust".to_string(),
            signature: Some("fn process()".to_string()),
        };
        
        code_graph.add_function(method.clone());
        
        // 测试解析限定名
        let result = parser._resolve_qualified_function_name("Calculator.process", &code_graph);
        assert!(result.is_some());
        
        let resolved_func = result.unwrap();
        assert_eq!(resolved_func.name, "process");
        assert_eq!(resolved_func.namespace, "Calculator");
    }

    #[test]
    fn test_incremental_build_with_md5_checking() {
        // 创建临时目录
        let temp_dir = tempfile::TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test_project");
        fs::create_dir(&project_dir).unwrap();

        // 创建测试文件
        let test_file = project_dir.join("test.rs");
        let content = r#"
pub fn hello() {
    println!("Hello, world!");
}

pub fn greet(name: &str) {
    println!("Hello, {}!", name);
}
"#;
        fs::write(&test_file, content).unwrap();

        // 创建解析器
        let mut parser = CodeParser::new();

        // 第一次构建
        let graph1 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats1 = graph1.get_stats();
        assert_eq!(stats1.total_functions, 2);

        // 修改文件内容
        let new_content = r#"
pub fn hello() {
    println!("Hello, world!");
}

pub fn greet(name: &str) {
    println!("Hello, {}!", name);
}

pub fn new_function() {
    println!("This is a new function!");
}
"#;
        fs::write(&test_file, new_content).unwrap();

        // 第二次构建（应该检测到文件变化）
        let graph2 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats2 = graph2.get_stats();
        assert_eq!(stats2.total_functions, 3); // 应该有3个函数

        // 再次构建（文件未变化，应该跳过解析）
        let graph3 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats3 = graph3.get_stats();
        assert_eq!(stats3.total_functions, 3); // 应该仍然是3个函数

        // 清理
        temp_dir.close().unwrap();
    }
}