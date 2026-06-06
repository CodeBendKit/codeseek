use serde::{Deserialize, Serialize};
use crate::config::RerankerConfig;
use crate::storage::traits_bm25::FusedCandidate;
use anyhow::{Result, anyhow};
use tracing::{info, debug};

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
    /// - `candidates`: RRF 融合后的候选列表
    ///
    /// # 返回
    /// 重排后的候选列表（按相关性分数降序），如果未启用则原样返回。
    ///
    /// # 参考
    /// 调用模式参考 `embedding_service.rs` 中的 `OpenAICompatibleEmbeddingProvider`。
    pub async fn rerank(
        &self,
        query: &str,
        candidates: Vec<FusedCandidate>,
    ) -> Result<Vec<FusedCandidate>> {
        if !self.config.enabled || candidates.is_empty() {
            return Ok(candidates);
        }

        // 检查 API Token 是否已配置
        if self.config.api_token.is_empty() {
            return Err(anyhow!(
                "Reranker API token not configured. Set [codebase.retrieval_pipeline.reranker] api_token in config.toml"
            ));
        }

        let documents: Vec<String> = candidates.iter().map(|c| c.code_block.clone()).collect();
        let top_n = self.config.top_n.min(candidates.len());

        let request = RerankRequest {
            model: self.config.model.clone(),
            query: query.to_string(),
            documents,
            top_n,
        };

        // 构建 API URL：兼容 base_url 带或不带 /v1 后缀
        let base_url = self.config.api_base_url.trim_end_matches('/');

        info!(
            "Reranker: calling {} with model={}, top_n={}",
            base_url, self.config.model, top_n
        );

        let response = self.client
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

        let mut reranked: Vec<FusedCandidate> = rerank_resp
            .results
            .into_iter()
            .filter_map(|item| {
                candidates.get(item.index).map(|orig| {
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

        debug!("Reranker: reranked {} candidates", reranked.len());
        Ok(reranked)
    }
}
