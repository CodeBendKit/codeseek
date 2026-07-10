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
use tracing::{info, error, debug};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use async_trait::async_trait;
use futures::TryStreamExt;
use rusqlite::{params, Connection as SqliteConnection};
use anyhow::anyhow;

use crate::codegraph::treesitter::TreeSitterParser;
use crate::codegraph::parser::CodeParser;
use crate::config::Config;
use crate::storage::traits_bm25::{TextSearchProvider, CodeChunk};

use lancedb::table::OptimizeAction;
use lance::dataset::optimize::CompactionOptions;
use std::sync::atomic::{AtomicU64, Ordering};
use chrono::TimeDelta;

/// 累积软删除次数阈值，达到后自动触发 LanceDB compaction
const OPTIMIZE_DELETE_THRESHOLD: u64 = 10;

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
    async fn get_embedding(&self, text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>>;
    /// Batch embed multiple texts in one API call. Default falls back to sequential single calls.
    async fn get_embeddings_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.get_embedding(text).await?);
        }
        Ok(results)
    }
    fn model(&self) -> String;
}

pub struct OpenAICompatibleEmbeddingProvider {
    client: Client,
    api_token: String,
    base_url: String,
    model: String,
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
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAICompatibleEmbeddingProvider {
    fn model(&self) -> String {
        self.model.clone()
    }

    async fn get_embedding(&self, code_block: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        if code_block.is_empty() {
            return Err("Code block is empty".into());
        }

        // Truncate if too long (approx 32k tokens, safe limit 30k chars for now)
        // Note: 32K context window is quite large, but we should still have a safety limit
        // Assuming ~4 chars per token for English, 32k tokens is ~128k chars.
        // For mixed content, being conservative with 64k chars is safe.
        let code_block = if code_block.len() > 64000 {
            &code_block[..64000]
        } else {
            code_block
        };
        
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

        let texts: Vec<&str> = texts.iter()
            .map(|t| if t.len() > 64000 { &t[..64000] } else { t.as_str() })
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
             dimensions = embedding_config.dimensions as i32;
        }
        
        let api_token = api_token.ok_or("API Key not found in config or environment")?;
        
        let provider = OpenAICompatibleEmbeddingProvider::new(api_token, base_url, model);
        
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
                // P0: Skip short code blocks to improve retrieval quality
                // See: docs/retrieval-quality-analysis.md
                if code_block.trim().chars().count() < self.min_code_block_length {
                    debug!("Skipping short symbol '{}' ({} chars, min: {})", 
                        name, code_block.len(), self.min_code_block_length);
                    continue;
                }
                
                // Index into BM25 if available
                if self.bm25_index.is_some() {
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
        let compact_options = CompactionOptions {
            target_rows_per_fragment: 1024 * 1024,  // ~1M 行每文件
            max_rows_per_group: 1024,                // 1K 行每组
            materialize_deletions: true,             // 强制物理删除软删除的行
            materialize_deletions_threshold: 0.1,    // 10% 删除阈值作为后备
            num_threads: Some(4),                    // 4 线程并行
            // 以下为新版本必填字段
            max_bytes_per_file: None,
            batch_size: None,
            io_buffer_size: None,
            defer_index_remap: false,
            compaction_mode: None,
            enable_binary_copy: false,
            enable_binary_copy_force: false,
            binary_copy_read_batch_bytes: Some(16 * 1024 * 1024),
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

    /// Search for code blocks using semantic search
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, anyhow::Error> {
        // 1. Generate embedding for the query
        let query_vector = self.embedding_provider.get_embedding(query).await.map_err(|e| anyhow::anyhow!("{}", e))?;

        // 2. Search in LanceDB
        let table = self.connection.open_table(&self.table_name).execute().await?;
        
        let mut results_stream = table.query()
            .nearest_to(query_vector)?
            .limit(limit)
            .execute()
            .await?;
            
        // 3. Parse results
        let mut search_results = Vec::new();
        
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

}
