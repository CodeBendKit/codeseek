use axum::{
    routing::{post, get},
    Router,
    response::Json,
    extract::State,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_http::classify::ServerErrorsFailureClass;
use axum::extract::Request;
use axum::response::Response;
use tracing::Span;
use crate::storage::StorageManager;
use crate::storage::TantivyBm25Index;
use crate::services::hybrid_search::{HybridSearchService, HybridSearchConfig};
use crate::services::reranker_service::RerankerService;
use crate::storage::traits_bm25::TextSearchProvider;
use crate::services::embedding_service::EmbeddingService;

use super::{
    handlers::{query_call_graph, query_code_snippet, query_code_skeleton,
         query_hierarchical_graph, draw_call_graph, draw_call_graph_home,
         investigate_repo, semantic_search, query_indexing_status,
         perform_analysis, setup_watcher, trigger_embedding_build,
         commit_embed, commit_search, commit_clear,
         repo_knowledge_embed, repo_knowledge_search},
    models::ApiResponse,
};
use crate::services::embedding_service::OpenAICompatibleEmbeddingProvider;
use crate::services::commit_embedding_service::EmbeddingProviderAdapter;
use crate::services::repo_knowledge_service::{EmbeddingProviderAdapter as RepoKnowledgeEmbeddingProviderAdapter};

/// Combined state for routes that need both StorageManager and HybridSearchService
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<StorageManager>,
    pub hybrid: Option<Arc<HybridSearchService>>,
}

pub struct CodeSeekServer {
    storage: Arc<StorageManager>,
    repo_path: String,
    /// Hybrid search service for semantic search endpoint.
    /// Initialized in `start()` using Tantivy BM25 index + EmbeddingService.
    hybrid_search_service: Option<Arc<HybridSearchService>>,
}

#[derive(serde::Serialize)]
struct StatusResponse {
    repo_path: String,
    project_id: String,
    total_functions: usize,
    total_files: usize,
    embedding_enabled: bool,
    indexing_status: String,
}

impl CodeSeekServer {
    pub fn new(storage: Arc<StorageManager>, repo_path: String) -> Self {
        Self {
            storage,
            repo_path,
            hybrid_search_service: None,
        }
    }

    pub async fn start(&mut self, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
        // 提前 clone repo_path 避免后续借用冲突
        let repo_path = self.repo_path.clone();
        
        // ---- 启动时自动初始化仓库 ----
        let project_dir = std::path::Path::new(&repo_path);
        if !project_dir.exists() || !project_dir.is_dir() {
            return Err(format!("Repository path does not exist or is not a directory: {}", self.repo_path).into());
        }

        // 绑定当前进程到该仓库
        if let Err(existing) = self.storage.try_bind_repo(&self.repo_path) {
            return Err(format!("Process already bound to repo '{}'", existing).into());
        }

        let project_id = format!("{:x}", md5::compute(self.repo_path.as_bytes()));
        tracing::info!("Initializing repo: {} (project_id: {})", self.repo_path, project_id);

        // 加载已有图谱或执行分析
        match self.storage.get_persistence().load_graph(&project_id) {
            Ok(Some(graph)) => {
                let stats = graph.get_stats().clone();
                self.storage.set_graph(graph);
                tracing::info!("Loaded cached graph: {} functions, {} files", stats.total_functions, stats.total_files);
            }
            Ok(None) => {
                tracing::info!("No cached graph found, performing analysis...");
                match perform_analysis(
                    self.storage.clone(),
                    project_dir.to_path_buf(),
                    project_id.clone(),
                ).await {
                    Ok(resp) => {
                        tracing::info!("Analysis complete: {} functions, {} files", resp.total_functions, resp.total_files);
                    }
                    Err(e) => {
                        tracing::error!("Analysis failed: {:?}", e);
                        return Err("Failed to analyze repository".into());
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to load graph: {}", e);
                return Err(e.into());
            }
        }

        // ===== Create shared Tantivy BM25 index FIRST (before background tasks) =====
        // This avoids LockBusy conflicts from multiple open_or_create calls on the same dir.
        let shared_bm25_index: Arc<dyn TextSearchProvider> = {
            let config = self.storage.get_config();
            if config.as_ref().map_or(false, |c| c.codebase.enable_embedding) {
                let config = config.unwrap();
                let db_path = &config.codebase.embedding_db_uri;
                // 使用项目特定子目录实现 BM25 索引隔离（与向量索引 collection 命名对齐）
                let repo_path = std::path::Path::new(&self.repo_path);
                let last_dir = repo_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                let hash = md5::compute(&self.repo_path);
                let hash_hex = format!("{:x}", hash);
                let tantivy_dir = std::path::Path::new(db_path)
                    .join("tantivy_bm25")
                    .join(format!("{}_{}", last_dir, hash_hex));
                match TantivyBm25Index::open_or_create(&tantivy_dir) {
                    Ok(idx) => {
                        tracing::info!("Tantivy BM25 index ready at {:?}", tantivy_dir);
                        Arc::new(idx) as Arc<dyn TextSearchProvider>
                    }
                    Err(e) => {
                        tracing::warn!("Failed to create Tantivy index (search will fall back to dense-only): {}", e);
                        Arc::new(FallbackTextSearchProvider) as Arc<dyn TextSearchProvider>
                    }
                }
            } else {
                Arc::new(FallbackTextSearchProvider) as Arc<dyn TextSearchProvider>
            }
        };

        // Store shared index in StorageManager for file watcher access
        self.storage.set_bm25_index(shared_bm25_index.clone());

        // 触发嵌入索引构建 (pass shared index to avoid LockBusy)
        if let Err(e) = trigger_embedding_build(self.storage.clone(), repo_path.clone(), Some(shared_bm25_index.clone())).await {
            tracing::info!("Embedding build skipped: {}", e);
        }

       // 初始化 Commit Embedding Service
        let project_id = format!("{:x}", md5::compute(self.repo_path.as_bytes()));
        if let Err(e) = self.init_commit_embedding_service(&project_id).await {
            tracing::warn!("Failed to initialize commit embedding service: {}", e);
        }

        // 初始化混合检索服务 (Hybrid Search)
        if let Ok(config) = self.storage.get_config().ok_or("Config not set") {
            if config.codebase.enable_embedding {
                let _embedding_config = &config.codebase.embedding;
                let db_path = &config.codebase.embedding_db_uri;
                
                // 生成 collection 名称
                let path = std::path::Path::new(&self.repo_path);
                let last_dir = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                let hash = md5::compute(&self.repo_path);
                let hash_hex = format!("{:x}", hash);
                let collection = format!("{}_{}", last_dir, hash_hex);
                
                // 创建 EmbeddingService
                let embedding_service = match EmbeddingService::new(db_path, collection.clone(), Some(&config), None).await {
                    Ok(s) => Some(Arc::new(s)),
                    Err(e) => {
                        tracing::warn!("Failed to create EmbeddingService for hybrid search: {}", e);
                        None
                    }
                };
                
                // 复用已创建的共享 Tantivy BM25 Index (避免 LockBusy)
                let tantivy_index = shared_bm25_index.clone();
                
                 // 创建 HybridSearchService
                if let Some(embedding_service) = embedding_service {
                    let hybrid_cfg = &config.codebase.retrieval_pipeline.hybrid;
                    
                    // 可选：创建 RerankerService
                    let reranker = if config.codebase.retrieval_pipeline.reranker.enabled {
                        Some(RerankerService::new(config.codebase.retrieval_pipeline.reranker.clone()))
                    } else {
                        None
                    };
                    
                    let hybrid = HybridSearchService::with_reranker(
                        embedding_service,
                        tantivy_index,
                        HybridSearchConfig {
                            enable_sparse: true,
                            rrf_k: 60.0,
                            dense_limit: 100,
                            sparse_limit: 100,
                            timeout_ms: 0,
                            short_code_threshold: hybrid_cfg.short_code_threshold,
                            short_code_penalty: hybrid_cfg.short_code_penalty,
                        },
                        reranker,
                    );
                    self.hybrid_search_service = Some(Arc::new(hybrid));
                    tracing::info!("HybridSearchService initialized");
                }
            }
        }

        // 初始化 Repo Knowledge Service
        if let Err(e) = self.init_repo_knowledge_service(&project_id).await {
            tracing::warn!("Failed to initialize repo knowledge service: {}", e);
        }

        // 启动文件监听
        setup_watcher(self.storage.clone(), project_dir.to_path_buf(), project_id.clone());

        // ---- 启动 HTTP 服务器 ----
        let app = self.create_router();

        let listener = TcpListener::bind(addr).await?;
        println!("CodeSeek HTTP server starting on {}, repo: {}", addr, repo_path);

        axum::serve(listener, app).await?;
        Ok(())
    }

     /// 初始化 Commit Embedding Service
    async fn init_commit_embedding_service(
        &mut self, 
        project_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 获取配置
        let config = self.storage.get_config().ok_or("Config not set")?;
        
        // 检查 embedding 是否启用
        if !config.codebase.enable_embedding {
            return Err("Embedding not enabled in config".into());
        }

        // 获取 embedding 配置
        let mut api_token = std::env::var("SILICONFLOW_API_KEY").ok();
        let mut base_url = None;
        let mut model = "Qwen/Qwen3-Embedding-4B".to_string();

        let embedding_config = &config.codebase.embedding;
        if !embedding_config.api_token.is_empty() {
            api_token = Some(embedding_config.api_token.clone());
        }
        if !embedding_config.api_base_url.is_empty() {
            base_url = Some(embedding_config.api_base_url.clone());
        }
        if !embedding_config.model.is_empty() {
            model = embedding_config.model.clone();
        }

        let api_token = api_token.ok_or("API Key not found in config or environment variable SILICONFLOW_API_KEY")?;
        
         // 创建 embedding provider
       let provider = OpenAICompatibleEmbeddingProvider::new(api_token, base_url, model);
        let adapter = EmbeddingProviderAdapter::from_openai_provider(provider);
        
        // 初始化 commit embedding service
        self.storage.init_commit_embedding_service(Box::new(adapter), project_id).await?;
        
        tracing::info!("Commit embedding service initialized successfully for project: {}", project_id);
        Ok(())
    }

    /// 初始化 Repo Knowledge Service
    async fn init_repo_knowledge_service(&mut self, project_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        // 获取配置
        let config = self.storage.get_config().ok_or("Config not set")?;
        
        // 检查 embedding 是否启用
        if !config.codebase.enable_embedding {
            return Err("Embedding not enabled in config".into());
        }

        // 获取 embedding 配置
        let mut api_token = std::env::var("SILICONFLOW_API_KEY").ok();
        let mut base_url = None;
        let mut model = "Qwen/Qwen3-Embedding-4B".to_string();

        let embedding_config = &config.codebase.embedding;
        if !embedding_config.api_token.is_empty() {
            api_token = Some(embedding_config.api_token.clone());
        }
        if !embedding_config.api_base_url.is_empty() {
            base_url = Some(embedding_config.api_base_url.clone());
        }
        if !embedding_config.model.is_empty() {
            model = embedding_config.model.clone();
        }

        let api_token = api_token.ok_or("API Key not found in config or environment variable SILICONFLOW_API_KEY")?;
        
        // 创建 embedding provider
        let provider = OpenAICompatibleEmbeddingProvider::new(api_token, base_url, model);
        let adapter = RepoKnowledgeEmbeddingProviderAdapter::from_embedding_provider(Box::new(provider));
        
        // 初始化 repo knowledge service
        self.storage.init_repo_knowledge_service(Box::new(adapter), project_id).await?;
        
        tracing::info!("Repo knowledge service initialized successfully");
        Ok(())
    }

    fn create_router(&self) -> Router {
        let cors = CorsLayer::permissive();
        
        // 创建 HTTP 请求日志中间件
        let request_logging = TraceLayer::new_for_http()
            .make_span_with(|request: &Request| {
                let method = request.method().to_string();
                let path = request.uri().path().to_string();
                let query = request.uri().query().unwrap_or("");
                
                tracing::info_span!(
                    "http_request",
                    method = %method,
                    path = %path,
                    query = %query,
                )
            })
            .on_request(|request: &Request, _span: &Span| {
                let method = request.method().to_string();
                let path = request.uri().path().to_string();
                let client_ip = request
                    .headers()
                    .get("x-forwarded-for")
                    .map(|v| v.to_str().unwrap_or(""))
                    .unwrap_or("");
                
                tracing::info!(
                    method = method,
                    path = path,
                    client_ip = %client_ip,
                    "Request started"
                );
            })
            .on_response(|response: &Response, latency: Duration, _span: &Span| {
                let status = response.status().as_u16();
                let latency_ms = latency.as_millis();
                
                if status >= 500 {
                    tracing::error!(
                        status = status,
                        latency_ms = latency_ms,
                        "Request completed (server error)"
                    );
                } else if status >= 400 {
                    tracing::warn!(
                        status = status,
                        latency_ms = latency_ms,
                        "Request completed (client error)"
                    );
                } else {
                    tracing::info!(
                        status = status,
                        latency_ms = latency_ms,
                        "Request completed (success)"
                    );
                }
            })
            .on_failure(|failure: ServerErrorsFailureClass, latency: Duration, _span: &Span| {
                let latency_ms = latency.as_millis();
                match &failure {
                    ServerErrorsFailureClass::Error(error) => {
                        tracing::error!(
                            error = %error,
                            latency_ms = latency_ms,
                            "Server error occurred"
                        );
                    }
                    ServerErrorsFailureClass::StatusCode(status) => {
                        tracing::error!(
                            status = %status,
                            latency_ms = latency_ms,
                            "Request failed with status code"
                        );
                    }
                }
            });
        
         // Create unified AppState
        let app_state = AppState {
            storage: self.storage.clone(),
            hybrid: self.hybrid_search_service.clone(),
        };
        
            // Build all routes with unified state
        let app = Router::new()
            .route("/health", get(health_check))
            .route("/status", get(get_status))
            .route("/query_call_graph", post(query_call_graph))
            .route("/query_code_snippet", post(query_code_snippet))
            .route("/query_code_skeleton", post(query_code_skeleton))
            .route("/query_hierarchical_graph", post(query_hierarchical_graph))
            .route("/investigate_repo", post(investigate_repo))
            .route("/query_indexing_status", post(query_indexing_status))
            .route("/commit/embed", post(commit_embed))
            .route("/commit/search", post(commit_search))
            .route("/commit/clear", post(commit_clear))
            .route("/repo_knowledge/embed", post(repo_knowledge_embed))
            .route("/repo_knowledge/search", post(repo_knowledge_search))
            .route("/", get(draw_call_graph_home))
            .route("/draw_call_graph", get(draw_call_graph))
            .route("/semantic_search", post(semantic_search))
            .with_state(app_state)
            .layer(request_logging)
            .layer(cors);
        
        app
    }
}

// Health check endpoint
async fn health_check() -> Json<ApiResponse<&'static str>> {
    Json(ApiResponse {
        success: true,
        data: "CodeSeek HTTP service is running",
    })
}

// Status endpoint - returns info about the currently indexed repo
async fn get_status(
    State(storage): State<AppState>,
) -> Json<ApiResponse<StatusResponse>> {
    let repo_path = storage.storage.get_current_repo().unwrap_or_default();
    let project_id = format!("{:x}", md5::compute(&repo_path));

    let (total_functions, total_files) = storage.storage
        .get_graph_clone()
        .map(|g| {
            let stats = g.get_stats();
            (stats.total_functions, stats.total_files)
        })
        .unwrap_or((0, 0));

    let embedding_enabled = storage.storage
        .get_config()
        .map(|c| c.codebase.enable_embedding)
        .unwrap_or(false);

    let indexing_status = {
        let tasks = storage.storage.vector_tasks.lock().unwrap();
        if tasks.contains(&repo_path) {
            "indexing".to_string()
        } else {
            "idle".to_string()
        }
    };

    Json(ApiResponse {
        success: true,
        data: StatusResponse {
            repo_path,
            project_id,
            total_functions,
            total_files,
            embedding_enabled,
            indexing_status,
        },
    })
}

/// Fallback provider when Tantivy index is unavailable.
/// Always returns empty results to trigger dense-only fallback.
struct FallbackTextSearchProvider;

#[async_trait::async_trait]
impl crate::storage::traits_bm25::TextSearchProvider for FallbackTextSearchProvider {
    async fn index_chunks(&self, _chunks: Vec<crate::storage::traits_bm25::CodeChunk>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn search(&self, _query: &str, _limit: usize) -> anyhow::Result<Vec<crate::storage::traits_bm25::TextSearchResult>> {
        Ok(Vec::new())
    }
    async fn remove_by_path(&self, _file_path: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn commit(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_ready(&self) -> bool {
        false
    }
    async fn document_count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }
}
