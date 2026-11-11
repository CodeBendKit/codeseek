//! Hybrid search for knowledge entries: vector (LanceDB) + BM25 (Tantivy) + RRF fusion.
//!
//! 本模块只提供**通道无关**的融合原语（[`rrf_fuse`] / [`build_result`]）+ 结果结构体，
//! 真正的双通道编排在 [`crate::knowledge::store::KnowledgeStore::search`]：
//!
//! - 两个通道（向量 / BM25）必须拿到**相同**的召回预算 `K_recall`，否则一方候选更多，
//!   在 RRF 里天然占优，融合就是非对称的；
//! - 传给 [`rrf_fuse`] 的 `limit` 应大于最终要返回的条数（知识库用 `K_recall × 2` 作为
//!   融合池上界），这样「某个 id 在存储侧已缺失」时装配阶段还能用后续候选补齐。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::knowledge::record::KnowledgeRecord;

/// A single search result combining scores from multiple retrieval channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchResult {
    #[serde(flatten)]
    pub record: KnowledgeRecord,
    /// Vector search score: 1 / (1 + distance)
    pub vector_score: Option<f32>,
    /// BM25 score from Tantivy
    pub bm25_score: Option<f32>,
    /// Final fused score (higher = more relevant)
    pub final_score: f32,
    /// Reranker score (if reranking was applied)
    pub rerank_score: Option<f32>,
}

/// Reciprocal Rank Fusion (RRF) constant.
const RRF_K: f64 = 60.0;

/// RRF fusion of two ranked lists.
///
/// Both `vector_results` and `bm25_results` are expected to be sorted
/// by relevance (best first). Each entry is identified by its record ID.
///
/// `limit` 是**融合池**上界，不一定是最终返回条数：调用方应传一个大于 `K_final` 的
/// 池大小（知识库用 `K_recall × 2`），这样某个 id 在存储侧缺失、装配阶段被跳过时，
/// 仍然有后备候选可以补齐到 `K_final`。
pub fn rrf_fuse(
    vector_results: Vec<(String, f32)>,
    bm25_results: Vec<(String, f32)>,
    limit: usize,
) -> Vec<(String, f64)> {
    let mut score_map: HashMap<String, f64> = HashMap::new();

    for (rank, (id, _)) in vector_results.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + rank as f64 + 1.0);
        *score_map.entry(id.clone()).or_insert(0.0) += rrf;
    }
    for (rank, (id, _)) in bm25_results.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + rank as f64 + 1.0);
        *score_map.entry(id.clone()).or_insert(0.0) += rrf;
    }

    let mut ranked: Vec<(String, f64)> = score_map.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(limit).collect()
}

/// Build a KnowledgeSearchResult from a record and scores.
pub fn build_result(
    record: KnowledgeRecord,
    vector_score: Option<f32>,
    bm25_score: Option<f32>,
    final_score: f32,
    rerank_score: Option<f32>,
) -> KnowledgeSearchResult {
    KnowledgeSearchResult {
        record,
        vector_score,
        bm25_score,
        final_score,
        rerank_score,
    }
}

/// Map domain names to knowledge type values used for filtering.
/// - "repo" → "repo_retrieval"
/// - "coding" → "coding_modification"
/// Unknown values are passed through unchanged (supports passing raw type values).
pub fn domain_to_type_values(domains: &[String]) -> Vec<String> {
    let mut types = Vec::with_capacity(domains.len());
    for d in domains {
        match d.as_str() {
            "repo" => types.push("repo_retrieval".to_string()),
            "coding" => types.push("coding_modification".to_string()),
            other => types.push(other.to_string()),
        }
    }
    types
}
