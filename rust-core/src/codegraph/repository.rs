use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;
use tracing::{info, warn, debug};

use crate::codegraph::types::{
    EntityGraph, PetCodeGraph, SnippetIndex, FunctionInfo
};
use crate::codegraph::parser::CodeParser;
use crate::services::SnippetService;
use crate::storage::IncrementalManager;

/// 仓库管理器，整合代码分析、增量更新和查询功能
pub struct RepositoryManager {
    /// 实体图（类、结构体等）
    entity_graph: Arc<RwLock<EntityGraph>>,
    /// 调用图（函数调用关系）
    call_graph: Arc<RwLock<PetCodeGraph>>,
    /// 代码解析器
    parser: CodeParser,
    /// 增量更新管理器
    incremental_manager: IncrementalManager,
    /// 代码片段服务
    snippet_service: Arc<RwLock<SnippetService>>,
    /// 仓库根路径
    repository_path: PathBuf,
}

impl RepositoryManager {
    pub fn new(repository_path: PathBuf) -> Self {
        let entity_graph = Arc::new(RwLock::new(EntityGraph::new()));
        let call_graph = Arc::new(RwLock::new(PetCodeGraph::new()));
        let snippet_index = SnippetIndex::default();
        let snippet_service = Arc::new(RwLock::new(SnippetService::new(snippet_index)));

        Self {
            entity_graph,
            call_graph,
            parser: CodeParser::new(),
            incremental_manager: IncrementalManager::new(),
            snippet_service,
            repository_path,
        }
    }

    /// 初始化仓库分析
    pub fn initialize(&mut self) -> Result<(), String> {
        info!("Initializing repository analysis for: {}", self.repository_path.display());

        // 扫描所有文件
        let files = self.parser.scan_directory(&self.repository_path);
        info!("Found {} files to analyze", files.len());

        // 分析每个文件
        for file_path in files {
            if let Err(e) = self.refresh_file(&file_path) {
                warn!("Failed to analyze file {}: {}", file_path.display(), e);
            }
        }

        // 预热代码片段缓存
        if let Err(e) = self.warm_snippet_cache() {
            warn!("Failed to warm snippet cache: {}", e);
        }

        info!("Repository analysis completed");
        Ok(())
    }

    /// 增量更新单个文件
    pub fn refresh_file(&mut self, file_path: &PathBuf) -> Result<(), String> {
        info!("Refreshing file: {}", file_path.display());

        // 检查文件是否需要更新
        if let Ok(needs_update) = self.incremental_manager.needs_update(file_path) {
            if !needs_update {
                debug!("File {} unchanged, skipping", file_path.display());
                return Ok(());
            }
        }

        // 使用解析器刷新文件
        let mut entity_graph = self.entity_graph.write();
        let mut call_graph = self.call_graph.write();

        self.parser.refresh_file(file_path, &mut entity_graph, &mut call_graph)?;

        // 更新统计信息
        entity_graph.update_stats();
        call_graph.update_stats();

        info!("Successfully refreshed file: {}", file_path.display());
        Ok(())
    }

    /// 批量更新多个文件
    pub fn refresh_files(&mut self, file_paths: &[PathBuf]) -> Result<(), String> {
        info!("Refreshing {} files", file_paths.len());

        let mut errors = Vec::new();
        for file_path in file_paths {
            if let Err(e) = self.refresh_file(file_path) {
                errors.push(format!("{}: {}", file_path.display(), e));
            }
        }

        if !errors.is_empty() {
            Err(format!("Failed to refresh some files:\n{}", errors.join("\n")))
        } else {
            Ok(())
        }
    }

    /// 获取仓库统计信息
    pub fn get_repository_stats(&self) -> RepositoryStats {
        let entity_graph = self.entity_graph.read();
        let call_graph = self.call_graph.read();
        let snippet_service = self.snippet_service.read();

        let (total_snippets, cached_snippets) = snippet_service.get_snippet_stats();

        RepositoryStats {
            total_classes: entity_graph.stats.total_classes,
            total_functions: call_graph.stats.total_functions,
            total_files: entity_graph.stats.total_files,
            total_languages: entity_graph.stats.total_languages,
            resolved_calls: call_graph.stats.resolved_calls,
            unresolved_calls: call_graph.stats.unresolved_calls,
            total_snippets,
            cached_snippets,
        }
    }

    /// 搜索实体
    pub fn search_entities(&self, query: &str) -> Vec<SearchResult> {
        let mut results = Vec::new();

        // 搜索函数
        let call_graph = self.call_graph.read();
        let functions = call_graph.find_functions_by_name(query);
        for function in functions {
            results.push(SearchResult {
                id: function.id,
                name: function.name.clone(),
                entity_type: "function".to_string(),
                file_path: function.file_path.clone(),
                line_start: function.line_start,
                line_end: function.line_end,
                language: function.language.clone(),
            });
        }

        // 搜索类
        let entity_graph = self.entity_graph.read();
        let classes = entity_graph.find_classes_by_name(query);
        for class in classes {
            results.push(SearchResult {
                id: class.id,
                name: class.name.clone(),
                entity_type: "class".to_string(),
                file_path: class.file_path.clone(),
                line_start: class.line_start,
                line_end: class.line_end,
                language: class.language.clone(),
            });
        }

        results
    }

    /// 获取函数的调用者
    pub fn get_function_callers(&self, function_id: &uuid::Uuid) -> Vec<FunctionInfo> {
        let call_graph = self.call_graph.read();
        call_graph.get_callers(function_id)
            .into_iter()
            .map(|(func, _)| func.clone())
            .collect()
    }

    /// 获取函数调用的函数
    pub fn get_function_callees(&self, function_id: &uuid::Uuid) -> Vec<FunctionInfo> {
        let call_graph = self.call_graph.read();
        call_graph.get_callees(function_id)
            .into_iter()
            .map(|(func, _)| func.clone())
            .collect()
    }

    /// 获取代码片段
    pub fn get_snippet(&self, entity_id: &uuid::Uuid, entity_type: &str) -> Result<String, String> {
        let mut snippet_service = self.snippet_service.write();
        let entity_graph = self.entity_graph.read();
        let call_graph = self.call_graph.read();

        match entity_type {
            "function" => snippet_service.get_function_snippet(entity_id, &call_graph),
            "class" => snippet_service.get_class_snippet(entity_id, &entity_graph),
            _ => Err(format!("Unknown entity type: {}", entity_type)),
        }
    }

    /// 获取调用链
    pub fn get_call_chain(&self, function_id: &uuid::Uuid, max_depth: usize) -> Vec<Vec<uuid::Uuid>> {
        let call_graph = self.call_graph.read();
        call_graph.get_call_chain(function_id, max_depth)
    }

    /// 预热代码片段缓存
    pub fn warm_snippet_cache(&self) -> Result<(), String> {
        let mut snippet_service = self.snippet_service.write();
        let entity_graph = self.entity_graph.read();
        let call_graph = self.call_graph.read();

        snippet_service.warm_cache(&call_graph, &entity_graph)
    }

    /// 清理代码片段缓存
    pub fn clear_snippet_cache(&mut self) {
        let mut snippet_service = self.snippet_service.write();
        snippet_service.clear_cache();
    }

    /// 保存仓库状态
    pub fn save_state(&self, state_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| format!("Failed to create state directory: {}", e))?;

        // 保存实体图
        let entity_graph_path = state_dir.join("entity_graph.json");
        let entity_graph = self.entity_graph.read();
        let entity_graph_json = entity_graph.to_json()
            .map_err(|e| format!("Failed to serialize entity graph: {}", e))?;
        std::fs::write(&entity_graph_path, entity_graph_json)
            .map_err(|e| format!("Failed to write entity graph: {}", e))?;

        // 保存调用图
        let call_graph_path = state_dir.join("call_graph.json");
        let call_graph = self.call_graph.read();
        let call_graph_json = call_graph.to_json()
            .map_err(|e| format!("Failed to serialize call graph: {}", e))?;
        std::fs::write(&call_graph_path, call_graph_json)
            .map_err(|e| format!("Failed to write call graph: {}", e))?;

        // 保存增量更新状态
        let incremental_state_path = state_dir.join("incremental_state.json");
        self.incremental_manager.save_state(&incremental_state_path)?;

        info!("Repository state saved to: {}", state_dir.display());
        Ok(())
    }

    /// 加载仓库状态
    pub fn load_state(&mut self, state_dir: &Path) -> Result<(), String> {
        info!("Loading repository state from: {}", state_dir.display());

        // 加载实体图
        let entity_graph_path = state_dir.join("entity_graph.json");
        if entity_graph_path.exists() {
            let entity_graph_json = std::fs::read_to_string(&entity_graph_path)
                .map_err(|e| format!("Failed to read entity graph: {}", e))?;
            let entity_graph = EntityGraph::from_json(&entity_graph_json)
                .map_err(|e| format!("Failed to deserialize entity graph: {}", e))?;
            *self.entity_graph.write() = entity_graph;
        }

        // 加载调用图
        let call_graph_path = state_dir.join("call_graph.json");
        if call_graph_path.exists() {
            let call_graph_json = std::fs::read_to_string(&call_graph_path)
                .map_err(|e| format!("Failed to read call graph: {}", e))?;
            let call_graph = PetCodeGraph::from_json(&call_graph_json)
                .map_err(|e| format!("Failed to deserialize call graph: {}", e))?;
            *self.call_graph.write() = call_graph;
        }

        // 加载增量更新状态
        let incremental_state_path = state_dir.join("incremental_state.json");
        if incremental_state_path.exists() {
            self.incremental_manager.load_state(&incremental_state_path)?;
        }

        info!("Repository state loaded successfully");
        Ok(())
    }

    /// 获取仓库路径
    pub fn get_repository_path(&self) -> &Path {
        &self.repository_path
    }

    /// 获取实体图引用
    pub fn get_entity_graph(&self) -> Arc<RwLock<EntityGraph>> {
        self.entity_graph.clone()
    }

    /// 获取调用图引用
    pub fn get_call_graph(&self) -> Arc<RwLock<PetCodeGraph>> {
        self.call_graph.clone()
    }

    /// 获取代码片段服务引用
    pub fn get_snippet_service(&self) -> Arc<RwLock<SnippetService>> {
        self.snippet_service.clone()
    }
}

/// 仓库统计信息
#[derive(Debug, Clone)]
pub struct RepositoryStats {
    pub total_classes: usize,
    pub total_functions: usize,
    pub total_files: usize,
    pub total_languages: usize,
    pub resolved_calls: usize,
    pub unresolved_calls: usize,
    pub total_snippets: usize,
    pub cached_snippets: usize,
}

/// 搜索结果
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: uuid::Uuid,
    pub name: String,
    pub entity_type: String,
    pub file_path: PathBuf,
    pub line_start: usize,
    pub line_end: usize,
    pub language: String,
}

impl Default for RepositoryManager {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
} 