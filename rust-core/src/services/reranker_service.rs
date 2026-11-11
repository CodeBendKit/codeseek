use crate::config::RerankerConfig;
use crate::storage::traits_bm25::FusedCandidate;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Cross-Encoder Reranker 服务
///
/// 调用兼容 OpenAI API 协议的 Reranker 端点（如 SiliconFlow /v1/rerank）
/// 对 RRF 融合后的候选结果进行语义精排。
///
/// 参考：`embedding_service.rs` 中 `OpenAICompatibleEmbeddingProvider` 的调用模式。
pub struct RerankerService {
    config: RerankerConfig,
    client: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct RerankRequest {
    model: String,
    query: String,
    documents: Vec<String>,
    top_n: usize,
}

#[derive(Debug, Deserialize)]
struct RerankResponse {
    results: Vec<RerankResultItem>,
}

#[derive(Debug, Deserialize)]
struct RerankResultItem {
    index: usize,
    #[serde(alias = "score")]
    relevance_score: f64,
}

/// 挑出可送入 cross-encoder 的候选：只有正文非空的候选才有意义。
///
/// 返回 `(documents, candidate_indices)`，`candidate_indices[i]` 是 `documents[i]`
/// 对应的候选下标 —— cross-encoder 响应里的 `index` 是 **documents 的下标**，
/// 必须经它映射回候选，绝不能用它直接索引候选列表。
///
/// 纯空白正文（`"  \n\t "`）与空串同等对待：BM25 通道的 content 非 STORED，
/// 这种候选一律回填失败，送给模型只会得到无意义分数。
fn collect_documents(candidates: &[FusedCandidate]) -> (Vec<String>, Vec<usize>) {
    let mut documents = Vec::new();
    let mut candidate_indices = Vec::new();

    for (idx, c) in candidates.iter().enumerate() {
        if !c.code_block.trim().is_empty() {
            documents.push(c.code_block.clone());
            candidate_indices.push(idx);
        }
    }

    (documents, candidate_indices)
}

/// 由最终返回预算推导 cross-encoder 的 `top_n`。
///
/// 不变式：`k_final <= top_n <= 文档数`（文档数本身 ≤ 精排池 ≤ K_rerank）。
/// - 下界取 `k_final`：保证最终名次全部来自 cross-encoder 分数，不再出现「前 10 条是
///   精排分数、后 10 条是 RRF 分数」的混排；
/// - `configured_top_n` 作为配置下界保留（显式配大值时仍多打分，但会被文档数夹住）；
/// - 上界取 `document_count`：满足服务端 `top_n <= documents.len()` 的约束。
fn derived_top_n(configured_top_n: usize, final_budget: usize, document_count: usize) -> usize {
    configured_top_n.max(final_budget).min(document_count)
}

impl RerankerService {
    pub fn new(config: RerankerConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .expect("Failed to build reqwest client for RerankerService");
        Self { config, client }
    }

    /// 获取配置的只读引用
    pub fn config(&self) -> &RerankerConfig {
        &self.config
    }

    /// 执行 Cross-Encoder 重排
    ///
    /// # 参数
    /// - `query`: 用户的原始搜索查询
    /// - `candidates`: RRF 融合后的候选列表（精排池，大小 ≤ `K_rerank`）
    /// - `final_budget`: **最终返回预算**（调用方的 `K_final`）。它决定送进 cross-encoder
    ///   的 `top_n` 下界，保证最终名次全部来自 cross-encoder 分数。
    ///
    /// # 返回
    /// 重排后的候选列表（按相关性分数降序），如果未启用则原样返回。
    ///
    /// # 语义
    /// 只有 `code_block` 非空的候选才会送进 cross-encoder：空文档对模型等同噪声，
    /// 只会把本来相关的候选错误压到底部（BM25 通道的 content 非 STORED，若不回填就会
    /// 出现这种候选）。缺正文的候选**不参与打分**，按原 RRF 顺序追加在已重排结果之后，
    /// 并保留各自的 RRF `final_score` —— 两套分数体系不混排。全部候选都缺正文时直接
    /// 跳过 HTTP 调用。
    ///
    /// 由于 `top_n = max(config.top_n, final_budget)`（再被可打分文档数夹住），
    /// `top_n >= K_final` 恒成立，因此调用方截断到 `K_final` 之后，名次应**全部**来自
    /// cross-encoder 分数；只有当「有正文、能被打分的候选」少于 `K_final` 时，才可能由
    /// 这些未打分候选补齐。
    ///
    /// # 参考
    /// 调用模式参考 `embedding_service.rs` 中的 `OpenAICompatibleEmbeddingProvider`。
    pub async fn rerank(
        &self,
        query: &str,
        candidates: Vec<FusedCandidate>,
        final_budget: usize,
    ) -> Result<Vec<FusedCandidate>> {
        if !self.config.enabled || candidates.is_empty() {
            return Ok(candidates);
        }

        // 最终预算为 0 时无需调用服务端：调用方不会返回任何结果
        if final_budget == 0 {
            return Ok(Vec::new());
        }

        // 检查 API Token 是否已配置
        if self.config.api_token.is_empty() {
            return Err(anyhow!(
                "Reranker API token not configured. Set [codebase.retrieval_pipeline.reranker] api_token in config.toml"
            ));
        }

        // 只挑有正文的候选送 cross-encoder：空文档会让模型给出无意义分数，
        // 把本来相关的候选错误压到底部
        let (documents, candidate_indices) = collect_documents(&candidates);
        if documents.is_empty() {
            warn!(
                "Reranker: {} 条候选全部缺少正文，跳过 cross-encoder 精排（保留 RRF 顺序）",
                candidates.len()
            );
            return Ok(candidates);
        }
        if documents.len() < candidates.len() {
            warn!(
                "Reranker: {} 条候选缺少正文，不送 cross-encoder，将排在已重排结果之后",
                candidates.len() - documents.len()
            );
        }

        let top_n = derived_top_n(self.config.top_n, final_budget, documents.len());

        let request = RerankRequest {
            model: self.config.model.clone(),
            query: query.to_string(),
            documents,
            top_n,
        };

        // 构建 API URL：兼容 base_url 带或不带 /v1 后缀
        let base_url = self.config.api_base_url.trim_end_matches('/');

        info!(
            "Reranker: calling {} with model={}, top_n={} (final_budget={}, configured_top_n={})",
            base_url, self.config.model, top_n, final_budget, self.config.top_n
        );

        let response = self
            .client
            .post(base_url)
            .header("Authorization", format!("Bearer {}", self.config.api_token))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Reranker HTTP request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Reranker API returned error status {}: {}",
                status,
                body
            ));
        }

        let rerank_resp: RerankResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Reranker response parse failed: {}", e))?;

        // `scored[i]` 标记候选 i 是否已拿到 relevance_score
        let mut scored = vec![false; candidates.len()];
        let mut reranked: Vec<FusedCandidate> = rerank_resp
            .results
            .into_iter()
            // 服务端返回的 index 是 documents 的下标，必须经 candidate_indices 映射回候选
            .filter_map(|item| {
                let cand_idx = *candidate_indices.get(item.index)?;
                if scored[cand_idx] {
                    // 服务端重复返回同一 index 时只保留首条，避免候选重复
                    return None;
                }
                scored[cand_idx] = true;
                candidates.get(cand_idx).map(|orig| {
                    let mut c = orig.clone();
                    c.final_score = item.relevance_score;
                    c
                })
            })
            .collect();

        // 确保按分数降序排列
        reranked.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // 未参与打分的候选（缺正文，或服务端漏返回）按原 RRF 顺序追加到末尾：
        // 它们的 final_score 是 RRF 分数，与 relevance_score 混排只会得到错误顺序
        for (idx, c) in candidates.iter().enumerate() {
            if !scored[idx] {
                reranked.push(c.clone());
            }
        }

        debug!("Reranker: reranked {} candidates", reranked.len());
        Ok(reranked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::traits_bm25::CandidateSource;

    fn candidate(code_block: &str) -> FusedCandidate {
        FusedCandidate {
            snippet_id: "id".to_string(),
            final_score: 0.5,
            file_path: "/tmp/a.rs".to_string(),
            symbol_name: "sym".to_string(),
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            code_block: code_block.to_string(),
            source: CandidateSource::SparseOnly,
        }
    }

    #[test]
    fn collect_documents_returns_empty_when_all_bodies_missing() {
        // 全部候选都没有正文时不应产生任何 document（rerank 据此跳过 HTTP 调用）
        let candidates = vec![candidate(""), candidate("")];

        let (documents, candidate_indices) = collect_documents(&candidates);

        assert!(documents.is_empty());
        assert!(candidate_indices.is_empty());
    }

    #[test]
    fn collect_documents_keeps_non_empty_bodies_with_matching_indices() {
        let candidates = vec![candidate("body-0"), candidate(""), candidate("body-2")];

        let (documents, candidate_indices) = collect_documents(&candidates);

        assert_eq!(documents, vec!["body-0".to_string(), "body-2".to_string()]);
        assert_eq!(
            candidate_indices,
            vec![0, 2],
            "documents[i] 必须对应 candidates[candidate_indices[i]]"
        );
    }

    #[test]
    fn collect_documents_treats_blank_body_as_empty() {
        let candidates = vec![candidate("  \n\t "), candidate("real body")];

        let (documents, candidate_indices) = collect_documents(&candidates);

        assert_eq!(documents.len(), 1, "纯空白正文不应送 cross-encoder");
        assert_eq!(documents[0], "real body");
        assert_eq!(candidate_indices, vec![1]);
    }

    // ---- derived_top_n ---------------------------------------------------

    #[test]
    fn derived_top_n_follows_budget_when_budget_exceeds_config() {
        // 配置 10 / 预算 20 / 文档 50 ⇒ 20：预算大于配置时按预算
        assert_eq!(derived_top_n(10, 20, 50), 20);
    }

    #[test]
    fn derived_top_n_clamped_by_document_count() {
        // 配置 10 / 预算 20 / 文档 5 ⇒ 5：文档不足时按文档数
        assert_eq!(derived_top_n(10, 20, 5), 5);
    }

    #[test]
    fn derived_top_n_keeps_larger_configured_value() {
        // 配置 50 / 预算 10 / 文档 100 ⇒ 50：配置更大时仍按配置（兼容保留值）
        assert_eq!(derived_top_n(50, 10, 100), 50);
    }
}
