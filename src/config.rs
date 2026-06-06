use serde::Deserialize;
use std::fs;
use tracing::info;

fn default_embedding_db_uri() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    home.join(".codeseek/data/embedding")
        .to_string_lossy()
        .to_string()
}

fn default_graph_db_uri() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    home.join(".codeseek/data/graph")
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub codebase: CodeSeekConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CodeSeekConfig {
    #[serde(default)]
    pub enable_embedding: bool,
    #[serde(default = "default_embedding_db_uri")]
    pub embedding_db_uri: String,
    #[serde(default = "default_graph_db_uri")]
    pub graph_db_uri: String,
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub repo_knowledge: RepoKnowledgeConfig,
    #[serde(default)]
    pub retrieval_pipeline: RetrievalPipelineConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddingConfig {
    pub model: String,
    pub api_token: String,
    pub api_base_url: String,
    pub dimensions: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct RepoKnowledgeConfig {
    #[serde(default = "default_repo_knowledge_table_name")]
    pub table_name: String,
}

fn default_repo_knowledge_table_name() -> String {
    "repo_knowledge".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct RetrievalPipelineConfig {
    /// 是否启用多阶段级联检索 Pipeline
    #[serde(default = "default_retrieval_enabled")]
    pub enabled: bool,
    /// Hybrid Search 配置
    #[serde(default)]
    pub hybrid: HybridSearchConfig,
    /// Reranker 配置
    #[serde(default)]
    pub reranker: RerankerConfig,
    /// Graph Expansion 配置
    #[serde(default)]
    pub graph_expansion: GraphExpansionConfig,
    /// Minimum code block length (in chars after trim) to be indexed.
    /// Blocks shorter than this are skipped during embedding indexing.
    #[serde(default = "default_min_code_block_length")]
    pub min_code_block_length: usize,
}

impl Default for RetrievalPipelineConfig {
    fn default() -> Self {
        Self {
            enabled: default_retrieval_enabled(),
            hybrid: HybridSearchConfig::default(),
            reranker: RerankerConfig::default(),
            graph_expansion: GraphExpansionConfig::default(),
            min_code_block_length: default_min_code_block_length(),
        }
    }
}

fn default_retrieval_enabled() -> bool {
    false // 默认关闭，向后兼容
}

#[derive(Debug, Deserialize, Clone)]
pub struct HybridSearchConfig {
    /// BM25 索引存储路径（相对于 embedding_db_uri）
    #[serde(default = "default_bm25_index_path")]
    pub bm25_index_path: String,
    /// BM25 每通道召回数
    #[serde(default = "default_bm25_top_k")]
    pub bm25_top_k: usize,
    /// Vector 每通道召回数
    #[serde(default = "default_vector_top_k")]
    pub vector_top_k: usize,
    /// RRF 融合常数 (Reciprocal Rank Fusion k)
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f64,
    /// RRF 融合后返回 Top-K 结果数
    #[serde(default = "default_rrf_top_k")]
    pub rrf_top_k: usize,
    /// 是否启用 BM25 稀疏检索通道
    #[serde(default = "default_enable_bm25")]
    pub enable_bm25: bool,
    /// Short code threshold for penalty during fusion. Blocks shorter than this 
    /// get score penalty in RRF fusion.
    #[serde(default = "default_short_code_threshold")]
    pub short_code_threshold: usize,
    /// Penalty factor for short code blocks (0.0 ~ 1.0). 
    /// final_score *= (1.0 - max(0.0, 1.0 - len/threshold) * penalty)
    #[serde(default = "default_short_code_penalty")]
    pub short_code_penalty: f64,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            bm25_index_path: default_bm25_index_path(),
            bm25_top_k: default_bm25_top_k(),
            vector_top_k: default_vector_top_k(),
            rrf_k: default_rrf_k(),
            rrf_top_k: default_rrf_top_k(),
            enable_bm25: default_enable_bm25(),
            short_code_threshold: default_short_code_threshold(),
            short_code_penalty: default_short_code_penalty(),
        }
    }
}

fn default_bm25_index_path() -> String {
    "tantivy_bm25".to_string()
}
fn default_bm25_top_k() -> usize { 100 }
fn default_vector_top_k() -> usize { 100 }
fn default_rrf_k() -> f64 { 60.0 }
fn default_rrf_top_k() -> usize { 20 }
fn default_enable_bm25() -> bool { true }
fn default_min_code_block_length() -> usize { 16 }
fn default_short_code_threshold() -> usize { 30 }
fn default_short_code_penalty() -> f64 { 0.5 }

#[derive(Debug, Deserialize, Clone)]
pub struct RerankerConfig {
    /// 是否启用 Reranker 重排
    #[serde(default)]
    pub enabled: bool,
    /// Reranker 模型名称
    #[serde(default = "default_reranker_model")]
    pub model: String,
    /// Reranker API 的 API Key（从 TOML 配置读取，参考 embedding.api_token）
    #[serde(default)]
    pub api_token: String,
    /// Reranker API 基础 URL（如 https://api.siliconflow.cn）
    #[serde(default = "default_reranker_base_url")]
    pub api_base_url: String,
    /// 重排后保留的 Top-N 结果数
    #[serde(default = "default_reranker_top_n")]
    pub top_n: usize,
    /// 候选池倍数：RRF 融合后取 limit * candidate_multiplier 个候选传给 reranker
    #[serde(default = "default_reranker_candidate_multiplier")]
    pub candidate_multiplier: usize,
    /// Reranker API 调用超时时间（秒）
    #[serde(default = "default_reranker_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_reranker_model(),
            api_token: String::new(),  // 默认空字符串
            api_base_url: default_reranker_base_url(),
            top_n: default_reranker_top_n(),
            candidate_multiplier: default_reranker_candidate_multiplier(),
            timeout_secs: default_reranker_timeout_secs(),
        }
    }
}

fn default_reranker_model() -> String {
    "BAAI/bge-reranker-v2-m3".to_string()
}
fn default_reranker_base_url() -> String {
    "https://api.siliconflow.cn".to_string()
}
fn default_reranker_top_n() -> usize { 10 }
fn default_reranker_candidate_multiplier() -> usize { 5 }
fn default_reranker_timeout_secs() -> u64 { 30 }

#[derive(Debug, Deserialize, Clone)]
pub struct GraphExpansionConfig {
    /// 是否启用图扩展
    #[serde(default)]
    pub enabled: bool,
    /// BFS/DFS 扩展最大深度
    #[serde(default = "default_graph_max_depth")]
    pub max_depth: usize,
    /// Token 预算上限
    #[serde(default = "default_graph_token_budget")]
    pub token_budget: usize,
    /// 扩展方向: "bidirectional", "upward", "downward"
    #[serde(default = "default_graph_direction")]
    pub direction: String,
    /// 最大扩展节点数
    #[serde(default = "default_graph_max_nodes")]
    pub max_nodes: usize,
}

impl Default for GraphExpansionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_depth: default_graph_max_depth(),
            token_budget: default_graph_token_budget(),
            direction: default_graph_direction(),
            max_nodes: default_graph_max_nodes(),
        }
    }
}

fn default_graph_max_depth() -> usize { 2 }
fn default_graph_token_budget() -> usize { 4096 }
fn default_graph_direction() -> String { "bidirectional".to_string() }
fn default_graph_max_nodes() -> usize { 10 }

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let home_dir = dirs::home_dir().ok_or("Could not find home directory")?;
        let config_path = home_dir.join(".codeseek/config/config.toml");

        info!("Loading configuration from: {:?}", config_path);

        let contents = fs::read_to_string(&config_path).map_err(|e| {
            format!("Failed to read config file at {:?}: {}", config_path, e)
        })?;

        let config: Config = toml::from_str(&contents).map_err(|e| {
            format!("Failed to parse config file: {}", e)
        })?;

        Ok(config)
    }
}
