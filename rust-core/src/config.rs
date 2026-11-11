use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

/// 全文检索配置
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HybridSearchConfig {
    #[serde(default = "default_true")]
    pub enable_bm25: bool,
    /// 映射到 `services::HybridSearchConfig.sparse_limit` —— BM25 通道取回条数的下界。
    ///
    /// 实际取回条数与 RRF 保留条数由 `SearchBudget` 推导：
    /// `K_recall = max(bm25_top_k, vector_top_k, K_rerank)`，不再单独决定融合截断长度。
    #[serde(default = "default_100")]
    pub bm25_top_k: usize,
    /// 映射到 `services::HybridSearchConfig.dense_limit` —— 向量通道取回条数的下界。
    ///
    /// 与 `bm25_top_k` 一起构成 `K_recall` 的下界（见上）。
    #[serde(default = "default_100")]
    pub vector_top_k: usize,
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f64,
    #[serde(default = "default_20")]
    pub rrf_top_k: usize,
    #[serde(default = "default_short_code_threshold")]
    pub short_code_threshold: usize,
    #[serde(default = "default_short_code_penalty")]
    pub short_code_penalty: f64,
}

fn default_true() -> bool {
    true
}
fn default_100() -> usize {
    100
}
fn default_20() -> usize {
    20
}
fn default_rrf_k() -> f64 {
    60.0
}
fn default_short_code_threshold() -> usize {
    30
}
fn default_short_code_penalty() -> f64 {
    0.5
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            enable_bm25: default_true(),
            bm25_top_k: default_100(),
            vector_top_k: default_100(),
            rrf_k: default_rrf_k(),
            rrf_top_k: default_20(),
            short_code_threshold: default_short_code_threshold(),
            short_code_penalty: default_short_code_penalty(),
        }
    }
}

/// Qwen3-Embedding 官方查询侧指令（instruction-aware，非对称检索）。
///
/// 官方规范：只有 **query** 侧需要加 `Instruct: {task}\nQuery:{query}` 前缀；
/// document / passage（本项目即入库的代码块、知识记录）**绝对不加**任何指令，
/// 否则已有 LanceDB 向量索引与新查询向量处于不同分布，检索质量退化。
/// 官方另建议：指令用英文书写，每个 query 配一句任务描述。
pub const DEFAULT_QUERY_INSTRUCT: &str =
    "Given a code search query, retrieve relevant functions or code snippets that answer the query.";

/// 嵌入配置
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct EmbeddingConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_token: String,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,
    /// 查询侧 instruct 前缀中的任务描述。**仅作用于查询，不作用于入库**。
    ///
    /// 语义约定（三态）：
    /// - 字段缺省（旧配置文件 / 整个 embedding 段缺失）→ 使用 [`DEFAULT_QUERY_INSTRUCT`]
    /// - 显式配置为 `""`（或纯空白）→ **关闭** instruct 注入，查询保持裸文本
    /// - 显式配置为其它文本 → 使用该文本
    #[serde(default = "default_instruct")]
    pub instruct: Option<String>,
}

fn default_provider() -> String {
    "openai-compatible".to_string()
}
fn default_model() -> String {
    "Qwen/Qwen3-Embedding-4B".to_string()
}
fn default_api_base_url() -> String {
    "https://api.siliconflow.cn/v1".to_string()
}
fn default_dimensions() -> usize {
    2560
}
fn default_instruct() -> Option<String> {
    Some(DEFAULT_QUERY_INSTRUCT.to_string())
}

impl EmbeddingConfig {
    /// 实际生效的查询侧指令文本。返回**空串**表示关闭 instruct 注入。
    ///
    /// `Option::None`（字段缺省）与 `Some("")`（显式关闭）走不同分支，
    /// 因此调用方不能用 `unwrap_or_default()` 抹平这个区别。
    pub fn effective_instruct(&self) -> &str {
        match self.instruct.as_deref() {
            None => DEFAULT_QUERY_INSTRUCT,
            Some(s) if s.trim().is_empty() => "",
            Some(s) => s.trim(),
        }
    }
}

/// 索引配置
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IndexConfig {
    /// 代码块进入 **dense（向量）通道** 的最小长度，单位为字符数
    /// （按 `trim()` 后的 `chars().count()` 计，不是字节数）。
    ///
    /// 该阈值**仅作用于 dense / 向量通道**：过短的块语义信息不足，embedding 信噪比低，
    /// 会拉低向量检索质量。
    ///
    /// ⚠️ BM25 稀疏通道**不做长度过滤**：短代码块往往正是标识符 / 关键字查询的目标，
    /// 相关性由融合层的 `short_code_penalty`（`services/hybrid_search.rs`）降权处理。
    #[serde(default = "default_min_code_block_length")]
    pub min_code_block_length: usize,
    #[serde(default)]
    pub enable_reranker: bool,
    #[serde(default)]
    pub hybrid: HybridSearchConfig,
    #[serde(default)]
    pub reranker: RerankerConfig,
}

fn default_min_code_block_length() -> usize {
    16
}

/// 重排序配置（保留向后兼容）
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RerankerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_reranker_model")]
    pub model: String,
    #[serde(default)]
    pub api_token: String,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    /// cross-encoder 的 `top_n` **配置下界**（不再是硬性上限）。
    ///
    /// 旧语义是「最多只给 `top_n` 条候选打分」，导致最终预算大于该值时，前 `top_n` 条
    /// 用 cross-encoder 分数、其余候选带着 RRF 分数被追加，最终名次混用两套分数体系。
    ///
    /// 现在的实际语义（见 `services/reranker_service.rs::derived_top_n`）：
    /// `top_n = max(config.top_n, K_final)`，再被**可打分的文档数**夹住。
    /// 因此它退化为一个向后兼容的下界：显式配大时仍会多打分，但最终名次全部来自
    /// cross-encoder，不会被这个常量截断成两套分数混排。
    #[serde(default = "default_10")]
    pub top_n: usize,
    /// 精排池放大倍数：`K_rerank = K_final × candidate_multiplier`（至少按 1 计）。
    ///
    /// 决定从 RRF 融合结果里取多少条送进 cross-encoder（受召回总数夹住）。
    /// 配成 0 时按 1 处理 —— 精排池至少是最终预算大小，避免静默跳过精排。
    /// 未启用 reranker 时该值不生效（`K_rerank` 收敛为 `K_final`）。
    #[serde(default = "default_5")]
    pub candidate_multiplier: usize,
    #[serde(default = "default_30")]
    pub timeout_secs: u64,
}

fn default_reranker_model() -> String {
    "BAAI/bge-reranker-v2-m3".to_string()
}
fn default_10() -> usize {
    10
}
fn default_5() -> usize {
    5
}
fn default_30() -> u64 {
    30
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            min_code_block_length: default_min_code_block_length(),
            enable_reranker: false,
            hybrid: HybridSearchConfig::default(),
            reranker: RerankerConfig {
                enabled: false,
                model: default_reranker_model(),
                api_token: String::new(),
                api_base_url: default_api_base_url(),
                top_n: default_10(),
                candidate_multiplier: default_5(),
                timeout_secs: default_30(),
            },
        }
    }
}

/// 全局配置（~/.codeseek/config.json）
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub index: IndexConfig,
    /// 已安装 git hooks 的项目 { project_path → [hook_names] }
    #[serde(default)]
    pub installed_hooks: HashMap<String, Vec<String>>,
}

impl Config {
    /// 全局配置文件路径
    pub fn global_config_path() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("config.json")
    }

    /// 二进制存储目录
    pub fn bin_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("bin")
    }

    /// 项目索引目录: ~/.codeseek/projects/<project_hash>/
    pub fn project_index_dir(project_hash: &str) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("projects").join(project_hash)
    }

    /// 全局共享缓存目录: ~/.codeseek/cache/
    pub fn cache_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("cache")
    }

    /// 项目数据根目录: ~/.codeseek/projects/
    pub fn projects_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("projects")
    }

    /// CodeSeek 全局配置根目录: ~/.codeseek/
    pub fn codeseek_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek")
    }

    /// 项目 LanceDB 向量数据库目录: ~/.codeseek/projects/<project_hash>/lancedb/
    pub fn lancedb_dir(project_hash: &str) -> PathBuf {
        Self::project_index_dir(project_hash).join("lancedb")
    }

    /// 项目 BM25 全文索引目录: ~/.codeseek/projects/<project_hash>/tantivy_bm25/
    pub fn bm25_dir(project_hash: &str) -> PathBuf {
        Self::project_index_dir(project_hash).join("tantivy_bm25")
    }

    /// 项目知识向量库目录: ~/.codeseek/projects/<project_hash>/knowledge/
    pub fn knowledge_dir(project_hash: &str) -> PathBuf {
        Self::project_index_dir(project_hash).join("knowledge")
    }

    /// 项目知识 BM25 全文索引目录: ~/.codeseek/projects/<project_hash>/knowledge_index/
    pub fn knowledge_bm25_dir(project_hash: &str) -> PathBuf {
        Self::project_index_dir(project_hash).join("knowledge_index")
    }

    /// 加载全局配置
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = Self::global_config_path();
        info!("Loading configuration from: {:?}", config_path);

        if !config_path.exists() {
            return Err(format!(
                "Config file not found at {:?}. Run 'codeseek' to set up.",
                config_path
            )
            .into());
        }

        let contents = fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config file at {:?}: {}", config_path, e))?;

        let config: Config = serde_json::from_str(&contents)
            .map_err(|e| format!("Failed to parse config file: {}", e))?;

        Ok(config)
    }

    /// 保存全局配置
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::global_config_path();

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, contents)?;

        info!("Configuration saved to: {:?}", config_path);
        Ok(())
    }

    /// 从当前目录检测项目根（向上找第一个 .git/ 目录）
    pub fn detect_project_root() -> Option<PathBuf> {
        let mut current = std::env::current_dir().ok()?;
        loop {
            if current.join(".git").exists() {
                return Some(current);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// 计算项目 hash
    pub fn compute_project_hash(project_root: &PathBuf) -> String {
        format!(
            "{:x}",
            md5::compute(project_root.to_string_lossy().as_bytes())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧配置文件（完全没有 instruct 字段）必须能正常反序列化，并取到默认指令。
    #[test]
    fn embedding_config_without_instruct_field_uses_default() {
        let json = r#"{
            "provider": "openai-compatible",
            "model": "Qwen/Qwen3-Embedding-4B",
            "api_token": "sk-test",
            "api_base_url": "https://api.siliconflow.cn/v1",
            "dimensions": 2560
        }"#;
        let cfg: EmbeddingConfig =
            serde_json::from_str(json).expect("legacy config must deserialize");
        assert_eq!(cfg.instruct.as_deref(), Some(DEFAULT_QUERY_INSTRUCT));
        assert_eq!(cfg.effective_instruct(), DEFAULT_QUERY_INSTRUCT);
    }

    /// 显式配置为空字符串 → 字段保持空串（而不是被默认值覆盖），并且 effective_instruct 表示关闭。
    #[test]
    fn embedding_config_explicit_empty_instruct_stays_empty() {
        let json = r#"{"instruct": ""}"#;
        let cfg: EmbeddingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.instruct.as_deref(), Some(""));
        assert_eq!(cfg.effective_instruct(), "");
    }

    /// 纯空白同样视为关闭（避免拼出 "Instruct: \nQuery:xxx" 这种污染查询）。
    #[test]
    fn embedding_config_whitespace_only_instruct_disables_injection() {
        let cfg: EmbeddingConfig = serde_json::from_str(r#"{"instruct": "   "}"#).unwrap();
        assert_eq!(cfg.effective_instruct(), "");
    }

    /// 自定义指令：原样使用（首尾空白裁掉）。
    #[test]
    fn embedding_config_custom_instruct_is_used_verbatim_after_trim() {
        let cfg: EmbeddingConfig = serde_json::from_str(
            r#"{"instruct": "  Given a question, retrieve matching documents  "}"#,
        )
        .unwrap();
        assert_eq!(
            cfg.effective_instruct(),
            "Given a question, retrieve matching documents"
        );
    }

    /// 整个 embedding 段缺失（`Config` 上挂了 `#[serde(default)]`）→ 走 Default(None) → 仍是默认指令。
    #[test]
    fn config_without_embedding_section_falls_back_to_default_instruct() {
        let cfg: Config =
            serde_json::from_str(r#"{}"#).expect("empty config object must deserialize");
        assert_eq!(cfg.embedding.instruct, None);
        assert_eq!(cfg.embedding.effective_instruct(), DEFAULT_QUERY_INSTRUCT);
        assert_eq!(DEFAULT_QUERY_INSTRUCT,
            "Given a code search query, retrieve relevant functions or code snippets that answer the query.");
    }

    /// 保存/加载往返：自定义值与"显式关闭"都能无损写回 JSON。
    #[test]
    fn instruct_field_survives_serialization_round_trip() {
        let cfg = EmbeddingConfig {
            instruct: Some("custom instruct".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: EmbeddingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.effective_instruct(), "custom instruct");

        let off = EmbeddingConfig {
            instruct: Some(String::new()),
            ..Default::default()
        };
        let json = serde_json::to_string(&off).unwrap();
        let back: EmbeddingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.instruct.as_deref(), Some(""));
        assert_eq!(back.effective_instruct(), "");
    }
}
