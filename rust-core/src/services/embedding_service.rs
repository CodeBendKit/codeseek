use std::path::Path;
use std::fs;
use std::sync::{Arc, Mutex};
use std::env;
use lancedb::{connect, Connection};
use lancedb::query::{QueryBase, ExecutableQuery};
use arrow::array::{
    FixedSizeListBuilder, Float32Builder, Int64Builder, RecordBatch, StringBuilder, AsArray
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatchIterator;
use uuid::Uuid;
use tracing::{info, error, debug, warn};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use async_trait::async_trait;
use futures::TryStreamExt;
use rusqlite::{params, Connection as SqliteConnection};
use anyhow::anyhow;

use crate::codegraph::treesitter::TreeSitterParser;
use crate::codegraph::parser::CodeParser;
use crate::config::{Config, DEFAULT_QUERY_INSTRUCT};
use crate::storage::traits_bm25::{TextSearchProvider, CodeChunk};

use lancedb::table::OptimizeAction;
use lance::dataset::optimize::{CompactionMode, CompactionOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use chrono::TimeDelta;

/// 累积软删除次数阈值，达到后自动触发 LanceDB compaction
const OPTIMIZE_DELETE_THRESHOLD: u64 = 10;

/// embedding API 输入的上限，单位为**字节**（不是字符数，也不是 token 数）。
/// 约对应 32k token 的保守取值：混合内容（中英/代码/emoji）下按 ~2 字节/token 估算。
const MAX_EMBEDDING_INPUT_BYTES: usize = 64_000;

/// 按字节上限截断字符串，若截断点落在多字节字符中间则回退到最近的 UTF-8 char boundary，
/// 保证不 panic 且返回值为原串的合法前缀。
///
/// 说明：`max_bytes` 是**字节**上限。`s.len() <= max_bytes` 时原样返回；
/// `max_bytes == 0`（或回退后为 0，如首个多字节字符就超出上限）时返回空串。
fn truncate_to_byte_limit(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 判断代码块是否应进入 dense（向量）通道。
///
/// `min_code_block_length` 是 **dense 专用**阈值，按 `trim()` 后的字符数
/// （`chars().count()`，**不是字节数**）比较：过短的代码块语义信息不足，
/// embedding 信噪比低，会拉低向量检索的整体质量。
///
/// 注意：该阈值**不得**用于 BM25 稀疏通道（见 [`should_index_bm25`]）——
/// 短代码块在词项检索里往往是高价值召回，不应因 dense 的质量门槛被裁掉。
fn should_index_dense(code_block: &str, min_code_block_length: usize) -> bool {
    code_block.trim().chars().count() >= min_code_block_length
}

/// 判断代码块是否应进入 BM25（稀疏）通道。
///
/// BM25 **不做任何长度过滤**：短代码块往往正是标识符 / 关键字查询
/// （`fn`、`id`、`useState` …）的目标，相关性交给融合层的 `short_code_penalty`
/// （见 `services/hybrid_search.rs`）统一降权，而不是在入库前裁掉。
///
/// 这里只排除 `trim()` 后为空的块 —— 空文本产生 0 个 token，写入索引没有意义。
fn should_index_bm25(code_block: &str) -> bool {
    !code_block.trim().is_empty()
}

/// 按 Qwen3-Embedding 官方模板为**查询**拼接指令前缀：
///
/// ```text
/// Instruct: {task_description}\nQuery:{query}
/// ```
///
/// 格式细节（逐字符固定，官方 README `get_detailed_instruct`）：
/// `Instruct:` 冒号后**有一个空格**；中间是 `\n`；`Query:` 冒号后**没有空格**，紧接查询原文。
///
/// ⚠️ 该函数**只允许出现在查询路径**。文档/代码入库路径（`process_file_content`、
/// `KnowledgeStore::add`）必须继续发送裸文本，否则已建好的 LanceDB 向量索引全部失效。
///
/// - `instruct` 为空（配置显式关闭）→ 原样返回 query，不加任何前缀
/// - `query` 为空白 → 原样返回，保持"空输入报错"的既有行为，不会把指令单独 embedding
pub fn build_instruct_query(instruct: &str, query: &str) -> String {
    if instruct.is_empty() || query.trim().is_empty() {
        return query.to_string();
    }
    format!("Instruct: {}\nQuery:{}", instruct, query)
}

struct EmbeddingCache {
    conn: Mutex<SqliteConnection>,
}

impl EmbeddingCache {
    fn new(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = SqliteConnection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS embedding_cache (
                hash TEXT PRIMARY KEY,
                vector BLOB,
                created_at INTEGER
            )",
            [],
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn get(&self, hash: &str) -> Option<Vec<f32>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT vector FROM embedding_cache WHERE hash = ?1").ok()?;
        let mut rows = stmt.query(params![hash]).ok()?;
        
        if let Some(row) = rows.next().ok()? {
            let blob: Vec<u8> = row.get(0).ok()?;
            bincode::deserialize(&blob).ok()
        } else {
            None
        }
    }

    fn insert(&self, hash: &str, vector: &[f32]) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap();
        let blob = bincode::serialize(vector)?;
        conn.execute(
            "INSERT OR REPLACE INTO embedding_cache (hash, vector, created_at) VALUES (?1, ?2, strftime('%s', 'now'))",
            params![hash, blob],
        )?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

struct CodePoint {
    id: String,
    vector: Vec<f32>,
    file_path: String,
    symbol_name: String,
    symbol_type: String,
    language: String,
    line_start: i64,
    line_end: i64,
    code_block: String,
}

/// 搜索结果（统一使用 score 字段，越大越相关）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub file_path: String,
    pub symbol_name: String,
    pub code_block: String,
    /// 相关性分数，越大表示越相关（统一转换自所有搜索来源）
    pub score: f32,
    // Fields for hybrid search
    pub symbol_type: String,
    pub language: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Get embedding for a single text (used for search queries).
    ///
    /// ⚠️ 输入**原样**发给 API：不在此处、也不在 `get_embeddings_batch` 内加任何指令前缀。
    /// 查询侧前缀只由 [`EmbeddingProvider::format_query`] 在调用点构造。
    async fn get_embedding(&self, text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>>;
    /// Batch embed multiple texts in one API call. Default falls back to sequential single calls.
    async fn get_embeddings_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.get_embedding(text).await?);
        }
        Ok(results)
    }
    /// **查询专用**：按 Qwen3-Embedding 官方模板给查询加指令前缀（非对称检索）。
    ///
    /// 默认实现是恒等变换 —— 不支持 instruction-aware 的模型 / 测试用 mock provider
    /// 保持裸文本。文档、代码、知识记录的入库路径**禁止**调用本方法。
    fn format_query(&self, query: &str) -> String {
        query.to_string()
    }
    fn model(&self) -> String;
}

pub struct OpenAICompatibleEmbeddingProvider {
    client: Client,
    api_token: String,
    base_url: String,
    model: String,
    /// 查询侧指令（来自 `EmbeddingConfig::effective_instruct`）。空串 = 关闭注入。
    /// 只被 `format_query` 读取，`get_embedding` / `get_embeddings_batch` 完全不受影响。
    instruct: String,
}

impl OpenAICompatibleEmbeddingProvider {
    pub fn new(api_token: String, base_url: Option<String>, model: String) -> Self {
        let base_url = base_url.unwrap_or_else(|| "https://api.siliconflow.cn/v1".to_string());
        // Clean up the URL if it contains backticks or extra spaces
        let base_url = base_url.replace('`', "").trim().to_string();
        Self {
            client: Client::new(),
            api_token,
            base_url,
            model,
            // 缺省即官方推荐的 instruction-aware 用法；用 with_instruct("") 关闭
            instruct: DEFAULT_QUERY_INSTRUCT.to_string(),
        }
    }

    /// 设置查询侧 instruct。传入空串表示关闭前缀注入。
    /// 注意：该设置**只影响 `format_query`**，入库文本永远保持裸文本。
    pub fn with_instruct(mut self, instruct: impl Into<String>) -> Self {
        self.instruct = instruct.into();
        self
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAICompatibleEmbeddingProvider {
    fn model(&self) -> String {
        self.model.clone()
    }

    /// 查询侧加官方指令前缀；`instruct` 为空时保持裸文本（等效于关闭）。
    fn format_query(&self, query: &str) -> String {
        build_instruct_query(&self.instruct, query)
    }

    async fn get_embedding(&self, code_block: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        if code_block.is_empty() {
            return Err("Code block is empty".into());
        }

        // Truncate if too long — 32K 上下文窗口很大，但仍需保守的安全上限。
        // MAX_EMBEDDING_INPUT_BYTES 是**字节**上限（不是字符数，约对应 32k token）；
        // 截断点会对齐到 UTF-8 char boundary，避免在多字节字符（中文/emoji）中间切片导致 panic。
        let code_block = truncate_to_byte_limit(code_block, MAX_EMBEDDING_INPUT_BYTES);
        
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let response = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": self.model, 
                "input": code_block,
                "encoding_format": "float"
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            return Err(format!("API request failed with status {}: {}", status, text).into());
        }

        let embedding_response: EmbeddingResponse = response.json().await?;
        
        if let Some(data) = embedding_response.data.into_iter().next() {
            Ok(data.embedding)
        } else {
            Err("No embedding data returned from API".into())
        }
    }

    /// Batch embed multiple texts in a single API call — 20x faster than sequential.
    async fn get_embeddings_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // 每条输入按字节上限截断，截断点对齐到 UTF-8 char boundary（见 truncate_to_byte_limit）
        let texts: Vec<&str> = texts.iter()
            .map(|t| truncate_to_byte_limit(t, MAX_EMBEDDING_INPUT_BYTES))
            .collect();

        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let response = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": self.model,
                "input": texts,
                "encoding_format": "float"
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await?;
            return Err(format!("Batch API request failed with status {}: {}", status, body).into());
        }

        let embedding_response: EmbeddingResponse = response.json().await?;
        let results: Vec<Vec<f32>> = embedding_response.data.into_iter()
            .map(|d| d.embedding)
            .collect();

        // API may return results in different order; we trust the API's order
        if results.len() != texts.len() {
            return Err(format!("Expected {} embeddings, got {}", texts.len(), results.len()).into());
        }
        Ok(results)
    }
}

pub struct EmbeddingService {
    connection: Connection, 
    pub table_name: String,
    embedding_provider: Box<dyn EmbeddingProvider + Send + Sync>,
    pub dimensions: i32,
    cache: Arc<EmbeddingCache>,
    /// Optional BM25 text search index for sparse channel.
    /// When Some, chunks are indexed here alongside LanceDB vector indexing.
    pub bm25_index: Option<Arc<dyn TextSearchProvider>>,
    /// Minimum code block length (chars after trim) to be indexed.
    min_code_block_length: usize,
    /// 自上次 optimize 以来的 pending 删除操作计数，用于阈值触发优化
    pending_delete_count: AtomicU64,
}

impl EmbeddingService {
    pub async fn new(
        db_path: &str, 
        table_name: String, 
        config: Option<&Config>,
        bm25_index: Option<Arc<dyn TextSearchProvider>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // LanceDB connection (embedded)
        let connection = connect(db_path).execute().await?;
        
        // Initialize Cache — 全局共享缓存，所有项目共用同一份
        // 路径: ~/.codeseek/cache/embedding_cache.sqlite
        let cache_dir = Config::cache_dir();
        fs::create_dir_all(&cache_dir)?;
        let cache = Arc::new(EmbeddingCache::new(cache_dir.join("embedding_cache.sqlite").to_str().unwrap())?);

        // Initialize HTTP client and get API token
        let mut api_token = env::var("SILICONFLOW_API_KEY").ok();
        let mut base_url = None;
        let mut model = "Qwen/Qwen3-Embedding-4B".to_string(); // Default fallback
        let mut dimensions = 2560;
        // 查询侧指令：无配置时用默认指令；配置里显式空串时关闭注入
        let mut instruct = DEFAULT_QUERY_INSTRUCT.to_string();

        if let Some(conf) = config {
             let embedding_config = &conf.embedding;
             if !embedding_config.api_token.is_empty() {
                 api_token = Some(embedding_config.api_token.clone());
             }
             if !embedding_config.api_base_url.is_empty() {
                 base_url = Some(embedding_config.api_base_url.clone());
             }
             if !embedding_config.model.is_empty() {
                 model = embedding_config.model.clone();
             }
             instruct = embedding_config.effective_instruct().to_string();
             dimensions = embedding_config.dimensions as i32;
        }
        
        let api_token = api_token.ok_or("API Key not found in config or environment")?;
        
        let provider = OpenAICompatibleEmbeddingProvider::new(api_token, base_url, model)
            .with_instruct(instruct);
        
        let min_code_block_length = config
            .map(|cfg| cfg.index.min_code_block_length)
            .unwrap_or(16);
        
        Ok(Self {
            connection,
            table_name,
            embedding_provider: Box::new(provider),
            dimensions,
            cache,
            bm25_index,
            min_code_block_length,
            pending_delete_count: AtomicU64::new(0),
        })
    }
    
    /// Create a new VectorizeService with a custom embedding provider
    pub async fn new_with_provider(
        db_path: &str, 
        table_name: String, 
        provider: Box<dyn EmbeddingProvider>
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let connection = connect(db_path).execute().await?;
        
        // Initialize Cache — 全局共享缓存，所有项目共用同一份
        // 路径: ~/.codeseek/cache/embedding_cache.sqlite
        let cache_dir = Config::cache_dir();
        fs::create_dir_all(&cache_dir)?;
        let cache_path = cache_dir.join("embedding_cache.sqlite");
        let cache = Arc::new(EmbeddingCache::new(cache_path.to_str().unwrap())?);

       Ok(Self {
            connection,
            table_name,
            embedding_provider: provider,
            dimensions: 2560,
            cache,
            bm25_index: None,
            min_code_block_length: 16,
            pending_delete_count: AtomicU64::new(0),
        })
    }

    /// Create or get the collection (table)
    pub async fn ensure_collection(&self) -> Result<(), Box<dyn std::error::Error>> {
        let table_names = self.connection.table_names().execute().await?;
        if !table_names.contains(&self.table_name) {
            info!("Creating table: {}", self.table_name);
            
            // Qwen/Qwen3-Embedding-4B has 2560 dimensions
            let vector_size = self.dimensions; 
            
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Utf8, false),
                Field::new("vector", DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    vector_size
                ), false),
                Field::new("file_path", DataType::Utf8, false),
                Field::new("symbol_name", DataType::Utf8, false),
                Field::new("symbol_type", DataType::Utf8, false),
                Field::new("language", DataType::Utf8, false),
                Field::new("line_start", DataType::Int64, false),
                Field::new("line_end", DataType::Int64, false),
                Field::new("code_block", DataType::Utf8, false),
            ]));
            
            self.connection.create_empty_table(&self.table_name, schema).execute().await?;
            info!("Table {} created successfully", self.table_name);
        } else {
            info!("Table {} already exists", self.table_name);
        }

        Ok(())
    }

    /// Vectorize directory
    pub async fn vectorize_directory(&self, dir_path: &str, existing_hashes: Option<&std::collections::HashMap<String, String>>) -> Result<std::collections::HashMap<String, String>, Box<dyn std::error::Error>> {
        info!("Starting vectorization of directory: {}", dir_path);
        
        let mut parser = CodeParser::new();
        let mut ts_parser = TreeSitterParser::new();
        
        let path = Path::new(dir_path);
        let files = parser.scan_directory(path);
        
        info!("Found {} files to vectorize", files.len());
        let mut total_vectors = 0;
        let mut new_hashes = std::collections::HashMap::new();
        
        for file_path in files {
            // Calculate MD5
            let content = match fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to read file {}: {}", file_path.display(), e);
                    continue;
                }
            };
            let hash = format!("{:x}", md5::compute(&content));
            let file_key = file_path.to_string_lossy().to_string();
            
            new_hashes.insert(file_key.clone(), hash.clone());

            // Check if modified
            if let Some(hashes) = existing_hashes {
                if let Some(old_hash) = hashes.get(&file_key) {
                    if old_hash == &hash {
                        continue;
                    }
                }
            }

            // ── 混淆 JS 文件过滤：避免将混淆代码索引到向量库中 ──
            if file_path.extension().map(|e| e == "js" || e == "jsx").unwrap_or(false) {
                if crate::detector::analyze_js_code(&content).code_type == crate::detector::CodeType::CompiledCode {
                    info!("Skipping obfuscated JS file: {}", file_path.display());
                    continue;
                }
            }

            match self.process_file_content(&file_path, &content, &mut ts_parser).await {
                Ok(vectors) => {
                    total_vectors += vectors;
                    info!("File {} processed successfully with {} vectors", file_path.display(), vectors);
                }
                Err(e) => {
                    error!("Failed to process file {}: {}", file_path.display(), e);
                }
            }
        }
        
        // Clean up embeddings for files that were deleted since last run
        if let Some(hashes) = existing_hashes {
            let mut deleted_count = 0;
            for old_file in hashes.keys() {
                if !new_hashes.contains_key(old_file) {
                    info!("Cleaning up embeddings for deleted file: {}", old_file);
                    if let Err(e) = self.delete_file_embeddings(old_file).await {
                        error!("Failed to delete LanceDB embeddings for {}: {}", old_file, e);
                    } else {
                        deleted_count += 1;
                    }
                    // Also clean up BM25 index entries for the deleted file
                    if let Some(ref bm25) = self.bm25_index {
                        if let Err(e) = bm25.remove_by_path(old_file).await {
                            error!("Failed to remove BM25 entries for {}: {}", old_file, e);
                        }
                    }
                }
            }
            if deleted_count > 0 {
                info!("Cleaned up embeddings for {} deleted files", deleted_count);
            }
        }

        info!("Vectorization completed. Total vectors created: {}", total_vectors);

        // ── Post-batch: LanceDB storage optimization ──
        // 批量处理完成后进行 compact + prune，回收软删除行的磁盘空间
        // 非致命：即使优化失败，数据依然正确，仅磁盘回收延迟
        if let Err(e) = self.optimize_lancedb().await {
            error!("LanceDB post-batch optimization failed (non-fatal): {e}");
        }

        Ok(new_hashes)
    }

    /// Load persisted file hashes from project index directory.
    /// Returns None if the hashes file doesn't exist or is corrupted.
    pub fn load_hashes(project_hash: &str) -> Option<std::collections::HashMap<String, String>> {
        let path = Config::project_index_dir(project_hash).join("embedding_hashes.json");
        if !path.exists() {
            return None;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to read embedding hashes file: {}", e);
                return None;
            }
        };
        let hashes: std::collections::HashMap<String, String> = match serde_json::from_str(&content) {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to parse embedding hashes: {}", e);
                return None;
            }
        };
        info!("Loaded {} file hashes from {:?}", hashes.len(), path);
        Some(hashes)
    }

    /// Persist file hashes to project index directory.
    pub fn save_hashes(project_hash: &str, hashes: &std::collections::HashMap<String, String>) -> Result<(), Box<dyn std::error::Error>> {
        let path = Config::project_index_dir(project_hash).join("embedding_hashes.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(hashes)?;
        fs::write(&path, content)?;
        info!("Saved {} file hashes to {:?}", hashes.len(), path);
        Ok(())
    }

    /// Delete existing embeddings for a file
    async fn delete_file_embeddings(&self, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let table = self.connection.open_table(&self.table_name).execute().await?;
        // Delete rows where file_path matches
        let predicate = format!("file_path = '{}'", file_path.replace("'", "''"));
        table.delete(&predicate).await?;

        // ── 更新 pending 删除计数 ──
        let count = self.pending_delete_count.fetch_add(1, Ordering::Relaxed) + 1;
        debug!("LanceDB soft-delete recorded, pending_deletes={}", count);

        Ok(())
    }

    /// Process single file content
    async fn process_file_content(&self, file_path: &Path, _content: &str, ts_parser: &mut TreeSitterParser) -> Result<usize, Box<dyn std::error::Error>> {
        // Delete existing LanceDB embeddings for this file to prevent duplicates
        self.delete_file_embeddings(&file_path.to_string_lossy()).await?;

        // Also clean up BM25 index entries for this file before re-adding
        if let Some(ref bm25) = self.bm25_index {
            let fp = file_path.to_string_lossy();
            if let Err(e) = bm25.remove_by_path(&fp).await {
                error!("Failed to remove BM25 entries for {}: {}", fp, e);
            }
        }

        // Parse with TreeSitter
        let symbols = ts_parser.parse_file(&file_path.to_path_buf())?;
        
        let mut vectors_created = 0;
        let mut points = Vec::new();
        let mut bm25_chunks: Vec<CodeChunk> = Vec::new();
        // Collect cache-miss items for batch embedding (20-50x speedup)
        let mut cache_miss_queue: Vec<(String, String, String, String, String, usize, usize, String)> = Vec::new();
        const BATCH_SIZE: usize = 20;
        
        for symbol in symbols {
            // Extract data and drop guard immediately
            let extracted = {
                let symbol_guard = symbol.read();
                let symbol_ref = symbol_guard.as_ref();
                
                match symbol_ref.symbol_type() {
                    crate::codegraph::treesitter::structs::SymbolType::StructDeclaration |
                    crate::codegraph::treesitter::structs::SymbolType::FunctionDeclaration => {
                        let symbol_info = symbol_ref.symbol_info_struct();
                        let code_block = symbol_info.get_content_from_file_blocked()
                            .unwrap_or_else(|e| {
                                eprintln!("Warning: Failed to get content for {}: {}", symbol_ref.name(), e);
                                symbol_ref.name().to_string()
                            });
                        
                        Some((
                            code_block,
                            symbol_ref.name().to_string(),
                            format!("{:?}", symbol_ref.symbol_type()),
                            format!("{:?}", symbol_ref.language()),
                            symbol_ref.full_range().start_point.row,
                            symbol_ref.full_range().end_point.row,
                        ))
                    }
                    _ => None,
                }
            };

            if let Some((code_block, name, symbol_type_str, language_str, start_row, end_row)) = extracted {
                // ── 通道分层：先判 BM25（稀疏），再判 dense（向量）──
                //
                // 两个通道的质量判据不同，必须分开处理，不能用同一把尺子：
                //   * BM25 是词项匹配，短代码块（`fn f()`、`x`）往往正是标识符 / 关键字查询的
                //     目标，因此**不做长度过滤**，只排除 trim 后为空的块（空文本产生 0 个 token，
                //     写入索引无意义）。相关性由融合层的 short_code_penalty 统一降权。
                //   * dense 阈值（min_code_block_length）用于避免语义信息不足的块拉低向量检索
                //     信噪比，**只作用于向量通道**，绝不能反过来把 BM25 候选一起裁掉。
                // 规则来源：混合检索分层设计（dense 控语义质量 / BM25 控词项召回 / 融合层控排序）。
                if self.bm25_index.is_some() && should_index_bm25(&code_block) {
                    let chunk = CodeChunk::new(
                        file_path.to_string_lossy().into_owned(),
                        code_block.clone(),
                        name.clone(),
                        symbol_type_str.clone(),
                        language_str.clone(),
                        start_row + 1,
                        end_row + 1,
                    );
                    bm25_chunks.push(chunk);
                }

                // dense 通道长度过滤：判据与日志口径统一为**字符数**（trim 后 chars().count()），
                // 避免中文代码块下「字节数 vs 字符数」口径不一致导致排障误判。
                if !should_index_dense(&code_block, self.min_code_block_length) {
                    let code_chars = code_block.trim().chars().count();
                    debug!(
                        "Skipping short symbol '{}' from dense channel ({} chars, min: {})",
                        name, code_chars, self.min_code_block_length
                    );
                    // 仅当该块确实已进 BM25（有稀疏索引且非空块）时提示，避免空块场景误报
                    if self.bm25_index.is_some() && should_index_bm25(&code_block) {
                        // 已进 BM25、仅被 dense 过滤：便于排障时区分「未召回」与「只走稀疏通道」
                        debug!(
                            "Symbol '{}' kept in BM25 sparse channel only ({} chars < dense min {})",
                            name, code_chars, self.min_code_block_length
                        );
                    }
                    continue;
                }

                // Check cache first; collect cache-miss items for batch processing
                let model = self.embedding_provider.model();
                let hash_input = format!("{}{}", model, code_block);
                let hash = format!("{:x}", md5::compute(&hash_input));

                let embedding = if let Some(cached) = self.cache.get(&hash) {
                    cached
                } else {
                    // Queue for batch embedding
                    cache_miss_queue.push((
                        hash, code_block.clone(), name.clone(),
                        symbol_type_str.clone(), language_str.clone(),
                        start_row + 1, end_row + 1,
                        file_path.to_string_lossy().into_owned(),
                    ));
                    continue; // skip CodePoint creation — handled after batch
                };

                // Create point
                let point = CodePoint {
                    id: Uuid::new_v4().to_string(),
                    vector: embedding,
                    file_path: file_path.to_string_lossy().to_string(),
                    symbol_name: name,
                    symbol_type: symbol_type_str,
                    language: language_str,
                    line_start: (start_row + 1) as i64,
                    line_end: (end_row + 1) as i64,
                    code_block,
                };
                
                debug!("Point created for symbol: {}", point.symbol_name);
                points.push(point);
                vectors_created += 1;
                
                // Batch upload every 100 vectors
                if points.len() >= 100 {
                    self.upload_points(&points).await?;
                    points.clear();
                }
            }
        }

        // ── Batch embed all cache misses ──────────────────────
        if !cache_miss_queue.is_empty() {
            info!("Batch-embedding {} cache misses ({}x speedup)", cache_miss_queue.len(), BATCH_SIZE);
            for chunk in cache_miss_queue.chunks(BATCH_SIZE) {
                let codes: Vec<String> = chunk.iter().map(|(_, code, _, _, _, _, _, _)| code.clone()).collect();
                // 文档/入库侧：发送**裸源码**，禁止调用 format_query —— 否则已有向量索引全部失效
                match self.embedding_provider.get_embeddings_batch(&codes).await {
                    Ok(embeddings) => {
                        for (item, vec) in chunk.iter().zip(embeddings) {
                            let (hash, _code, name, symbol_type_str, language_str, line_start, line_end, file_path) = item;
                            // Cache the result
                            if let Err(e) = self.cache.insert(hash, &vec) {
                                error!("Failed to cache embedding for {}: {}", name, e);
                            }
                            // Create CodePoint
                            points.push(CodePoint {
                                id: Uuid::new_v4().to_string(),
                                vector: vec,
                                file_path: file_path.clone(),
                                symbol_name: name.clone(),
                                symbol_type: symbol_type_str.clone(),
                                language: language_str.clone(),
                                line_start: *line_start as i64,
                                line_end: *line_end as i64,
                                code_block: _code.clone(),
                            });
                            vectors_created += 1;
                        }
                        // Batch upload every 100
                        if points.len() >= 100 {
                            self.upload_points(&points).await?;
                            points.clear();
                        }
                    }
                    Err(e) => {
                        error!("Batch embedding failed: {}", e);
                    }
                }
            }
        }

        // Upload remaining vectors
        if !points.is_empty() {
            self.upload_points(&points).await?;
        }
        
        // Batch index chunks into BM25
        if !bm25_chunks.is_empty() {
            if let Some(bm25) = &self.bm25_index {
                if let Err(e) = bm25.index_chunks(bm25_chunks.clone()).await {
                    tracing::warn!("BM25 indexing failed for {:?}: {}", file_path, e);
                    // Non-fatal: continue with vector indexing
                }
            }
        }
        
        // ── 阈值触发优化检查 ──
        // 当累积软删除操作达到阈值时，自动触发 compact + prune
        let count = self.pending_delete_count.load(Ordering::Relaxed);
        if count >= OPTIMIZE_DELETE_THRESHOLD {
            info!(
                "Pending deletes ({}) reached threshold ({}), triggering optimization",
                count, OPTIMIZE_DELETE_THRESHOLD
            );
            if let Err(e) = self.optimize_lancedb().await {
                error!("Threshold-triggered LanceDB optimization failed (non-fatal): {e}");
            }
        }
        
        Ok(vectors_created)
    }

    /// Upload vectors to LanceDB
    async fn upload_points(&self, points: &[CodePoint]) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Uploading {} vectors to LanceDB", points.len());
        
        if points.is_empty() {
            return Ok(());
        }

        let vector_size = self.dimensions;

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("vector", DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                vector_size
            ), false),
            Field::new("file_path", DataType::Utf8, false),
            Field::new("symbol_name", DataType::Utf8, false),
            Field::new("symbol_type", DataType::Utf8, false),
            Field::new("language", DataType::Utf8, false),
            Field::new("line_start", DataType::Int64, false),
            Field::new("line_end", DataType::Int64, false),
            Field::new("code_block", DataType::Utf8, false),
        ]));

        // Build arrays
        let mut id_builder = StringBuilder::new();
        let mut vector_builder = FixedSizeListBuilder::new(Float32Builder::new(), vector_size);
        let mut file_path_builder = StringBuilder::new();
        let mut symbol_name_builder = StringBuilder::new();
        let mut symbol_type_builder = StringBuilder::new();
        let mut language_builder = StringBuilder::new();
        let mut line_start_builder = Int64Builder::new();
        let mut line_end_builder = Int64Builder::new();
        let mut code_block_builder = StringBuilder::new();
        
        for p in points {
            id_builder.append_value(&p.id);
            
            // Ensure vector size matches
            if p.vector.len() != vector_size as usize {
                error!("Vector size mismatch: expected {}, got {}", vector_size, p.vector.len());
                continue;
            }

            vector_builder.values().append_slice(&p.vector);
            vector_builder.append(true);
            
            file_path_builder.append_value(&p.file_path);
            symbol_name_builder.append_value(&p.symbol_name);
            symbol_type_builder.append_value(&p.symbol_type);
            language_builder.append_value(&p.language);
            line_start_builder.append_value(p.line_start);
            line_end_builder.append_value(p.line_end);
            code_block_builder.append_value(&p.code_block);
        }
        
        let batch = RecordBatch::try_new(schema.clone(), vec![
            Arc::new(id_builder.finish()),
            Arc::new(vector_builder.finish()),
            Arc::new(file_path_builder.finish()),
            Arc::new(symbol_name_builder.finish()),
            Arc::new(symbol_type_builder.finish()),
            Arc::new(language_builder.finish()),
            Arc::new(line_start_builder.finish()),
            Arc::new(line_end_builder.finish()),
            Arc::new(code_block_builder.finish()),
        ])?;
        
        let table = self.connection.open_table(&self.table_name).execute().await?;
        
        let batches = vec![Ok(batch)];
        let data: Box<dyn arrow::array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(batches, schema));
        table.add(data).execute().await?;

        Ok(())
    }

    /// 优化 LanceDB 存储：compact 碎片整理 + prune 旧版本清理
    ///
    /// # 功能
    /// 1. **Compact**: 物理合并且删除已标记为删除的行，回收磁盘空间
    /// 2. **Prune**: 清理旧的版本文件
    ///
    /// # 何时调用
    /// - `vectorize_directory()` 批量处理完成后自动调用
    /// - 当 `pending_delete_count` 达到阈值时自动调用
    pub async fn optimize_lancedb(&self) -> anyhow::Result<()> {
        info!("Starting LanceDB storage optimization (compact + prune)");

        let table = self.connection.open_table(&self.table_name).execute().await
            .map_err(|e| anyhow::anyhow!("Failed to open LanceDB table for optimization: {e}"))?;

        // ── Step 1: Compact ──
        // 合并小文件碎片，物理删除被标记为已删除的行
        #[allow(deprecated)]
        let compact_options = CompactionOptions {
            target_rows_per_fragment: 1024 * 1024,  // ~1M 行每文件
            max_rows_per_group: 1024,                // 1K 行每组
            materialize_deletions: true,             // 强制物理删除软删除的行
            materialize_deletions_threshold: 0.1,    // 10% 删除阈值作为后备
            num_threads: Some(4),                    // 4 线程并行
            max_bytes_per_file: None,
            batch_size: None,
            io_buffer_size: None,
            defer_index_remap: false,
            compaction_mode: Some(CompactionMode::Reencode),
            binary_copy_read_batch_bytes: None,
            enable_binary_copy: false,
            enable_binary_copy_force: false,
            max_source_fragments: None,
            transaction_properties: None,
        };

        let _compact_stats = table
            .optimize(OptimizeAction::Compact {
                options: compact_options,
                remap_options: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("LanceDB compact failed: {e}"))?;

        info!("LanceDB compact completed");

        // ── Step 2: Prune ──
        // 清理 compact 后遗留的旧版本文件
        let _prune_stats = table
            .optimize(OptimizeAction::Prune {
                older_than: Some(TimeDelta::zero()),   // 立即清理所有非最新版本
                delete_unverified: Some(false),        // 安全：只删除已验证的版本
                error_if_tagged_old_versions: None,    // 不检查 tagged 版本
            })
            .await
            .map_err(|e| anyhow::anyhow!("LanceDB prune failed: {e}"))?;

        info!("LanceDB prune completed");

        // 重置 pending 删除计数
        self.pending_delete_count.store(0, Ordering::Relaxed);

        Ok(())
    }

    /// **查询专用** helper：按配置的 instruct 把原始查询包装成官方非对称检索输入。
    ///
    /// 仅供检索入口调用；入库/文档路径（`process_file_content` → `get_embeddings_batch`）
    /// 必须继续发送裸源码，否则历史 LanceDB 向量与查询向量落在不同分布。
    pub fn format_query(&self, query: &str) -> String {
        self.embedding_provider.format_query(query)
    }

    /// Search for code blocks using semantic search
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, anyhow::Error> {
        // 1. Generate embedding for the query
        //    只对 query 侧注入 instruct 前缀（Qwen3-Embedding instruction-aware / 非对称检索）
        let formatted_query = self.embedding_provider.format_query(query);
        let query_vector = self.embedding_provider.get_embedding(&formatted_query).await.map_err(|e| anyhow::anyhow!("{}", e))?;

        // 2. Search in LanceDB
        let table = self.connection.open_table(&self.table_name).execute().await?;
        
        let mut results_stream = table.query()
            .nearest_to(query_vector)?
            .limit(limit)
            .execute()
            .await?;
            
        // 3. Parse results
        let mut search_results = Vec::new();
        // 旧索引表可能没有 line_start 列（ensure_collection 不做列迁移），只警告一次，避免逐行刷屏
        let mut warned_missing_line_start = false;
        
        while let Some(batch) = results_stream.try_next().await? {
            let file_path_col = batch.column_by_name("file_path").ok_or(anyhow!("Missing file_path column"))?.as_string::<i32>();
            let symbol_name_col = batch.column_by_name("symbol_name").ok_or(anyhow!("Missing symbol_name column"))?.as_string::<i32>();
            let code_block_col = batch.column_by_name("code_block").ok_or(anyhow!("Missing code_block column"))?.as_string::<i32>();
            
            let dist_col = batch.column_by_name("_distance");
            let dist_vals = if let Some(d) = dist_col {
                d.as_any().downcast_ref::<arrow::array::Float32Array>()
            } else {
                None
            };

            for i in 0..batch.num_rows() {
                let file_path = file_path_col.value(i).to_string();
                if !std::path::Path::new(&file_path).exists() {
                    continue;
                }
                // L2 距离 → 相关性分数 (0,1]，越大越相关
                let distance = if let Some(d) = dist_vals { d.value(i) } else { 0.0 };
                let score = (1.0 / (1.0 + distance)) as f32;
                
                // Try to get additional fields from LanceDB batch
                let symbol_type_col = batch.column_by_name("symbol_type");
                let language_col = batch.column_by_name("language");
                let line_start_col = batch.column_by_name("line_start");
                if line_start_col.is_none() && !warned_missing_line_start {
                    warn!(
                        "LanceDB 表 '{}' 缺少 line_start 列（旧索引 schema），dense 结果的行号将为 0，建议重建索引",
                        self.table_name
                    );
                    warned_missing_line_start = true;
                }
                let line_end_col = batch.column_by_name("line_end");

                let symbol_type = symbol_type_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
                    .map(|col| col.value(i).to_string())
                    .unwrap_or_default();

                let language = language_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
                    .map(|col| col.value(i).to_string())
                    .unwrap_or_default();

                let line_start = line_start_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::Int64Array>())
                    .map(|col| col.value(i) as usize)
                    .unwrap_or(0);

                let line_end = line_end_col
                    .and_then(|c| c.as_any().downcast_ref::<arrow::array::Int64Array>())
                    .map(|col| col.value(i) as usize)
                    .unwrap_or(0);

                search_results.push(SearchResult {
                    file_path,
                    symbol_name: symbol_name_col.value(i).to_string(),
                    code_block: code_block_col.value(i).to_string(),
                    score,
                    symbol_type,
                    language,
                    line_start,
                    line_end,
                });
            }
        }
        
        Ok(search_results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockEmbeddingProvider {
        call_count: Arc<AtomicUsize>,
    }

    impl MockEmbeddingProvider {
        fn new() -> Self {
            Self {
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbeddingProvider {
        fn model(&self) -> String {
            "mock-model".to_string()
        }

        async fn get_embedding(&self, _text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Return a dummy vector of size 2560
            Ok(vec![0.1; 2560])
        }
    }

    #[tokio::test]
    async fn test_search() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let db_path = dir.path().to_str().unwrap();
        let table_name = "test_vectors".to_string();

        let provider = Box::new(MockEmbeddingProvider::new());
        let service = EmbeddingService::new_with_provider(db_path, table_name.clone(), provider).await?;
        
        service.ensure_collection().await?;

        // Create a dummy file that exists so it passes the existence check
        let test_file_path = dir.path().join("test.rs");
        std::fs::write(&test_file_path, "fn test_fn() {}")?;
        let test_file_path_str = test_file_path.to_str().unwrap().to_string();

        // Manually insert some data using upload_points
        let point = CodePoint {
            id: Uuid::new_v4().to_string(),
            vector: vec![0.1; 2560],
            file_path: test_file_path_str.clone(),
            symbol_name: "test_fn".to_string(),
            symbol_type: "Function".to_string(),
            language: "Rust".to_string(),
            line_start: 1,
            line_end: 10,
            code_block: "fn test_fn() {}".to_string(),
        };

        service.upload_points(&[point]).await?;

        // Search
        let results = service.search("test query", 5).await?;
        
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol_name, "test_fn");
        assert_eq!(results[0].file_path, test_file_path_str);

        Ok(())
    }

    // ---- query-side instruct prefix (Qwen3-Embedding asymmetric retrieval) ----

    /// ①正常指令 + 查询：与官方 `get_detailed_instruct` 模板**逐字节**一致。
    /// `Instruct:` 冒号后有一个空格，中间是 \n，`Query:` 冒号后**没有空格**。
    #[test]
    fn build_instruct_query_matches_official_template_byte_for_byte() {
        let out = build_instruct_query(
            "Given a web search query, retrieve relevant passages that answer the query",
            "how to parse toml",
        );
        assert_eq!(
            out.as_bytes(),
            b"Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:how to parse toml"
        );
        assert_eq!(
            out,
            "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:how to parse toml"
        );
        // 结构细节：前缀以 "Instruct: " 开头，"Query:" 后紧跟原文且无空格
        assert!(out.starts_with("Instruct: "));
        assert!(out.contains("\nQuery:how"));
        assert!(!out.contains("Query: h"), "Query: 之后不能有空格");
    }

    /// ②instruct 为空字符串（配置显式关闭）→ 返回原查询，不加任何前缀。
    #[test]
    fn build_instruct_query_with_empty_instruct_returns_raw_query() {
        assert_eq!(build_instruct_query("", "find login handler"), "find login handler");
        assert_eq!(build_instruct_query("", "解析配置文件"), "解析配置文件");
        // 空 instruct 绝不产生 "Instruct:" 痕迹
        assert!(!build_instruct_query("", "q").contains("Instruct:"));
    }

    /// ③中文查询原样保留在 `Query:` 之后（不被转写、不被截断、UTF-8 完整）。
    #[test]
    fn build_instruct_query_keeps_chinese_query_verbatim() {
        let instruct = "Given a code search query, retrieve relevant functions or code snippets that answer the query.";
        let query = "如何解析全局配置文件里的嵌入模型参数？";
        let out = build_instruct_query(instruct, query);
        assert_eq!(
            out.as_bytes(),
            format!("Instruct: {instruct}\nQuery:{query}").as_bytes()
        );
        let (head, tail) = out.split_once('\n').expect("one newline separator");
        assert_eq!(head, format!("Instruct: {instruct}"));
        assert_eq!(tail, format!("Query:{query}"));
        assert_eq!(tail.strip_prefix("Query:"), Some(query));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// ④空白查询：不加前缀，保持既有"空输入直接报错"的行为，避免只 embedding 指令本身。
    #[test]
    fn build_instruct_query_with_blank_query_returns_raw_query() {
        assert_eq!(build_instruct_query("some instruct", ""), "");
        assert_eq!(build_instruct_query("some instruct", "   "), "   ");
    }

    /// 默认指令必须是官方建议的英文单句（无换行），且逐字与需求一致。
    #[test]
    fn default_query_instruct_is_the_official_style_english_sentence() {
        assert_eq!(
            DEFAULT_QUERY_INSTRUCT,
            "Given a code search query, retrieve relevant functions or code snippets that answer the query."
        );
        assert!(!DEFAULT_QUERY_INSTRUCT.contains('\n'));
        assert!(DEFAULT_QUERY_INSTRUCT.is_ascii());
    }

    /// provider 层：默认 instruct 生效；with_instruct("") 关闭。
    #[test]
    fn provider_format_query_uses_configured_instruct() {
        let p = OpenAICompatibleEmbeddingProvider::new(
            "sk-not-used".to_string(),
            Some("https://api.siliconflow.cn/v1".to_string()),
            "Qwen/Qwen3-Embedding-4B".to_string(),
        );
        // new() 的缺省即默认指令
        assert_eq!(
            p.format_query("vector store").as_bytes(),
            format!("Instruct: {DEFAULT_QUERY_INSTRUCT}\nQuery:vector store").as_bytes()
        );
        // 自定义
        let p2 = p.with_instruct("Given a question, retrieve matching code".to_string());
        assert_eq!(p2.format_query("abc"), "Instruct: Given a question, retrieve matching code\nQuery:abc");
        // 显式关闭
        let p3 = OpenAICompatibleEmbeddingProvider::new(
            "sk-not-used".to_string(),
            None,
            "Qwen/Qwen3-Embedding-4B".to_string(),
        )
        .with_instruct(String::new());
        assert_eq!(p3.format_query("abc"), "abc");
    }

    /// service 层查询入口把前缀委托给 provider；mock provider（恒等）不受影响。
    #[tokio::test]
    async fn service_format_query_delegates_to_provider() {
        let dir = tempdir().unwrap();
        let provider = Box::new(
            OpenAICompatibleEmbeddingProvider::new(
                "sk-not-used".to_string(),
                None,
                "Qwen/Qwen3-Embedding-4B".to_string(),
            )
            .with_instruct(DEFAULT_QUERY_INSTRUCT.to_string()),
        );
        let service =
            EmbeddingService::new_with_provider(dir.path().to_str().unwrap(), "t".to_string(), provider)
                .await
                .unwrap();
        assert_eq!(
            service.format_query("查找索引重建入口").as_bytes(),
            format!("Instruct: {DEFAULT_QUERY_INSTRUCT}\nQuery:查找索引重建入口").as_bytes()
        );
    }

    /// 入库/文档路径使用的 mock provider 默认 format_query 必须是恒等（前缀是显式调用，不是隐式注入）。
    #[tokio::test]
    async fn mock_provider_format_query_is_identity_by_default() {
        let dir = tempdir().unwrap();
        let provider = Box::new(MockEmbeddingProvider::new());
        let service =
            EmbeddingService::new_with_provider(dir.path().to_str().unwrap(), "t2".to_string(), provider)
                .await
                .unwrap();
        assert_eq!(service.format_query("fn main() {}"), "fn main() {}");
        assert_eq!(service.format_query(""), "");
    }

    /// 端到端（不联网）：`EmbeddingService::new` 必须把 config.embedding.instruct 透传给 provider。
    /// 缺省 → 默认指令；显式空串 → 完全不加前缀。
    #[tokio::test]
    async fn service_new_honours_config_instruct_three_states() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let db = dir.path().to_str().unwrap();

        // 旧配置：没有 instruct 字段 → 默认指令生效
        let cfg: Config = serde_json::from_str(
            r#"{"embedding":{"api_token":"sk-not-used","model":"Qwen/Qwen3-Embedding-4B","dimensions":2560}}"#,
        )?;
        let svc = EmbeddingService::new(db, "t_default".to_string(), Some(&cfg), None).await?;
        assert_eq!(
            svc.format_query("find the entry point").as_bytes(),
            format!("Instruct: {DEFAULT_QUERY_INSTRUCT}\nQuery:find the entry point").as_bytes()
        );

        // 显式空串 → 关闭注入
        let cfg: Config = serde_json::from_str(
            r#"{"embedding":{"api_token":"sk-not-used","dimensions":2560,"instruct":""}}"#,
        )?;
        let svc = EmbeddingService::new(db, "t_off".to_string(), Some(&cfg), None).await?;
        assert_eq!(svc.format_query("find the entry point"), "find the entry point");

        // 自定义指令 → 使用该指令
        let cfg: Config = serde_json::from_str(
            r#"{"embedding":{"api_token":"sk-not-used","dimensions":2560,"instruct":"Given a bug report, retrieve the faulty function"}}"#,
        )?;
        let svc = EmbeddingService::new(db, "t_custom".to_string(), Some(&cfg), None).await?;
        assert_eq!(
            svc.format_query("空指针崩溃"),
            "Instruct: Given a bug report, retrieve the faulty function\nQuery:空指针崩溃"
        );

        // 维度/模型保持不变（本改动不得触碰维度、模型名、请求体结构）
        let cfg: Config = serde_json::from_str(
            r#"{"embedding":{"api_token":"sk-not-used","model":"Qwen/Qwen3-Embedding-4B","dimensions":2560}}"#,
        )?;
        let svc = EmbeddingService::new(db, "t_dims".to_string(), Some(&cfg), None).await?;
        assert_eq!(svc.dimensions, 2560);
        assert_eq!(svc.embedding_provider.model(), "Qwen/Qwen3-Embedding-4B");

        Ok(())
    }

    // ---- 通道分层：dense 长度阈值 vs BM25 不做长度过滤 ----

    /// BM25 通道不做长度过滤：极短代码块（`fn f()`、`x`）也必须被接受，
    /// 只有 trim 后为空的块（0 个 token，入库无意义）才被排除。
    #[test]
    fn should_index_bm25_accepts_short_code() {
        for short in ["fn f()", "x", "}", "id", "useState"] {
            assert!(
                should_index_bm25(short),
                "短代码块 {short:?} 必须被 BM25 通道接受（BM25 无长度阈值）"
            );
        }
        // 空文本 / 纯空白：产生 0 个 token，写入索引没有意义
        assert!(!should_index_bm25(""), "空块不得进入 BM25");
        assert!(!should_index_bm25("   \n\t "), "纯空白块不得进入 BM25");
    }

    /// dense 阈值按 trim 后的字符数比较：9 字符块在 min=16 时被过滤，长块通过；
    /// min=0 时非空块一律通过（与原 `count < min` 判据等价，dense 行为不变）。
    #[test]
    fn should_index_dense_respects_threshold() {
        let short = "fn f() {}";
        assert_eq!(short.chars().count(), 9);
        assert!(
            !should_index_dense(short, 16),
            "9 字符块在 min=16 时必须被 dense 通道过滤"
        );

        let long = "fn long_function_name() -> usize { 42 }";
        assert!(long.chars().count() >= 16);
        assert!(
            should_index_dense(long, 16),
            "长代码块必须通过 dense 阈值"
        );

        // 阈值为 0 → 非空块全部通过
        assert!(should_index_dense(short, 0));
        assert!(should_index_dense(long, 0));

        // 中文块按字符数而非字节数判断：11 字符（33 字节）在 min=16 时仍需过滤
        let chinese = "函数定义示例代码块内容";
        assert_eq!(chinese.chars().count(), 11);
        assert!(
            chinese.len() > 16,
            "该中文块字节数已超过阈值，字符数才是判据"
        );
        assert!(
            !should_index_dense(chinese, 16),
            "dense 阈值必须按字符数而不是字节数判断"
        );
    }

    /// 缺陷回归用例：短代码块不得因 dense 阈值被 BM25 通道丢弃。
    /// 正确分层是「只进 BM25、不进 dense」，而不是「两通道都不进」。
    #[test]
    fn short_code_goes_to_bm25_but_not_dense() {
        let code_block = "x";
        assert_eq!(code_block.chars().count(), 1);

        assert!(
            should_index_bm25(code_block),
            "短代码不得因 dense 阈值被 BM25 丢弃：BM25 通道必须接受 {code_block:?}"
        );
        assert!(
            !should_index_dense(code_block, 16),
            "同一个块仍应被 dense 阈值过滤（dense 侧行为保持不变）"
        );
        // 两个判据互相独立：BM25 为真、dense 为假 —— 即「只进 BM25」
        assert_ne!(
            should_index_bm25(code_block),
            should_index_dense(code_block, 16),
            "短代码应恰好落在「只进 BM25、不进 dense」的分层区间"
        );
    }

    // ---- truncate_to_byte_limit ----

    /// 纯 ASCII 超长串：截断到恰好等于字节上限。
    #[test]
    fn truncate_to_byte_limit_ascii_over_limit() {
        let limit = MAX_EMBEDDING_INPUT_BYTES;
        let mut s = String::with_capacity(limit + 1_000);
        for _ in 0..(limit + 1_000) {
            s.push('a');
        }
        assert_eq!(s.len(), limit + 1_000);

        let t = truncate_to_byte_limit(&s, limit);
        assert_eq!(t.len(), limit);
        assert!(s.starts_with(t));
        assert!(t.is_char_boundary(t.len()));
    }

    /// 多字节字符（中文，3 字节）恰好跨越 64_000 字节边界：不 panic，回退到合法前缀。
    #[test]
    fn truncate_to_byte_limit_multibyte_straddling_limit() {
        let limit = MAX_EMBEDDING_INPUT_BYTES;
        // '中' 占 3 字节，起点放在 limit - 2，使其内部覆盖 limit 这个切点。
        let mut s = String::with_capacity(limit + 16);
        for _ in 0..(limit - 2) {
            s.push('a');
        }
        s.push('中');
        for _ in 0..8 {
            s.push('b');
        }
        assert_eq!(s.len(), limit - 2 + 3 + 8);
        assert!(!s.is_char_boundary(limit));

        let t = truncate_to_byte_limit(&s, limit);
        assert!(t.len() <= limit, "truncated len {} exceeds limit {}", t.len(), limit);
        assert_eq!(t.len(), limit - 2, "should fall back to the preceding char boundary");
        assert!(s.starts_with(t), "result must be a lossless prefix of the input");
        assert!(t.is_char_boundary(t.len()));
    }

    /// 4 字节 emoji 跨越边界：同样回退，不 panic。
    #[test]
    fn truncate_to_byte_limit_emoji_straddling_limit() {
        let limit = MAX_EMBEDDING_INPUT_BYTES;
        let mut s = String::with_capacity(limit + 16);
        for _ in 0..(limit - 1) {
            s.push('a');
        }
        s.push('😀'); // 4 字节，覆盖 limit-1 .. limit+3
        for _ in 0..4 {
            s.push('c');
        }
        assert!(!s.is_char_boundary(limit));

        let t = truncate_to_byte_limit(&s, limit);
        assert_eq!(t.len(), limit - 1);
        assert!(t.len() <= limit);
        assert!(s.starts_with(t));
    }

    /// max_bytes = 0：非空串返回空串，空串返回空串。
    #[test]
    fn truncate_to_byte_limit_zero_max_bytes() {
        assert_eq!(truncate_to_byte_limit("hello", 0), "");
        assert_eq!(truncate_to_byte_limit("中文", 0), "");
        assert_eq!(truncate_to_byte_limit("", 0), "");
    }

    /// s.len() <= max_bytes：原样返回（同一块缓冲区，长度不变）。
    #[test]
    fn truncate_to_byte_limit_within_limit_returns_original() {
        let mut s = String::new();
        for _ in 0..256 {
            s.push('中');
        }
        let exact = s.as_str();
        // 恰好等于上限 —— 原样返回
        assert_eq!(truncate_to_byte_limit(exact, s.len()).as_ptr(), exact.as_ptr());
        assert_eq!(truncate_to_byte_limit(exact, s.len()).len(), s.len());
        // 上限大于长度 —— 原样返回
        assert_eq!(truncate_to_byte_limit(exact, s.len() + 1).as_ptr(), exact.as_ptr());
        // 空串在任何正上限下都是空串
        assert_eq!(truncate_to_byte_limit("", 10), "");
    }

    /// max_bytes 落在首个字符内部（"中" 的 max_bytes = 1）：回退到 0，返回空串。
    #[test]
    fn truncate_to_byte_limit_first_char_straddles_limit() {
        assert_eq!(truncate_to_byte_limit("中", 1), "");
        assert_eq!(truncate_to_byte_limit("中", 2), "");
        assert_eq!(truncate_to_byte_limit("中", 3), "中");
        assert_eq!(truncate_to_byte_limit("😀a", 2), "");
    }

    /// 截断后的字节必须是合法 UTF-8（用 from_utf8 反证，确保不会切开多字节序列）。
    #[test]
    fn truncate_to_byte_limit_result_is_valid_utf8() {
        let mut s = String::new();
        for _ in 0..30_000 {
            s.push_str("中文😀");
        }
        let t = truncate_to_byte_limit(&s, MAX_EMBEDDING_INPUT_BYTES);
        assert!(t.len() <= MAX_EMBEDDING_INPUT_BYTES);
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
        assert!(s.starts_with(t));
    }
}
