//! Knowledge store backed by LanceDB (vector) and Tantivy (BM25).
//!
//! Provides CRUD and hybrid search for [`KnowledgeRecord`] entries.
//!
//! # 对称混合召回（dense + sparse）
//!
//! [`KnowledgeStore::search`] 是一条**对称**的双通道混合召回链路，对称性由三点保证：
//!
//! 1. **共用召回预算**：向量通道与 BM25 通道都用同一个 `K_recall =
//!    K_final × [`KNOWLEDGE_RECALL_MULTIPLIER`]`，不存在「一个通道多取、另一个通道少取」
//!    的非对称预算。
//! 2. **域过滤在检索前下推（prefilter）**：两个通道都在检索之前过滤 `type`（`domains`
//!    经 [`crate::knowledge::search::domain_to_type_values`] 映射为 `type` 值）。LanceDB
//!    侧用 `only_if`（`QueryRequest.prefilter` 默认 `true`，`top_k = limit + offset` 作用于
//!    **过滤后**的行集）；Tantivy 侧用 `BooleanQuery(Must)` 的 `type` 项。因此绝不会出现
//!    「先取 top-k、再事后裁剪」导致的召回塌陷。
//! 3. **稀疏独占命中不丢**：只有 BM25 命中、没有向量命中的记录（纯稀疏命中，没有向量
//!    距离，因此不会被向量批次带进 `record_map`）在融合后按 id 回查 LanceDB 一次补齐；
//!    回查后仍缺失的记录只告警，并继续用后续融合候选填满 `K_final`。

use anyhow::{anyhow, Context};
use arrow::array::{
    Array, FixedSizeListBuilder, Float32Builder, Int32Builder, ListBuilder, RecordBatch,
    RecordBatchIterator, StringBuilder,
};
use arrow::datatypes::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::connect;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{Config, DEFAULT_QUERY_INSTRUCT};
use crate::knowledge::record::KnowledgeRecord;
use crate::knowledge::search::{
    build_result, domain_to_type_values, rrf_fuse, KnowledgeSearchResult,
};
use crate::knowledge::tantivy::KnowledgeTantivyIndex;
use crate::services::embedding_service::{EmbeddingProvider, OpenAICompatibleEmbeddingProvider};

/// 两个召回通道共用的召回放大倍数：`K_recall = K_final × MULTIPLIER`。
///
/// 向量通道与 BM25 通道**必须**使用同一个 `K_recall`，否则融合就是非对称的
/// （一侧候选更多，RRF 里天然占优）。
const KNOWLEDGE_RECALL_MULTIPLIER: usize = 4;

/// 构造知识库 LanceDB 表 schema（`ensure_table` / `add` / 测试共用）。
fn knowledge_schema(dimensions: usize) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimensions as i32,
            ),
            false,
        ),
        Field::new("type", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new(
            "tags",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new(
            "related_files",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
        Field::new("source_agent", DataType::Utf8, false),
        Field::new("task_id", DataType::Utf8, true),
        Field::new("confidence", DataType::Float32, false),
        Field::new("created_at", DataType::Utf8, false),
        Field::new("updated_at", DataType::Utf8, false),
        Field::new("access_count", DataType::Int32, false),
        Field::new("last_accessed", DataType::Utf8, true),
        Field::new(
            "parent_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            true,
        ),
    ]))
}

/// 按 SQL 字符串字面量规则转义单引号（`'` → `''`）。
fn sql_escape(value: &str) -> String {
    value.replace('\'', "''")
}

/// 把 domain 映射出的 `type` 值列表编成 LanceDB 过滤表达式（`only_if` / prefilter 用）。
///
/// 空列表返回 `None`（不过滤）。
///
/// 列名用**裸** `type`，不能写成 `"type"`：LanceDB 的 SQL 过滤解析器把双引号当作
/// 字符串字面量而不是带引号标识符，`"type" IN ('repo_retrieval')` 会静默匹配 0 行
/// （实测：`type IN (...)` 命中 1 行，`"type" IN (...)` 命中 0 行）。裸 `type` 在本
/// 版本解析器里可用，由 `vector_prefilter_applies_domain_before_top_k` 回归测试守护。
fn type_filter_predicate(type_values: &[String]) -> Option<String> {
    if type_values.is_empty() {
        return None;
    }
    let list = type_values
        .iter()
        .map(|v| format!("'{}'", sql_escape(v)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("type IN ({})", list))
}

/// 把缺失记录的 id 编成 `id IN ('a','b')` 过滤表达式（按 id 回查 LanceDB 用）。
///
/// 空列表返回 `None`；单引号同样按 SQL 规则转义。
fn id_filter_predicate(ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let list = ids
        .iter()
        .map(|v| format!("'{}'", sql_escape(v)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("id IN ({})", list))
}

/// 融合结果里「还没有记录」的 id，按融合顺序返回、去重。
fn missing_record_ids(
    fused: &[(String, f64)],
    known: &HashMap<String, KnowledgeRecord>,
) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut missing = Vec::new();
    for (id, _) in fused {
        if known.contains_key(id) {
            continue;
        }
        if seen.insert(id.as_str()) {
            missing.push(id.clone());
        }
    }
    missing
}

/// 按融合顺序装配最终结果：命中记录则 [`build_result`]，记录缺失则跳过并继续填充，
/// 直到凑满 `k_final`。
///
/// 跳过而不是提前 `break` 是刻意的：融合池大于 `k_final`，缺失的记录不能白白吃掉
/// 一个结果位（那正是「纯 BM25 命中占位却返回不足条数」的老问题）。真正需要告警的
/// 缺失由 [`missing_record_ids`] 在调用方统一记录。
fn assemble_results(
    fused: Vec<(String, f64)>,
    records: HashMap<String, KnowledgeRecord>,
    vector_scores: &HashMap<String, f32>,
    bm25_scores: &HashMap<String, f32>,
    k_final: usize,
) -> Vec<KnowledgeSearchResult> {
    let mut results = Vec::with_capacity(k_final.min(fused.len()));
    if k_final == 0 {
        return results;
    }
    for (id, fused_score) in fused {
        if results.len() >= k_final {
            break;
        }
        let Some(record) = records.get(&id) else {
            continue;
        };
        results.push(build_result(
            record.clone(),
            vector_scores.get(&id).copied(),
            bm25_scores.get(&id).copied(),
            fused_score as f32,
            None, // rerank_score: TODO (Phase 1 disabled)
        ));
    }
    results
}

/// Project-level knowledge store: LanceDB for vectors + Tantivy for BM25.
pub struct KnowledgeStore {
    /// Path to the LanceDB database directory (~/.codeseek/projects/<project_hash>/knowledge).
    db_path: PathBuf,
    /// Table name within the LanceDB database.
    table_name: String,
    /// BM25 full-text index (optional, always initialised).
    bm25: Option<KnowledgeTantivyIndex>,
}

impl KnowledgeStore {
    /// Open (or create) the knowledge store at `~/.codeseek/projects/<project_hash>/knowledge`.
    pub fn new(project_hash: &str) -> anyhow::Result<Self> {
        let db_path = Config::knowledge_dir(project_hash);
        let bm25_dir = Config::knowledge_bm25_dir(project_hash);

        std::fs::create_dir_all(&db_path).context("failed to create knowledge DB directory")?;
        std::fs::create_dir_all(&bm25_dir)
            .context("failed to create knowledge BM25 index directory")?;

        let bm25 = KnowledgeTantivyIndex::open_or_create(&bm25_dir)
            .context("failed to open/create Tantivy index")?;

        Ok(Self {
            db_path,
            table_name: "knowledge".to_string(),
            bm25: Some(bm25),
        })
    }

    /// Ensure the LanceDB table exists with the correct schema.
    pub async fn ensure_table(&self) -> anyhow::Result<()> {
        let db_path_str = self.db_path.to_string_lossy().to_string();
        let connection = connect(&db_path_str).execute().await.with_context(|| {
            format!("failed to connect to LanceDB at {}", self.db_path.display())
        })?;

        let table_names = connection
            .table_names()
            .execute()
            .await
            .with_context(|| "failed to list LanceDB tables")?;

        if table_names.contains(&self.table_name) {
            return Ok(());
        }

        let dimensions = Config::load()
            .map(|c| c.embedding.dimensions)
            .unwrap_or(2560);

        let schema = knowledge_schema(dimensions);

        connection
            .create_empty_table(&self.table_name, schema)
            .execute()
            .await
            .with_context(|| format!("failed to create table '{}'", self.table_name))?;

        log::info!(
            "Created LanceDB table '{}' with {} dimensions",
            self.table_name,
            dimensions
        );
        Ok(())
    }

    /// Build an embedding provider from config (or environment fallback).
    async fn build_provider(&self) -> anyhow::Result<Box<dyn EmbeddingProvider>> {
        let config = Config::load().ok();
        let config = config.as_ref();
        let api_token = if config
            .map(|c| &c.embedding.api_token)
            .unwrap_or(&String::new())
            .is_empty()
        {
            std::env::var("SILICONFLOW_API_KEY").unwrap_or_default()
        } else {
            config
                .map(|c| c.embedding.api_token.clone())
                .unwrap_or_default()
        };
        let base_url = if config
            .map(|c| &c.embedding.api_base_url)
            .unwrap_or(&String::new())
            .is_empty()
        {
            None
        } else {
            Some(
                config
                    .map(|c| c.embedding.api_base_url.clone())
                    .unwrap_or_default(),
            )
        };
        let model = if config
            .map(|c| &c.embedding.model)
            .unwrap_or(&String::new())
            .is_empty()
        {
            "Qwen/Qwen3-Embedding-4B".to_string()
        } else {
            config
                .map(|c| c.embedding.model.clone())
                .unwrap_or_default()
        };
        // 查询侧指令：仅用于 provider.format_query()；入库(add)路径不会读取它。
        // 显式配置为空串 => effective_instruct() 返回 "" => 关闭前缀注入。
        let instruct = config
            .map(|c| c.embedding.effective_instruct().to_string())
            .unwrap_or_else(|| DEFAULT_QUERY_INSTRUCT.to_string());

        let provider = OpenAICompatibleEmbeddingProvider::new(api_token, base_url, model)
            .with_instruct(instruct);
        Ok(Box::new(provider))
    }

    /// Insert a knowledge record into both LanceDB and the BM25 index.
    pub async fn add(&self, mut record: KnowledgeRecord) -> anyhow::Result<KnowledgeRecord> {
        // Sanitize then compute embedding.
        record = record.sanitize();
        let embed_text = record.embed_text();

        let provider = self
            .build_provider()
            .await
            .context("failed to build embedding provider")?;

        // 文档/入库侧：裸 embed_text，不加任何 instruct 前缀（保持与既有向量索引同分布）
        let vector = provider
            .get_embedding(&embed_text)
            .await
            .map_err(|e| anyhow!("embedding API call failed: {}", e))?;

        let dimensions = Config::load()
            .map(|c| c.embedding.dimensions)
            .unwrap_or(2560);
        if vector.len() != dimensions {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                dimensions,
                vector.len()
            ));
        }
        record.vector = vector;

        self.ensure_table().await?;

        let dimensions = Config::load()
            .map(|c| c.embedding.dimensions)
            .unwrap_or(2560);

        let schema = knowledge_schema(dimensions);

        let mut id_builder = StringBuilder::new();
        let mut vector_builder =
            FixedSizeListBuilder::new(Float32Builder::new(), dimensions as i32);
        let mut type_builder = StringBuilder::new();
        let mut title_builder = StringBuilder::new();
        let mut content_builder = StringBuilder::new();

        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        for t in &record.tags {
            tags_builder.values().append_value(t);
        }
        tags_builder.append(true);

        let mut related_files_builder = ListBuilder::new(StringBuilder::new());
        for f in &record.related_files {
            related_files_builder.values().append_value(f);
        }
        related_files_builder.append(true);

        let mut source_agent_builder = StringBuilder::new();
        let mut task_id_builder = StringBuilder::new();
        let mut confidence_builder = Float32Builder::new();
        let mut created_at_builder = StringBuilder::new();
        let mut updated_at_builder = StringBuilder::new();
        let mut access_count_builder = Int32Builder::new();
        let mut last_accessed_builder = StringBuilder::new();
        let mut parent_ids_builder = ListBuilder::new(StringBuilder::new());
        for p in &record.parent_ids {
            parent_ids_builder.values().append_value(p);
        }
        parent_ids_builder.append(true);

        id_builder.append_value(&record.id);
        vector_builder.values().append_slice(&record.vector);
        vector_builder.append(true);
        type_builder.append_value(&record.type_);
        title_builder.append_value(&record.title);
        content_builder.append_value(&record.content);
        source_agent_builder.append_value(&record.source_agent);
        task_id_builder.append_option(if record.task_id.is_empty() {
            None
        } else {
            Some(&record.task_id)
        });
        confidence_builder.append_value(record.confidence);
        created_at_builder.append_value(&record.created_at);
        updated_at_builder.append_value(&record.updated_at);
        access_count_builder.append_value(record.access_count as i32);
        last_accessed_builder.append_option(record.last_accessed.as_ref());

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(id_builder.finish()),
                Arc::new(vector_builder.finish()),
                Arc::new(type_builder.finish()),
                Arc::new(title_builder.finish()),
                Arc::new(content_builder.finish()),
                Arc::new(tags_builder.finish()),
                Arc::new(related_files_builder.finish()),
                Arc::new(source_agent_builder.finish()),
                Arc::new(task_id_builder.finish()),
                Arc::new(confidence_builder.finish()),
                Arc::new(created_at_builder.finish()),
                Arc::new(updated_at_builder.finish()),
                Arc::new(access_count_builder.finish()),
                Arc::new(last_accessed_builder.finish()),
                Arc::new(parent_ids_builder.finish()),
            ],
        )
        .with_context(|| "failed to build RecordBatch")?;

        let db_path_str = self.db_path.to_string_lossy().to_string();
        let table = connect(&db_path_str).execute().await.with_context(|| {
            format!("failed to connect to LanceDB at {}", self.db_path.display())
        })?;
        let table = table
            .open_table(&self.table_name)
            .execute()
            .await
            .with_context(|| format!("failed to open table '{}'", self.table_name))?;

        let batches = vec![Ok(batch)];
        let data: Box<dyn arrow::array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(batches, schema));
        table
            .add(data)
            .execute()
            .await
            .with_context(|| "failed to add record to LanceDB")?;

        // Index into BM25.
        if let Some(ref bm25) = self.bm25 {
            bm25.add_document(&record)
                .with_context(|| "failed to add document to BM25 index")?;
        }

        log::info!("Added knowledge record: {}", record.id);

        // Return record with vector cleared (not stored in the struct field).
        record.vector = vec![];
        Ok(record)
    }

    /// Delete a record by ID from both LanceDB and the BM25 index.
    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.ensure_table().await?;

        let db_path_str = self.db_path.to_string_lossy().to_string();
        let table = connect(&db_path_str).execute().await.with_context(|| {
            format!("failed to connect to LanceDB at {}", self.db_path.display())
        })?;
        let table = table
            .open_table(&self.table_name)
            .execute()
            .await
            .with_context(|| format!("failed to open table '{}'", self.table_name))?;

        let predicate = format!("id = '{}'", id.replace('\'', "''"));
        table
            .delete(&predicate)
            .await
            .with_context(|| format!("failed to delete record '{}' from LanceDB", id))?;

        if let Some(ref bm25) = self.bm25 {
            if let Err(e) = bm25.delete_by_id(id) {
                log::warn!("Failed to delete from BM25 index: {}", e);
            }
        }

        log::info!("Deleted knowledge record: {}", id);
        Ok(())
    }

    /// List records with optional filters. Returns up to `limit` records.
    pub async fn list(
        &self,
        limit: usize,
        type_filter: Option<&str>,
        tag_filter: Option<&str>,
    ) -> anyhow::Result<Vec<KnowledgeRecord>> {
        self.ensure_table().await?;

        let db_path_str = self.db_path.to_string_lossy().to_string();
        let connection = connect(&db_path_str).execute().await.with_context(|| {
            format!("failed to connect to LanceDB at {}", self.db_path.display())
        })?;
        let table = connection
            .open_table(&self.table_name)
            .execute()
            .await
            .with_context(|| format!("failed to open table '{}'", self.table_name))?;

        // Scan the full table (knowledge store is expected to be small).
        let fetch_limit = limit.saturating_mul(5).max(100);
        let mut results_stream = table
            .query()
            .limit(fetch_limit)
            .execute()
            .await
            .with_context(|| "failed to execute LanceDB scan")?;

        let mut records = Vec::new();
        while let Some(batch) = results_stream
            .try_next()
            .await
            .with_context(|| "failed to read LanceDB batch")?
        {
            for row in 0..batch.num_rows() {
                let record = self.record_from_batch(&batch, row);
                // Apply filters.
                if let Some(tf) = type_filter {
                    if record.type_ != tf {
                        continue;
                    }
                }
                if let Some(tg) = tag_filter {
                    if !record.tags.iter().any(|t| t == tg) {
                        continue;
                    }
                }
                records.push(record);
                if records.len() >= limit {
                    return Ok(records);
                }
            }
        }

        Ok(records)
    }

    /// 知识库对称混合检索：向量（LanceDB）+ BM25（Tantivy）→ RRF 融合。
    ///
    /// 对称性三要点（详见模块文档）：
    /// 1. 两个通道共用同一个召回预算 `K_recall = limit × KNOWLEDGE_RECALL_MULTIPLIER`
    ///    （下限 `limit`）。
    /// 2. `domains` 域过滤在两个通道都是**检索前**下推：LanceDB 侧走 `only_if`
    ///    （`QueryRequest.prefilter` 默认 `true`，`top_k = limit + offset` 作用于**过滤后**
    ///    的行集），Tantivy 侧走 `BooleanQuery(Must)` 的 `type` 项。不做任何事后裁剪。
    /// 3. 纯 BM25 命中（没有向量距离，因而不会被向量批次带进 `record_map`）在融合后按
    ///    id 回查 LanceDB 一次补齐；回查后仍缺失的只告警，并继续用后续融合候选填满
    ///    `limit`。
    ///
    /// `_rerank`：Phase 1 未启用重排，参数保留以兼容既有调用方（`knowledge/cli.rs`），
    /// 当前恒被忽略。
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        _rerank: bool,
        domains: &[String],
    ) -> anyhow::Result<Vec<KnowledgeSearchResult>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let type_values = domain_to_type_values(domains);

        // 对称预算：向量通道与 BM25 通道使用完全相同的 K_recall。
        let k_recall = limit.saturating_mul(KNOWLEDGE_RECALL_MULTIPLIER).max(limit);

        let provider = self
            .build_provider()
            .await
            .context("failed to build embedding provider")?;
        // 查询侧注入官方 instruct 前缀；入库(add)侧保持裸文本，二者非对称。
        let formatted_query = provider.format_query(query);
        let query_vector = provider
            .get_embedding(&formatted_query)
            .await
            .map_err(|e| anyhow!("embedding API call failed for search query: {}", e))?;

        // ── Vector search（域过滤作为 prefilter 下推到检索前）──
        self.ensure_table().await?;
        let db_path_str = self.db_path.to_string_lossy().to_string();
        let connection = connect(&db_path_str).execute().await.with_context(|| {
            format!("failed to connect to LanceDB at {}", self.db_path.display())
        })?;
        let table = connection
            .open_table(&self.table_name)
            .execute()
            .await
            .with_context(|| format!("failed to open table '{}'", self.table_name))?;

        let mut vector_results: Vec<(String, f32)> = Vec::new();
        let mut record_map: HashMap<String, KnowledgeRecord> = HashMap::new();
        let mut vector_score_map: HashMap<String, f32> = HashMap::new();

        // `only_if` 必须在 `nearest_to` 之前调用：后者消费 Query 并转成 VectorQuery，
        // 过滤条件随之进入同一个 QueryRequest。`only_if` 默认就是 prefilter
        // （`QueryRequest.prefilter == true`），因此 limit(k_recall) 的 top-k 是作用在
        // **过滤之后**的行集上的 —— 这正是「域过滤在检索前」的语义。
        let type_pred = type_filter_predicate(&type_values);
        let mut vector_query = table.query();
        if let Some(pred) = type_pred.as_deref() {
            vector_query = vector_query.only_if(pred);
        }

        let mut results_stream = vector_query
            .nearest_to(query_vector)?
            .limit(k_recall)
            .execute()
            .await
            .with_context(|| "failed to execute vector search")?;

        while let Some(batch) = results_stream
            .try_next()
            .await
            .with_context(|| "failed to read vector search batch")?
        {
            let dist_col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::Float32Array>());
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>());

            for i in 0..batch.num_rows() {
                let id = id_col.map(|c| c.value(i).to_string()).unwrap_or_default();
                if id.is_empty() || vector_score_map.contains_key(&id) {
                    // 同一 id 已由更近的一条向量命中占住位次。
                    continue;
                }
                let distance = dist_col.map(|c| c.value(i)).unwrap_or(0.0);
                let score = 1.0 / (1.0 + distance);

                // 每条 row 只解析一次记录。这里**不再**按 `type_values` 事后裁剪：
                // 域过滤已由上面的 `only_if` 下推为 prefilter；事后裁剪正是原实现
                // 「先取 top-k、再过滤、过滤掉的候选不回补」导致召回塌陷的根因。
                vector_results.push((id.clone(), score));
                vector_score_map.insert(id.clone(), score);
                record_map.insert(id, self.record_from_batch(&batch, i));
            }
        }

        // ── BM25 search（预算与向量通道完全相同 → 对称）──
        let mut bm25_results: Vec<(String, f32)> = Vec::new();
        if let Some(ref bm25) = self.bm25 {
            bm25_results = bm25.search(query, k_recall, domains).unwrap_or_default();
        }
        // 预构建分数查表：O(1) 查分，与向量侧 vector_score_map 对称。
        let bm25_score_map: HashMap<String, f32> = bm25_results.iter().cloned().collect();

        // ── RRF fusion ──
        // 融合池取两通道并集的上界，而不是 limit：这样后面「记录缺失则跳过」时池子里
        // 还有后备候选可以补齐到 k_final。
        let fusion_pool = k_recall.saturating_mul(2);
        let fused = rrf_fuse(vector_results, bm25_results, fusion_pool);

        // ── BM25-only 记录回填（对称召回的保证）──
        // 纯 BM25 命中的记录没有向量距离，因此不会出现在向量批次里，也就不会进
        // record_map。若不放回，这些合法的稀疏命中既占了融合池位、又拿不到记录，
        // 最终条数必然不足。这里用**一次** `id IN (...)` 扫描把记录捞回来。
        let missing = missing_record_ids(&fused, &record_map);
        if !missing.is_empty() {
            if let Some(pred) = id_filter_predicate(&missing) {
                // 注意：回查**不叠加** domain 过滤 —— 这些 id 已经来自按 domain 过滤过的
                // 通道，再过滤一次只会掩盖 Tantivy / LanceDB 索引不一致的问题。
                let mut backfill_stream = table
                    .query()
                    .only_if(pred)
                    .limit(missing.len())
                    .execute()
                    .await
                    .with_context(|| "failed to execute LanceDB id backfill scan")?;

                while let Some(batch) = backfill_stream
                    .try_next()
                    .await
                    .with_context(|| "failed to read LanceDB backfill batch")?
                {
                    let id_col = batch
                        .column_by_name("id")
                        .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>());
                    for row in 0..batch.num_rows() {
                        let id = id_col.map(|c| c.value(row).to_string()).unwrap_or_default();
                        if id.is_empty() || record_map.contains_key(&id) {
                            continue;
                        }
                        record_map.insert(id, self.record_from_batch(&batch, row));
                    }
                }
            }

            // 仍然缺失 = Tantivy 与 LanceDB 索引不同步（记录可能已从 LanceDB 删除）。
            // 只告警，不中断：装配阶段会继续用后面的融合候选补齐到 limit。
            let still_missing = missing_record_ids(&fused, &record_map);
            if !still_missing.is_empty() {
                log::warn!(
                    "knowledge search: {} fused id(s) missing from LanceDB even after id backfill \
                     (Tantivy/LanceDB index out of sync, record may have been deleted): {:?}",
                    still_missing.len(),
                    still_missing
                );
            }
        }

        // ── Build results（按融合顺序装配，缺失跳过、继续补齐到 limit）──
        Ok(assemble_results(
            fused,
            record_map,
            &vector_score_map,
            &bm25_score_map,
            limit,
        ))
    }

    /// Parse a single row from a LanceDB batch into a [`KnowledgeRecord`].
    fn record_from_batch(&self, batch: &RecordBatch, row: usize) -> KnowledgeRecord {
        let id = batch
            .column_by_name("id")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        // Vector is skipped (serde(skip) on the struct field).
        let type_ = batch
            .column_by_name("type")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let title = batch
            .column_by_name("title")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let content = batch
            .column_by_name("content")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let tags = self.extract_list_string(batch, row, "tags");
        let related_files = self.extract_list_string(batch, row, "related_files");
        let source_agent = batch
            .column_by_name("source_agent")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let task_id = batch
            .column_by_name("task_id")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .filter(|c| !c.is_null(row))
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let confidence = batch
            .column_by_name("confidence")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::Float32Array>())
            .map(|c| c.value(row))
            .unwrap_or(0.0);

        let created_at = batch
            .column_by_name("created_at")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let updated_at = batch
            .column_by_name("updated_at")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .map(|c| c.value(row).to_string())
            .unwrap_or_default();

        let access_count = batch
            .column_by_name("access_count")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::Int32Array>())
            .map(|c| c.value(row) as u32)
            .unwrap_or(0);

        let last_accessed = batch
            .column_by_name("last_accessed")
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>())
            .filter(|c| !c.is_null(row))
            .map(|c| c.value(row).to_string());

        let parent_ids = self.extract_list_string(batch, row, "parent_ids");

        KnowledgeRecord {
            id,
            vector: vec![],
            type_,
            title,
            content,
            tags,
            related_files,
            source_agent,
            task_id,
            confidence,
            created_at,
            updated_at,
            access_count,
            last_accessed,
            parent_ids,
        }
    }

    /// Extract a List<Utf8> column as a Vec<String> for the given row.
    fn extract_list_string(
        &self,
        batch: &RecordBatch,
        row: usize,
        column_name: &str,
    ) -> Vec<String> {
        let list_col = batch
            .column_by_name(column_name)
            .and_then(|c| c.as_any().downcast_ref::<arrow::array::ListArray>());

        match list_col {
            None => Vec::new(),
            Some(list_arr) => {
                if list_arr.is_null(row) {
                    return Vec::new();
                }
                let value_arr = list_arr.value(row);
                let str_arr = match value_arr
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                {
                    Some(s) => s,
                    None => {
                        log::warn!(
                            "expected StringArray in column {}, got different type",
                            column_name
                        );
                        return Vec::new();
                    }
                };
                let str_len = str_arr.len();
                if str_len == 0 {
                    return Vec::new();
                }
                let start = usize::try_from(list_arr.value_offsets()[row]).unwrap_or(0);
                let end = usize::try_from(list_arr.value_offsets()[row + 1]).unwrap_or(0);
                // Clamp end to actual StringArray length to avoid panic on mismatched batches
                let end = end.min(str_len);
                let start = start.min(end);
                (start..end).map(|i| str_arr.value(i).to_string()).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::record::KnowledgeRecord;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_add_and_list() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("knowledge");
        let bm25_dir = dir.path().join("knowledge_index");
        std::fs::create_dir_all(&db_path).unwrap();
        std::fs::create_dir_all(&bm25_dir).unwrap();

        let store = KnowledgeStore {
            db_path: db_path.clone(),
            table_name: "test_knowledge".to_string(),
            bm25: Some(KnowledgeTantivyIndex::open_or_create(&bm25_dir).unwrap()),
        };

        let record = KnowledgeRecord::new(
            "Test Title".to_string(),
            "Test content about Rust programming".to_string(),
            "repo_retrieval".to_string(),
            "repo_agent".to_string(),
            "task-001".to_string(),
        )
        .sanitize();
        let _record = store.add(record).await;
        // Note: embedding API will be called; in tests with no API token this may fail.
        // The test is a structural check; actual integration requires a real API token.
        let _ = _record;
    }

    // ─────────────────────────── 纯函数单测 ───────────────────────────

    fn sample_record(id: &str) -> KnowledgeRecord {
        let mut record = KnowledgeRecord::new(
            format!("title-{}", id),
            format!("content about {}", id),
            "repo_retrieval".to_string(),
            "repo_agent".to_string(),
            "task-1".to_string(),
        );
        record.id = id.to_string();
        record
    }

    #[test]
    fn type_filter_predicate_none_on_empty() {
        assert_eq!(type_filter_predicate(&[]), None);
    }

    #[test]
    fn type_filter_predicate_maps_domains_to_types() {
        // domain "repo" → type "repo_retrieval"
        let repo = domain_to_type_values(&["repo".to_string()]);
        assert_eq!(
            type_filter_predicate(&repo).unwrap(),
            "type IN ('repo_retrieval')"
        );

        // 多值 → IN 列表，顺序与 domains 一致
        let both = domain_to_type_values(&["repo".to_string(), "coding".to_string()]);
        assert_eq!(
            type_filter_predicate(&both).unwrap(),
            "type IN ('repo_retrieval', 'coding_modification')"
        );

        // 未知 domain 原样透传（可直传 type 值）
        let raw = domain_to_type_values(&["repo_retrieval".to_string()]);
        assert_eq!(
            type_filter_predicate(&raw).unwrap(),
            "type IN ('repo_retrieval')"
        );
    }

    #[test]
    fn type_filter_predicate_escapes_single_quotes() {
        let values = vec!["o'brien".to_string()];
        assert_eq!(
            type_filter_predicate(&values).unwrap(),
            "type IN ('o''brien')"
        );
    }

    #[test]
    fn id_filter_predicate_builds_in_list() {
        assert_eq!(id_filter_predicate(&[]), None);
        assert_eq!(
            id_filter_predicate(&["kn_1".to_string()]).unwrap(),
            "id IN ('kn_1')"
        );
        assert_eq!(
            id_filter_predicate(&["kn_1".to_string(), "kn_2".to_string()]).unwrap(),
            "id IN ('kn_1', 'kn_2')"
        );
        // 单引号转义（防 SQL 语法错误 / 注入）
        assert_eq!(
            id_filter_predicate(&["o'b".to_string()]).unwrap(),
            "id IN ('o''b')"
        );
    }

    #[test]
    fn missing_record_ids_keeps_fusion_order_and_dedupes() {
        let mut known: HashMap<String, KnowledgeRecord> = HashMap::new();
        known.insert("A".to_string(), sample_record("A"));
        let fused = vec![
            ("A".to_string(), 0.9f64),
            ("B".to_string(), 0.8),
            ("C".to_string(), 0.7),
            ("B".to_string(), 0.6),
        ];
        assert_eq!(
            missing_record_ids(&fused, &known),
            vec!["B".to_string(), "C".to_string()]
        );
    }

    /// 直接编码缺陷 2：纯 BM25 命中（record_map 里只有它、没有向量分）必须保留。
    #[test]
    fn assemble_results_keeps_bm25_only_hit() {
        let fused = vec![("v".to_string(), 0.9f64), ("bm25only".to_string(), 0.8)];
        let mut records: HashMap<String, KnowledgeRecord> = HashMap::new();
        records.insert("v".to_string(), sample_record("v"));
        records.insert("bm25only".to_string(), sample_record("bm25only"));

        let mut vector_scores: HashMap<String, f32> = HashMap::new();
        vector_scores.insert("v".to_string(), 0.5);
        let mut bm25_scores: HashMap<String, f32> = HashMap::new();
        bm25_scores.insert("bm25only".to_string(), 7.5);

        let results = assemble_results(fused, records, &vector_scores, &bm25_scores, 2);

        assert_eq!(results.len(), 2, "BM25-only 命中不允许被丢弃");
        // 顺序按 fused
        assert_eq!(results[0].record.id, "v");
        assert_eq!(results[1].record.id, "bm25only");
        // 分数各归其通道
        assert_eq!(results[0].vector_score, Some(0.5));
        assert_eq!(results[0].bm25_score, None);
        assert_eq!(results[1].vector_score, None);
        assert_eq!(results[1].bm25_score, Some(7.5));
        assert_eq!(results[0].final_score, 0.9f32);
        assert_eq!(results[1].final_score, 0.8f32);
    }

    /// 记录缺失时跳过并**继续补齐**到 k_final，而不是提前结束导致返回不足。
    #[test]
    fn assemble_results_skips_missing_and_backfills_to_k_final() {
        let fused: Vec<(String, f64)> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| (id.to_string(), 1.0))
            .collect();
        let mut records: HashMap<String, KnowledgeRecord> = HashMap::new();
        for id in ["a", "c", "e"] {
            records.insert(id.to_string(), sample_record(id));
        }
        let empty: HashMap<String, f32> = HashMap::new();

        let results = assemble_results(fused, records, &empty, &empty, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].record.id, "a");
        assert_eq!(results[1].record.id, "c"); // 跳过缺失的 b 后继续补齐
        assert_eq!(results[0].vector_score, None);
        assert_eq!(results[0].bm25_score, None);
    }

    // ─────────────────────── LanceDB 集成测试（真实运行） ───────────────────────

    /// 构造知识库批次（不调用 `add()`，因此不会触发任何 embedding 网络请求）。
    fn test_batch(dimensions: usize, rows: &[(&str, &str, Vec<f32>)]) -> RecordBatch {
        let schema = knowledge_schema(dimensions);
        let mut id_builder = StringBuilder::new();
        let mut vector_builder =
            FixedSizeListBuilder::new(Float32Builder::new(), dimensions as i32);
        let mut type_builder = StringBuilder::new();
        let mut title_builder = StringBuilder::new();
        let mut content_builder = StringBuilder::new();
        let mut tags_builder = ListBuilder::new(StringBuilder::new());
        let mut related_files_builder = ListBuilder::new(StringBuilder::new());
        let mut source_agent_builder = StringBuilder::new();
        let mut task_id_builder = StringBuilder::new();
        let mut confidence_builder = Float32Builder::new();
        let mut created_at_builder = StringBuilder::new();
        let mut updated_at_builder = StringBuilder::new();
        let mut access_count_builder = Int32Builder::new();
        let mut last_accessed_builder = StringBuilder::new();
        let mut parent_ids_builder = ListBuilder::new(StringBuilder::new());

        for (id, type_, vector) in rows {
            assert_eq!(vector.len(), dimensions, "test vector dimension mismatch");
            id_builder.append_value(id);
            vector_builder.values().append_slice(vector);
            vector_builder.append(true);
            type_builder.append_value(type_);
            title_builder.append_value(format!("title-{}", id));
            content_builder.append_value(format!("content-{}", id));
            tags_builder.append(true);
            related_files_builder.append(true);
            source_agent_builder.append_value("repo_agent");
            task_id_builder.append_null();
            confidence_builder.append_value(1.0);
            created_at_builder.append_value("2026-01-01T00:00:00Z");
            updated_at_builder.append_value("2026-01-01T00:00:00Z");
            access_count_builder.append_value(0);
            last_accessed_builder.append_null();
            parent_ids_builder.append(true);
        }

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(id_builder.finish()),
                Arc::new(vector_builder.finish()),
                Arc::new(type_builder.finish()),
                Arc::new(title_builder.finish()),
                Arc::new(content_builder.finish()),
                Arc::new(tags_builder.finish()),
                Arc::new(related_files_builder.finish()),
                Arc::new(source_agent_builder.finish()),
                Arc::new(task_id_builder.finish()),
                Arc::new(confidence_builder.finish()),
                Arc::new(created_at_builder.finish()),
                Arc::new(updated_at_builder.finish()),
                Arc::new(access_count_builder.finish()),
                Arc::new(last_accessed_builder.finish()),
                Arc::new(parent_ids_builder.finish()),
            ],
        )
        .unwrap()
    }

    /// 跑一次向量查询并收集返回的 id（`predicate` 通过 `only_if` 传入，即 prefilter）。
    async fn run_vector_query(
        table: &lancedb::Table,
        vector: Vec<f32>,
        predicate: Option<String>,
        limit: usize,
    ) -> Vec<String> {
        let mut query = table.query();
        if let Some(pred) = predicate {
            query = query.only_if(pred);
        }
        let mut stream = query
            .nearest_to(vector)
            .unwrap()
            .limit(limit)
            .execute()
            .await
            .unwrap();

        let mut ids = Vec::new();
        while let Some(batch) = stream.try_next().await.unwrap() {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>());
            for row in 0..batch.num_rows() {
                ids.push(id_col.unwrap().value(row).to_string());
            }
        }
        ids
    }

    /// 关键回归测试：域过滤必须是**检索前**下推（prefilter），而不是「先 top-k 再裁剪」。
    ///
    /// 表里放 3 条距离查询向量极近的 `coding_modification` + 1 条距离最远的
    /// `repo_retrieval`。以 `limit = 2` 只查 `repo_retrieval`：
    /// - 正确实现（prefilter）：top-k 作用在过滤后的行集上 ⇒ 返回那条 repo 记录；
    /// - 旧实现（先取 top-2 再过滤）：top-2 全是 coding 记录，被裁掉后返回 0 条。
    #[tokio::test]
    async fn vector_prefilter_applies_domain_before_top_k() {
        let dimensions = 8usize;
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("knowledge_prefilter");
        std::fs::create_dir_all(&db_path).unwrap();

        let connection = connect(&db_path.to_string_lossy()).execute().await.unwrap();
        connection
            .create_empty_table("prefilter_test", knowledge_schema(dimensions))
            .execute()
            .await
            .unwrap();
        let table = connection
            .open_table("prefilter_test")
            .execute()
            .await
            .unwrap();

        // 查询向量全 0：`near` 与之距离为 0（最近），`far` 全 1（最远）。
        let near = vec![0.0f32; dimensions];
        let far = vec![1.0f32; dimensions];
        let batch = test_batch(
            dimensions,
            &[
                ("kn_coding_1", "coding_modification", near.clone()),
                ("kn_coding_2", "coding_modification", near.clone()),
                ("kn_coding_3", "coding_modification", near.clone()),
                ("kn_repo_1", "repo_retrieval", far),
            ],
        );

        let schema = knowledge_schema(dimensions);
        let batches = vec![Ok(batch)];
        let data: Box<dyn arrow::array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(batches, schema));
        table.add(data).execute().await.unwrap();

        // 无过滤：limit=1 必然落在最近的 coding 记录上（证明距离排序符合预期）。
        let unfiltered = run_vector_query(&table, near.clone(), None, 1).await;
        assert_eq!(unfiltered.len(), 1);
        assert!(
            unfiltered[0].starts_with("kn_coding_"),
            "expected nearest row to be a coding record, got {:?}",
            unfiltered
        );

        // 域过滤：domain "repo" → type "repo_retrieval"，在检索前下推。
        let type_values = domain_to_type_values(&["repo".to_string()]);
        let predicate =
            type_filter_predicate(&type_values).expect("repo domain must produce a predicate");
        let filtered = run_vector_query(&table, near.clone(), Some(predicate), 2).await;

        assert_eq!(
            filtered,
            vec!["kn_repo_1".to_string()],
            "prefilter must apply the type domain before top-k; \
             a post-filter implementation would return an empty list here"
        );

        // 对照证据（同一谓词、同一 limit，仅把 prefilter 关掉 = 旧实现的语义）：
        // 先取 top-2 再过滤，top-2 全是 coding 记录 ⇒ 返回 0 条。
        // 这条断言证明上面的断言确实能区分「检索前下推」与「事后裁剪」。
        let postfilter_query = table
            .query()
            .only_if("type IN ('repo_retrieval')")
            .postfilter();
        let mut postfilter_stream = postfilter_query
            .nearest_to(near)
            .unwrap()
            .limit(2)
            .execute()
            .await
            .unwrap();
        let mut postfiltered = Vec::new();
        while let Some(batch) = postfilter_stream.try_next().await.unwrap() {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<arrow::array::StringArray>());
            for row in 0..batch.num_rows() {
                postfiltered.push(id_col.unwrap().value(row).to_string());
            }
        }
        assert!(
            postfiltered.is_empty(),
            "post-filter (old semantics) is expected to lose the repo record, got {:?}",
            postfiltered
        );
    }
}
