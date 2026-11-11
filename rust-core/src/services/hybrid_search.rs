//! Hybrid search service combining Dense (Vector) and Sparse (BM25) retrieval channels.
//!
//! Uses Reciprocal Rank Fusion (RRF) to merge results from both channels,
//! providing better recall and precision than either channel alone.

use crate::services::embedding_service::{EmbeddingService, SearchResult};
use crate::services::reranker_service::RerankerService;
use crate::storage::traits_bm25::{CandidateSource, CodeChunk, FusedCandidate, TextSearchProvider, TextSearchResult, parse_snippet_id};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};

/// Configuration for hybrid search behavior.
#[derive(Debug, Clone)]
pub struct HybridSearchConfig {
    /// RRF fusion constant. Higher = ranks matter less.
    /// Industry standard is 60. See: https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf
    pub rrf_k: f64,
    /// Number of dense (vector) results to fetch per fusion query.
    /// Should be >= top_k to ensure enough candidates for fusion.
    pub dense_limit: usize,
    /// Number of sparse (BM25) results to fetch per fusion query.
    /// Should be >= top_k to ensure enough candidates for fusion.
    pub sparse_limit: usize,
    /// Whether to enable the sparse (BM25) channel.
    /// When false, falls back to dense-only search.
    pub enable_sparse: bool,
    /// Timeout for the entire hybrid search operation (milliseconds).
    /// 0 means no timeout.
    pub timeout_ms: u64,
    /// Threshold (in chars) for short code penalty. 
    /// Code blocks shorter than this get their score reduced.
    pub short_code_threshold: usize,
    /// Penalty strength for short code blocks (0.0 = no penalty, 1.0 = full penalty).
    /// Applied as: penalty = 1.0 - max(0, 1 - len/threshold) * strength
    pub short_code_penalty: f64,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            dense_limit: 100,
            sparse_limit: 100,
            enable_sparse: true,
            timeout_ms: 0,
            short_code_threshold: 30,
            short_code_penalty: 0.5,
        }
    }
}

/// The hybrid search service that orchestrates dual-channel retrieval.
///
/// Architecture:
/// ```text
///    Query
///      │
///      ├──→ EmbeddingService.search() ──→ DenseResults
///      │
///      └──→ TextSearchProvider.search() ──→ SparseResults (BM25)
///      │
///      └──→ RRF Fusion ──→ FusedCandidates (sorted by fused score)
///      │
///      └──→ (Optional) Cross-Encoder Reranker ──→ Final Results
/// ```
///
/// 降级方向（两通道对称）：dense 失败 → sparse-only；sparse 禁用/失败 → dense-only；
/// dense 失败且无可用 sparse → 返回 `Err`，由调用方回退到 PetCodeGraph 名称搜索。
///
/// 降级路径一律跳过 RRF；是否执行 Reranker 取决于正文是否可得：sparse-only 降级会先用
/// [`HybridSearchService::hydrate_code_blocks`] 回填正文再精排，dense-only 降级正文自带，
/// 但没有另一侧可比较的分数，因此仍不重排。详见 [`HybridSearchService::search`]。
pub struct HybridSearchService {
    dense: Arc<EmbeddingService>,
    sparse: Arc<dyn TextSearchProvider>,
    config: HybridSearchConfig,
    /// 可选的 Cross-Encoder Reranker 服务（RRF 融合后执行精排）
    reranker: Option<RerankerService>,
}

/// Internal metadata structure used to track line info during RRF fusion.
///
/// Filled from either channel: the dense channel carries the real `code_block` /
/// line range / language, the sparse (BM25) channel carries path, symbol, language
/// and — because Tantivy does not store `line_start` as a field — a `line_start`
/// reverse-parsed out of its `snippet_id` (see [`sparse_line_start`]).
/// Missing pieces are merged from the other channel via [`FusionMeta::merge_missing`].
#[derive(Clone, Default)]
struct FusionMeta {
    file_path: String,
    symbol_name: String,
    language: String,
    code_block: String,
    line_start: usize,
    line_end: usize,
}

impl From<&SearchResult> for FusionMeta {
    fn from(sr: &SearchResult) -> Self {
        Self {
            file_path: sr.file_path.clone(),
            symbol_name: sr.symbol_name.clone(),
            language: sr.language.clone(),
            code_block: sr.code_block.clone(),
            line_start: sr.line_start,
            line_end: sr.line_end,
        }
    }
}

/// Line number of a sparse (BM25) hit.
///
/// The Tantivy schema has no `line_start` field, so the real line is only available
/// inside `snippet_id` (`{file_path}#{line_start}#{symbol_name}`, built by
/// [`CodeChunk::generate_id`]). Prefer the already-populated `line_start` when a
/// provider supplies it, otherwise reverse-parse it from the id; 0 means unknown.
fn sparse_line_start(sr: &TextSearchResult) -> usize {
    if sr.line_start != 0 {
        return sr.line_start;
    }
    parse_snippet_id(&sr.snippet_id)
        .map(|(_, line_start, _)| line_start)
        .unwrap_or(0)
}

/// BM25 通道回填正文时，从 `line_start` 起最多读取的行数。
///
/// BM25 索引不存 `line_end`（Tantivy schema 里没有该字段），无法得知符号真实结束行，
/// 因此取一个足够宽的窗口而不是试图精确截断：多读几行上下文比截断到半句更利于
/// cross-encoder 打分，也比留下空文档强。
const BM25_BACKFILL_MAX_LINES: usize = 60;

/// 从整份文件内容中按 1-based 闭区间 `[line_start, line_end]` 切出代码片段。
///
/// `line_end == 0`（BM25 通道不存结束行）时退化为 `line_start .. line_start + fallback_lines`
/// 的窗口。返回 `(片段, 实际结束行)`；行号不可用（`line_start == 0`）、越界、或片段全是
/// 空白时返回 `None` —— 调用方据此保持候选原样，绝不把半截或空白噪声写进证据。
///
/// 结果保留原始缩进（不做 trim），因为缩进本身也是代码正文的一部分。
fn slice_code_lines(
    content: &str,
    line_start: usize,
    line_end: usize,
    fallback_lines: usize,
) -> Option<(String, usize)> {
    if line_start == 0 {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if line_start > total {
        return None;
    }

    let end = if line_end >= line_start {
        // dense 通道给了真实结束行：沿用它，越界则 clamp 到文件末尾
        line_end.min(total)
    } else {
        // BM25 通道没有结束行：向后取固定窗口
        let window = fallback_lines.max(1);
        line_start.saturating_add(window - 1).min(total)
    };

    let snippet = lines[line_start - 1..end].join("\n");
    if snippet.trim().is_empty() {
        return None;
    }
    Some((snippet, end))
}

impl From<&TextSearchResult> for FusionMeta {
    fn from(sr: &TextSearchResult) -> Self {
        Self {
            file_path: sr.file_path.clone(),
            symbol_name: sr.symbol_name.clone(),
            language: sr.language.clone(),
            // BM25 通道不存储正文（content 非 STORED），保持为空串；
            // 正文由 [`HybridSearchService::hydrate_code_blocks`] 在调用侧从源文件回填
            code_block: String::new(),
            // 行号从 snippet_id 反解得到（Tantivy 未存 line_start 字段）
            line_start: sparse_line_start(sr),
            // BM25 通道不存储结束行
            line_end: 0,
        }
    }
}

impl FusionMeta {
    /// Fill only the fields this side is missing from `other`.
    ///
    /// "Missing" means empty string for text fields and `0` for line numbers,
    /// so real data already present is never overwritten by a placeholder.
    fn merge_missing(&mut self, other: &Self) {
        if self.file_path.is_empty() {
            self.file_path = other.file_path.clone();
        }
        if self.symbol_name.is_empty() {
            self.symbol_name = other.symbol_name.clone();
        }
        if self.language.is_empty() {
            self.language = other.language.clone();
        }
        if self.code_block.is_empty() {
            self.code_block = other.code_block.clone();
        }
        if self.line_start == 0 {
            self.line_start = other.line_start;
        }
        if self.line_end == 0 {
            self.line_end = other.line_end;
        }
    }
}

/// Build the snippet id of a dense result using the very same scheme as the
/// Tantivy index (`CodeChunk::generate_id`), so both ranked lists can be merged
/// by id. Format: `{file_path}#{line_start}#{symbol_name}`.
///
/// 只用于 sparse 通道不可用的降级路径：那里没有 BM25 结果可借行号，
/// 只能按 dense 自带的行号成 id。[`HybridSearchService::fuse`] 会在回填
/// 行号后重算 id，因此不复用此函数。
fn dense_snippet_id(sr: &SearchResult) -> String {
    CodeChunk::generate_id(&sr.file_path, sr.line_start, &sr.symbol_name)
}

impl HybridSearchService {
    /// Create a new hybrid search service without reranker.
    pub fn new(
        dense: Arc<EmbeddingService>,
        sparse: Arc<dyn TextSearchProvider>,
        config: HybridSearchConfig,
    ) -> Self {
        Self { dense, sparse, config, reranker: None }
    }

    /// Create a new hybrid search service with optional reranker.
    pub fn with_reranker(
        dense: Arc<EmbeddingService>,
        sparse: Arc<dyn TextSearchProvider>,
        config: HybridSearchConfig,
        reranker: Option<RerankerService>,
    ) -> Self {
        Self { dense, sparse, config, reranker }
    }

    /// Execute hybrid search: query both channels, fuse results via RRF.
    ///
    /// # Arguments
    /// * `query` - The search query string
    /// * `limit` - Number of final results to return
    ///
    /// # Returns
    /// A vector of `FusedCandidate` sorted by fused RRF score (descending).
    ///
    /// 降级阶梯（两通道对称，任一条通道存活即出结果）：
    /// - dense 失败 + sparse 可用 → sparse-only 候选（`Self::sparse_only_candidates`）
    /// - sparse 禁用或失败 + dense 可用 → dense-only 候选（`Self::dense_only_candidates`）
    /// - dense 失败且 sparse 被禁用 / 也失败 → 返回 `Err`，由调用方回退到
    ///   PetCodeGraph 名称搜索（见 `main.rs` 的 `use_graph_fallback`）
    ///
    /// 降级路径**不执行 RRF**：存活通道的候选缺少可比较的另一侧分数。是否执行
    /// Reranker 则取决于正文是否可得：
    /// - sparse-only 降级：BM25 不存正文，但候选会先经 [`Self::hydrate_code_blocks`]
    ///   从源文件回填 `code_block`；正文补上后「cross-encoder 无文本可打分」这个原本
    ///   跳过精排的理由消失，因此**会**执行 rerank；
    /// - dense-only 降级：正文由 dense 通道自带，但没有另一侧分数可融合，降级语义
    ///   保持为「原样输出、不重排」。
    ///
    /// 融合主路径同样先回填正文再进 reranker：`SparseOnly` 候选（以及缺正文的
    /// `Fused` 候选）在精排输入与最终证据里都必须是带正文的。
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<FusedCandidate>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let dense_limit = self.config.dense_limit.max(limit);
        let sparse_limit = if self.config.enable_sparse {
            self.config.sparse_limit.max(limit)
        } else {
            0
        };

        // Dispatch both channels concurrently: `tokio::join!` polls both futures in
        // the same task, so the sparse (BM25) query no longer waits for the dense
        // (vector) query to finish before it even starts.
        let (dense_res, sparse_res_opt) = if self.config.enable_sparse {
            let (dense_res, sparse_res) = tokio::join!(
                self.dense.search(query, dense_limit),
                self.sparse.search(query, sparse_limit)
            );
            (dense_res, Some(sparse_res))
        } else {
            debug!("Sparse channel disabled in config");
            (self.dense.search(query, dense_limit).await, None)
        };

        // 两通道结果对称处理：任一条通道存活即出结果，只有两条都不可用时才整体失败
        let mut fused: Vec<FusedCandidate> = match (dense_res, sparse_res_opt) {
            (Ok(dense), Some(Ok(sparse))) => {
                debug!("Sparse channel returned {} results", sparse.len());
                // Perform RRF fusion — 返回足够的候选供后续 rerank 使用
                let rrf_limit = self.config.dense_limit.max(self.config.sparse_limit);
                self.reciprocal_rank_fusion(dense, sparse, rrf_limit)
            }
            (Ok(dense), Some(Err(e))) => {
                warn!(
                    "Sparse (BM25) channel failed, falling back to dense-only: {}",
                    e
                );
                // 降级：不施加 RRF / 短代码惩罚，也不进 reranker。
                // dense 候选的正文自带，无需 hydrate_code_blocks，降级语义保持不变。
                return Ok(Self::dense_only_candidates(dense, limit));
            }
            (Ok(dense), None) => {
                // sparse 被禁用：空 sparse 走 fusion，等价于融合后的 dense-only 排序
                let rrf_limit = self.config.dense_limit.max(self.config.sparse_limit);
                self.reciprocal_rank_fusion(dense, Vec::new(), rrf_limit)
            }
            (Err(dense_e), Some(Ok(sparse))) => {
                warn!(
                    "Dense (vector) channel failed, falling back to sparse-only (BM25): {}",
                    dense_e
                );
                // 降级：先用完整 BM25 池构造候选（上限取池长度，避免回填前就被 limit
                // 截断），回填正文后再决定是否精排 —— 正文补上后「cross-encoder 无文本
                // 可打分」这个原本跳过 reranker 的理由不再成立。
                let pool_len = sparse.len();
                let mut candidates = Self::sparse_only_candidates(sparse, pool_len.max(limit));
                Self::hydrate_code_blocks(&mut candidates).await;

                if let Some(ref reranker) = self.reranker {
                    if reranker.config().enabled {
                        return self.apply_rerank(reranker, query, candidates, limit).await;
                    }
                }
                candidates.truncate(limit);
                return Ok(candidates);
            }
            (Err(dense_e), Some(Err(sparse_e))) => {
                // 两条通道都不可用 → 聚合 Err，由调用方回退到 PetCodeGraph 名称搜索
                return Err(anyhow!(
                    "Hybrid search failed: dense (vector) channel: {}; sparse (BM25) channel: {}",
                    dense_e,
                    sparse_e
                ));
            }
            (Err(e), None) => {
                // 唯一通道（dense）失败且无备用通道 → 只能整体失败
                return Err(anyhow!(
                    "Dense (vector) search failed and sparse channel is disabled: {}",
                    e
                ));
            }
        };

        debug!("Hybrid search returned {} fused candidates (before rerank)", fused.len());

        // BM25 通道的 content 非 STORED，`SparseOnly` 候选天然没有正文：进 reranker 前
        // 必须先在磁盘上补齐，否则 cross-encoder 收到空 document、最终证据也是空的
        Self::hydrate_code_blocks(&mut fused).await;

        // Phase 2: Optional Cross-Encoder Rerank
        if let Some(ref reranker) = self.reranker {
            if reranker.config().enabled {
                return self.apply_rerank(reranker, query, fused, limit).await;
            }
        }

        // 无 rerank 时截断到 limit
        let mut result = fused;
        result.truncate(limit);
        Ok(result)
    }

    /// 为缺少正文的候选从源文件回填代码片段。
    ///
    /// BM25 通道的 content 字段非 STORED，`SparseOnly` 候选天然没有正文；不补的话
    /// cross-encoder 收到空 document、最终证据也是空的。同一 `file_path` 只读一次；
    /// 读取失败/行号不可用时保持原样并 warn，绝不影响整体搜索。
    ///
    /// 只处理「正文为空 + 行号可用 + 路径非空」的候选（dense 自带的正文是权威数据，
    /// 绝不覆盖）。不返回 `Result`：回填是尽力而为的增强，失败退化为修复前的行为。
    async fn hydrate_code_blocks(candidates: &mut [FusedCandidate]) {
        // 按文件归组待回填候选的下标：一个文件可能命中多个符号，没必要重复读盘
        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, c) in candidates.iter().enumerate() {
            if c.code_block.trim().is_empty() && c.line_start > 0 && !c.file_path.is_empty() {
                by_file.entry(c.file_path.clone()).or_default().push(idx);
            }
        }
        if by_file.is_empty() {
            return;
        }

        let mut hydrated = 0usize;
        let mut failed_files = 0usize;

        for (path, indices) in by_file {
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(content) => content,
                Err(e) => {
                    // 索引可能比工作区旧（文件已删/已改路径），只降级不失败
                    warn!("BM25 正文回填跳过 {}：{}", path, e);
                    failed_files += 1;
                    continue;
                }
            };

            for idx in indices {
                let (line_start, line_end) = {
                    let c = &candidates[idx];
                    (c.line_start, c.line_end)
                };
                match slice_code_lines(&content, line_start, line_end, BM25_BACKFILL_MAX_LINES) {
                    Some((snippet, real_end)) => {
                        let c = &mut candidates[idx];
                        c.code_block = snippet;
                        // BM25 通道原本没有结束行（0），回填后写入真实结束行
                        if c.line_end == 0 {
                            c.line_end = real_end;
                        }
                        hydrated += 1;
                    }
                    None => {
                        debug!(
                            "BM25 正文回填无有效片段：{}:{}（行号越界或该区间全为空白）",
                            path, line_start
                        );
                    }
                }
            }
        }

        debug!(
            "BM25 正文回填完成：成功 {} 条，读取失败文件 {} 个",
            hydrated, failed_files
        );
    }

    /// 降级：仅 dense 通道可用时，把向量命中原样转成候选（不施加 RRF / 短代码惩罚）。
    ///
    /// 用于 sparse (BM25) 通道禁用之外的失败场景。顺序沿用 dense 通道自身的相似度
    /// 排序，`final_score` 直接取向量分数，因此**不进 reranker**（另一侧分数缺失，
    /// 融合与精排都没有可比对象）。纯函数，不依赖 `self`，便于单测。
    fn dense_only_candidates(dense: Vec<SearchResult>, limit: usize) -> Vec<FusedCandidate> {
        dense
            .into_iter()
            .map(|sr| FusedCandidate {
                snippet_id: dense_snippet_id(&sr),
                final_score: sr.score as f64,
                file_path: sr.file_path,
                symbol_name: sr.symbol_name,
                language: sr.language,
                line_start: sr.line_start,
                line_end: sr.line_end,
                code_block: sr.code_block,
                source: CandidateSource::DenseOnly,
            })
            .take(limit)
            .collect()
    }

    /// 降级：仅 sparse (BM25) 通道可用时，把 BM25 命中原样转成候选。
    ///
    /// BM25 不存储正文（schema 里 content 非 STORED），故 `code_block` 为空、
    /// `line_end` 为 0；行号优先取 provider 已填充的 `line_start`，否则从
    /// `snippet_id` 反解（见 [`sparse_line_start`]）。顺序沿用 BM25 相关性排序，
    /// `final_score` 直接取 BM25 分数。
    ///
    /// 空 `code_block` 与 `line_end == 0` 只是**构造时**的状态：正文由调用侧的
    /// [`HybridSearchService::hydrate_code_blocks`] 从源文件回填，并顺带补上真实的
    /// `line_end`。因此本函数保持纯函数、不依赖 `self`，便于单测。
    fn sparse_only_candidates(sparse: Vec<TextSearchResult>, limit: usize) -> Vec<FusedCandidate> {
        sparse
            .into_iter()
            .map(|sr| {
                // 行号必须在任何字段被 move 之前求出（snippet_id 里带着真实行号）
                let line_start = sparse_line_start(&sr);
                FusedCandidate {
                    snippet_id: sr.snippet_id.clone(),
                    final_score: sr.score as f64,
                    file_path: sr.file_path,
                    symbol_name: sr.symbol_name,
                    language: sr.language,
                    line_start,
                    line_end: 0,
                    code_block: String::new(),
                    source: CandidateSource::SparseOnly,
                }
            })
            .take(limit)
            .collect()
    }

    /// Reciprocal Rank Fusion (RRF) for combining ranked lists.
    ///
    /// Formula: score(d) = Σ 1 / (k + rank(d))
    /// where k is the RRF constant (default 60), rank(d) is the 0-based position of document d
    /// in a ranked list, and the sum is over all channels that returned d.
    ///
    /// Documents appearing in both channels get a higher fused score.
    ///
    /// 两个通道的 id 统一由 [`CodeChunk::generate_id`] 生成（三段式
    /// `{file_path}#{line_start}#{symbol_name}`），因此「只被 BM25 命中」的片段
    /// 也会作为 `CandidateSource::SparseOnly` 候选保留，不再被静默丢弃。
    fn reciprocal_rank_fusion(
        &self,
        dense: Vec<SearchResult>,
        sparse: Vec<TextSearchResult>,
        limit: usize,
    ) -> Vec<FusedCandidate> {
        Self::fuse(dense, sparse, &self.config, limit)
    }

    /// Pure RRF fusion core.
    ///
    /// 只依赖入参与 `config`（不触碰 `self.dense` / `self.sparse`），因此无需构造
    /// EmbeddingService 或 Tantivy 索引即可单测。
    fn fuse(
        dense: Vec<SearchResult>,
        sparse: Vec<TextSearchResult>,
        config: &HybridSearchConfig,
        limit: usize,
    ) -> Vec<FusedCandidate> {
        let k = config.rrf_k;

        // Accumulate scores per snippet_id, along with metadata from whichever
        // channel(s) produced it. ids 记录在 set 里，供后面判定候选来源。
        let mut score_map: HashMap<String, f64> = HashMap::new();
        let mut meta_map: HashMap<String, FusionMeta> = HashMap::new();
        let mut dense_ids: HashSet<String> = HashSet::new();
        let mut sparse_ids: HashSet<String> = HashSet::new();

        // Process sparse channel FIRST — 纯 BM25 命中本身就是合法候选，并且只有它的
        // snippet_id 携带真实行号（Tantivy schema 未存 line_start 字段）。遍历时顺带
        // 建立 (file_path, symbol_name) → line_start 映射，供行号缺失的 dense 结果回填。
        let mut sparse_line_by_symbol: HashMap<(String, String), usize> = HashMap::new();
        for (rank, sr) in sparse.iter().enumerate() {
            let rrf_score = 1.0 / (k + rank as f64 + 1.0);
            *score_map.entry(sr.snippet_id.clone()).or_insert(0.0) += rrf_score;

            let sparse_meta = FusionMeta::from(sr);
            let meta = meta_map
                .entry(sr.snippet_id.clone())
                .or_insert_with(|| sparse_meta.clone());
            meta.merge_missing(&sparse_meta);

            // 只记录非 0 行号，同 (file, symbol) 取第一次出现的值
            if sparse_meta.line_start > 0 {
                sparse_line_by_symbol
                    .entry((sparse_meta.file_path.clone(), sparse_meta.symbol_name.clone()))
                    .or_insert(sparse_meta.line_start);
            }

            sparse_ids.insert(sr.snippet_id.clone());
        }

        // Process dense channel — 旧 LanceDB 表缺 line_start 列时行号为 0，先按
        // (file, symbol) 从 sparse 回填，再用回填后的行号重算 snippet_id。否则
        // dense 侧的 `{file}#0#{sym}` 与 BM25 侧真实 id 永不相等，RRF 既不能合并
        // 分数，也会多出一条行号为 0 的重复候选。
        for (rank, dr) in dense.iter().enumerate() {
            let mut dense_meta = FusionMeta::from(dr);
            if dense_meta.line_start == 0 {
                if let Some(line) = sparse_line_by_symbol
                    .get(&(dense_meta.file_path.clone(), dense_meta.symbol_name.clone()))
                {
                    dense_meta.line_start = *line;
                }
            }
            let snippet_id =
                CodeChunk::generate_id(&dense_meta.file_path, dense_meta.line_start, &dense_meta.symbol_name);

            let rrf_score = 1.0 / (k + rank as f64 + 1.0);
            *score_map.entry(snippet_id.clone()).or_insert(0.0) += rrf_score;

            // sparse meta 已是基底（携带真实行号），merge_missing 只补它缺的
            // code_block / line_end / language，不会用 0 覆盖真实行号
            let meta = meta_map
                .entry(snippet_id.clone())
                .or_insert_with(|| dense_meta.clone());
            meta.merge_missing(&dense_meta);

            dense_ids.insert(snippet_id);
        }

        // Sort by fused score descending
        let mut ranked: Vec<(String, f64)> = score_map.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top-K and build FusedCandidate structs
        ranked
            .into_iter()
            .take(limit)
            .filter_map(|(snippet_id, fused_score)| {
                // Determine source from the ids recorded while walking each channel
                let in_dense = dense_ids.contains(&snippet_id);
                let in_sparse = sparse_ids.contains(&snippet_id);
                let source = match (in_dense, in_sparse) {
                    (true, true) => CandidateSource::Fused,
                    (true, false) => CandidateSource::DenseOnly,
                    (false, true) => CandidateSource::SparseOnly,
                    (false, false) => return None,
                };

                // Both channels insert metadata, so every candidate has one here
                let meta = meta_map.get(&snippet_id)?;

                // P1: Apply short code penalty — 仅在真的有正文时计算，
                // 否则纯 BM25 候选（content 未存储）会被当成 0 长度代码而腰斩分数
                let final_score = if meta.code_block.trim().is_empty() {
                    fused_score
                } else {
                    let code_len = meta.code_block.trim().chars().count();
                    if code_len < config.short_code_threshold {
                        let ratio = code_len as f64 / config.short_code_threshold as f64;
                        let penalty_factor = 1.0 - (1.0 - ratio) * config.short_code_penalty;
                        let penalty_factor = penalty_factor.max(0.0); // clamp to [0, 1]
                        fused_score * penalty_factor
                    } else {
                        fused_score
                    }
                };
                
                Some(FusedCandidate {
                    snippet_id,
                    final_score,
                    file_path: meta.file_path.clone(),
                    symbol_name: meta.symbol_name.clone(),
                    language: meta.language.clone(),
                    line_start: meta.line_start,
                    line_end: meta.line_end,
                    code_block: meta.code_block.clone(),
                    source,
                })
            })
            .collect()
    }

    /// Get the current config.
    pub fn config(&self) -> &HybridSearchConfig {
        &self.config
    }

    /// 对 RRF 融合结果执行 Cross-Encoder 重排
    async fn apply_rerank(
        &self,
        reranker: &RerankerService,
        query: &str,
        fused: Vec<FusedCandidate>,
        limit: usize,
    ) -> Result<Vec<FusedCandidate>> {
        // 取 RRF 候选池（limit * candidate_multiplier），但不超过 fused 总长度
        let pool_size = (limit * reranker.config().candidate_multiplier).min(fused.len());
        let (candidate_pool, remaining) = fused.split_at(pool_size);

        debug!("Reranker: sending {} candidates (limit={}, multiplier={})", 
            pool_size, limit, reranker.config().candidate_multiplier);

        match reranker.rerank(query, candidate_pool.to_vec()).await {
            Ok(mut reranked) => {
                // 重排结果只取 top limit
                reranked.truncate(limit);
                
                // 如果重排结果不足 limit，用剩余的 RRF 候选填充
                if reranked.len() < limit {
                    let need = limit - reranked.len();
                    reranked.extend(
                        remaining.iter()
                            .take(need)
                            .cloned()
                    );
                }
                
                debug!("Reranker: final {} results after rerank", reranked.len());
                Ok(reranked)
            }
            Err(e) => {
                // 降级：返回 RRF 结果
                warn!("Reranker failed, falling back to RRF results: {}", e);
                let mut fallback = fused;
                fallback.truncate(limit);
                Ok(fallback)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 60+ chars — 超过 default 的 short_code_threshold(30)，避免惩罚干扰断言
    const CODE_BLOCK: &str = "fn foo() {\n    let mut v = Vec::new();\n    v.push(1);\n    v\n}\n";

    fn make_dense(file_path: &str, symbol: &str, line_start: usize, line_end: usize) -> SearchResult {
        SearchResult {
            file_path: file_path.to_string(),
            symbol_name: symbol.to_string(),
            code_block: CODE_BLOCK.to_string(),
            score: 0.9,
            symbol_type: "function".to_string(),
            language: "rust".to_string(),
            line_start,
            line_end,
        }
    }

    fn make_sparse(snippet_id: &str, file_path: &str, symbol: &str, line_start: usize) -> TextSearchResult {
        TextSearchResult {
            snippet_id: snippet_id.to_string(),
            score: 3.5,
            file_path: file_path.to_string(),
            symbol_name: symbol.to_string(),
            language: "rust".to_string(),
            line_start,
        }
    }

    #[test]
    fn sparse_only_candidate_survives_fusion() {
        let id = CodeChunk::generate_id("/tmp/a.rs", 10, "foo");
        let sparse = vec![make_sparse(&id, "/tmp/a.rs", "foo", 0)];
        let config = HybridSearchConfig::default();

        let fused = HybridSearchService::fuse(Vec::new(), sparse, &config, 10);

        assert_eq!(fused.len(), 1, "纯 BM25 命中不应被丢弃");
        let c = &fused[0];
        assert_eq!(c.snippet_id, id);
        assert_eq!(c.source, CandidateSource::SparseOnly);
        assert_eq!(c.file_path, "/tmp/a.rs");
        assert_eq!(c.symbol_name, "foo");
        assert_eq!(c.language, "rust");
        assert_eq!(c.line_start, 10, "行号应从 snippet_id 反解得到，不再是 0");
        assert_eq!(c.line_end, 0);
        assert!(c.code_block.is_empty());
        // 没有正文 ⇒ 不施加短代码惩罚，分数就是未惩罚的 1/(k+rank+1)
        let expected = 1.0 / (config.rrf_k + 0.0 + 1.0);
        assert!((c.final_score - expected).abs() < 1e-9);
    }

    #[test]
    fn sparse_line_start_recovered_from_snippet_id() {
        let id = CodeChunk::generate_id("/tmp/a.rs", 10, "foo");
        let sparse = make_sparse(&id, "/tmp/a.rs", "foo", 0);

        // FusionMeta 侧：行号来自 snippet_id 反解
        let meta = FusionMeta::from(&sparse);
        assert_eq!(meta.line_start, 10, "BM25 结果的行号应从 snippet_id 反解");

        // fuse 侧：即使 dense 通道为空也应有真实行号
        let config = HybridSearchConfig::default();
        let fused = HybridSearchService::fuse(Vec::new(), vec![sparse], &config, 10);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].line_start, 10);
        assert_eq!(fused[0].snippet_id, id);

        // provider 已给出非 0 行号时以它为准，不被 id 覆盖
        let with_line = make_sparse(&id, "/tmp/a.rs", "foo", 3);
        assert_eq!(sparse_line_start(&with_line), 3);
    }

    #[test]
    fn dense_missing_line_start_aligns_with_sparse() {
        // 模拟旧 LanceDB 表缺 line_start 列：dense 完全没有行号信息
        let dense = vec![make_dense("/tmp/a.rs", "foo", 0, 0)];
        let id = CodeChunk::generate_id("/tmp/a.rs", 10, "foo");
        // BM25 侧 line_start 字段为 0，但 snippet_id 里带真实行号
        let sparse = vec![make_sparse(&id, "/tmp/a.rs", "foo", 0)];
        let config = HybridSearchConfig::default();

        let fused = HybridSearchService::fuse(dense, sparse, &config, 10);

        assert_eq!(fused.len(), 1, "回填行号并重算 id 后，两侧必须合并为一条候选");
        let c = &fused[0];
        assert_eq!(c.snippet_id, id);
        assert_eq!(c.source, CandidateSource::Fused);
        assert_eq!(c.line_start, 10, "dense 缺失的行号应从 sparse 回填");
        assert_eq!(c.line_end, 0);
        let expected = 1.0 / (config.rrf_k + 1.0) + 1.0 / (config.rrf_k + 1.0);
        assert!((c.final_score - expected).abs() < 1e-9, "两通道 RRF 分数应累加");
        assert!(!c.code_block.is_empty(), "正文应由 dense 通道补齐");
    }

    #[test]
    fn fused_ids_align_and_scores_sum() {
        let dense = vec![make_dense("/tmp/a.rs", "foo", 10, 20)];
        let id = CodeChunk::generate_id("/tmp/a.rs", 10, "foo");
        let sparse = vec![make_sparse(&id, "/tmp/a.rs", "foo", 10)];
        let config = HybridSearchConfig::default();

        let fused = HybridSearchService::fuse(dense, sparse, &config, 10);

        assert_eq!(fused.len(), 1, "两个通道的 id 必须对齐为同一条候选");
        let c = &fused[0];
        assert_eq!(c.snippet_id, id);
        assert_eq!(c.source, CandidateSource::Fused);
        let expected = 1.0 / (config.rrf_k + 1.0) + 1.0 / (config.rrf_k + 1.0);
        assert!((c.final_score - expected).abs() < 1e-9);
        assert_eq!(c.line_start, 10);
        assert_eq!(c.line_end, 20);
        assert_eq!(c.language, "rust");
        assert!(!c.code_block.is_empty());
    }

    #[test]
    fn dense_only_candidate_keeps_real_line_info() {
        let dense = vec![make_dense("/tmp/a.rs", "foo", 5, 9)];
        let config = HybridSearchConfig::default();

        let fused = HybridSearchService::fuse(dense, Vec::new(), &config, 10);

        assert_eq!(fused.len(), 1);
        let c = &fused[0];
        assert_eq!(c.source, CandidateSource::DenseOnly);
        assert_eq!(c.snippet_id, CodeChunk::generate_id("/tmp/a.rs", 5, "foo"));
        assert_eq!(c.line_start, 5);
        assert_eq!(c.line_end, 9);
        assert_eq!(c.language, "rust");
    }

    #[test]
    fn sparse_only_candidates_degrade_survives_dense_failure() {
        // dense 通道失败时的降级路径：BM25 命中必须原样成为候选，而不是被整体 Err 丢掉
        let id = CodeChunk::generate_id("/tmp/a.rs", 10, "foo");
        let sparse = make_sparse(&id, "/tmp/a.rs", "foo", 0);
        let expected_score = sparse.score;

        let candidates = HybridSearchService::sparse_only_candidates(vec![sparse], 10);

        assert_eq!(candidates.len(), 1);
        let c = &candidates[0];
        assert_eq!(c.source, CandidateSource::SparseOnly);
        assert_eq!(c.snippet_id, id);
        assert_eq!(c.file_path, "/tmp/a.rs");
        assert_eq!(c.symbol_name, "foo");
        assert_eq!(c.language, "rust");
        assert_eq!(c.line_start, 10, "行号应从 snippet_id 反解得到");
        assert_eq!(c.line_end, 0, "BM25 通道没有结束行");
        assert!(c.code_block.is_empty(), "BM25 通道不存储正文");
        assert_eq!(c.final_score, expected_score as f64, "降级不施加 RRF，分数原样保留");
    }

    #[test]
    fn dense_only_candidates_truncates_to_limit() {
        // sparse 通道失败时的降级路径：保持 dense 顺序并按 limit 截断
        let dense = vec![
            make_dense("/tmp/a.rs", "foo", 1, 9),
            make_dense("/tmp/b.rs", "bar", 11, 19),
            make_dense("/tmp/c.rs", "baz", 21, 29),
        ];

        let candidates = HybridSearchService::dense_only_candidates(dense, 2);

        assert_eq!(candidates.len(), 2, "降级路径必须按 limit 截断");
        assert_eq!(candidates[0].snippet_id, CodeChunk::generate_id("/tmp/a.rs", 1, "foo"));
        assert_eq!(candidates[1].snippet_id, CodeChunk::generate_id("/tmp/b.rs", 11, "bar"));
        for c in &candidates {
            assert_eq!(c.source, CandidateSource::DenseOnly);
            assert!(!c.code_block.is_empty(), "正文应由 dense 通道自带");
            assert_eq!(c.final_score, 0.9_f32 as f64, "不做融合，取向量分数");
        }
    }

    #[test]
    fn sparse_only_candidates_preserves_input_rank_order() {
        // 降级路径不重排：BM25 通道自身已按相关性排序，输出顺序必须一致
        let high = {
            let mut s = make_sparse(&CodeChunk::generate_id("/tmp/a.rs", 1, "high"), "/tmp/a.rs", "high", 1);
            s.score = 8.0;
            s
        };
        let low = {
            let mut s = make_sparse(&CodeChunk::generate_id("/tmp/b.rs", 5, "low"), "/tmp/b.rs", "low", 5);
            s.score = 2.0;
            s
        };

        let candidates = HybridSearchService::sparse_only_candidates(vec![high, low], 10);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].symbol_name, "high");
        assert_eq!(candidates[1].symbol_name, "low");
        assert_eq!(candidates[0].final_score, 8.0);
        assert_eq!(candidates[1].final_score, 2.0);
        // provider 已给出非 0 行号时以它为准，不从 id 反解覆盖
        assert_eq!(candidates[1].line_start, 5);
    }

    // ---- slice_code_lines ------------------------------------------------

    #[test]
    fn slice_code_lines_respects_explicit_range() {
        let content = "line1\nline2\nline3\nline4";

        let (snippet, end) = slice_code_lines(content, 2, 3, 60).expect("1-based 闭区间应可切分");

        assert_eq!(snippet, "line2\nline3");
        assert_eq!(end, 3, "返回的结束行必须是真实的结束行");
    }

    #[test]
    fn slice_code_lines_uses_window_when_line_end_unknown() {
        let content = "l1\nl2\nl3\nl4\nl5";

        // BM25 通道没有结束行（line_end == 0）：按 fallback 窗口向后取
        let (snippet, end) = slice_code_lines(content, 2, 0, 3).expect("窗口切分应成功");
        assert_eq!(snippet, "l2\nl3\nl4");
        assert_eq!(end, 4, "结束行应是窗口末行，而不是留在 0");

        // 窗口超出文件末尾时必须 clamp，不能 panic
        let (snippet, end) = slice_code_lines(content, 4, 0, 60).expect("越界窗口应被 clamp");
        assert_eq!(snippet, "l4\nl5");
        assert_eq!(end, 5);
    }

    #[test]
    fn slice_code_lines_rejects_unusable_line_numbers() {
        let content = "a\nb";

        assert!(
            slice_code_lines(content, 3, 0, 60).is_none(),
            "line_start 越界应返回 None"
        );
        assert!(
            slice_code_lines(content, 0, 2, 60).is_none(),
            "line_start == 0 表示行号未知"
        );
        assert!(
            slice_code_lines("", 1, 0, 60).is_none(),
            "空文件没有可切的内容"
        );
    }

    #[test]
    fn slice_code_lines_rejects_blank_snippet() {
        let content = "code\n   \n\t\nmore";

        assert!(
            slice_code_lines(content, 2, 3, 60).is_none(),
            "全空白的片段不应写进证据"
        );
    }

    #[test]
    fn slice_code_lines_keeps_original_indentation() {
        let content = "fn main() {\n    let x = 1;\n}";

        let (snippet, _) = slice_code_lines(content, 2, 0, 1).expect("单行窗口应可切分");

        assert_eq!(snippet, "    let x = 1;", "缩进属于正文，不能被 trim 掉");
    }

    // ---- hydrate_code_blocks ---------------------------------------------

    /// 在临时目录里写一个源文件，返回 `(必须保活的 TempDir, 路径字符串)`。
    fn write_temp_source(file_name: &str, content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join(file_name);
        std::fs::write(&path, content).expect("写入临时源文件失败");
        (dir, path.to_string_lossy().into_owned())
    }

    fn make_candidate(
        file_path: &str,
        line_start: usize,
        line_end: usize,
        code_block: &str,
    ) -> FusedCandidate {
        FusedCandidate {
            snippet_id: CodeChunk::generate_id(file_path, line_start, "sym"),
            final_score: 0.5,
            file_path: file_path.to_string(),
            symbol_name: "sym".to_string(),
            language: "rust".to_string(),
            line_start,
            line_end,
            code_block: code_block.to_string(),
            source: CandidateSource::SparseOnly,
        }
    }

    #[tokio::test]
    async fn hydrate_code_blocks_fills_missing_body_from_disk() {
        let source = "fn a() {}\nfn b() {\n    let v = 1;\n    v\n}\n";
        let (_dir, path) = write_temp_source("a.rs", source);

        let mut candidates = vec![make_candidate(&path, 2, 0, "")];
        HybridSearchService::hydrate_code_blocks(&mut candidates).await;

        let c = &candidates[0];
        assert!(
            c.code_block.contains("fn b()"),
            "正文应从源文件回填，实际：{:?}",
            c.code_block
        );
        assert!(c.code_block.contains("let v = 1;"));
        assert!(c.line_end > 0, "回填时顺带补上真实结束行，不应还是 0");
    }

    #[tokio::test]
    async fn hydrate_code_blocks_never_overwrites_existing_body() {
        let source = "fn a() {}\n";
        let (_dir, path) = write_temp_source("a.rs", source);

        let original = "dense 通道自带的正文，必须原样保留";
        let mut candidates = vec![make_candidate(&path, 1, 1, original)];
        HybridSearchService::hydrate_code_blocks(&mut candidates).await;

        assert_eq!(
            candidates[0].code_block, original,
            "已有正文是权威数据，不得覆盖"
        );
        assert_eq!(candidates[0].line_end, 1, "已有的结束行也不应被改写");
    }

    #[tokio::test]
    async fn hydrate_code_blocks_survives_missing_file() {
        let mut candidates = vec![make_candidate("/nonexistent/codeseek/ghost.rs", 1, 0, "")];

        // 关键断言是「不 panic 也不返回错误」：这里的调用能正常返回即通过
        HybridSearchService::hydrate_code_blocks(&mut candidates).await;

        assert!(candidates[0].code_block.is_empty(), "读不到文件时保持原样");
        assert_eq!(candidates[0].line_end, 0);
    }

    #[tokio::test]
    async fn hydrate_code_blocks_slices_multiple_candidates_of_same_file() {
        let source = "fn one() {}\nfn two() {}\nfn three() {}\n";
        let (_dir, path) = write_temp_source("b.rs", source);

        // 同一文件的 2 条候选：归组后只读一次盘，但每条都要切到自己的片段
        let mut candidates = vec![
            make_candidate(&path, 1, 0, ""),
            make_candidate(&path, 3, 0, ""),
        ];
        HybridSearchService::hydrate_code_blocks(&mut candidates).await;

        assert!(candidates[0].code_block.contains("fn one()"));
        assert!(candidates[1].code_block.contains("fn three()"));
        assert!(candidates[0].line_end > 0 && candidates[1].line_end > 0);
    }

    #[tokio::test]
    async fn sparse_only_fusion_result_gets_body_after_hydration() {
        // 端到端复现原 bug：fuse 产出 SparseOnly 空正文 → hydrate → 正文非空
        let source = "pub fn target() {\n    do_something();\n}\n";
        let (_dir, path) = write_temp_source("e2e.rs", source);

        let config = HybridSearchConfig::default();
        let sparse = vec![make_sparse(
            &CodeChunk::generate_id(&path, 1, "target"),
            &path,
            "target",
            0,
        )];
        let mut fused = HybridSearchService::fuse(Vec::new(), sparse, &config, 10);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].source, CandidateSource::SparseOnly);
        assert!(
            fused[0].code_block.is_empty(),
            "BM25 通道构造时候选本就没有正文"
        );

        HybridSearchService::hydrate_code_blocks(&mut fused).await;

        assert!(
            !fused[0].code_block.trim().is_empty(),
            "回填后最终证据不能是空正文"
        );
        assert!(fused[0].code_block.contains("do_something()"));
        assert!(fused[0].line_end > 0);
    }
}
