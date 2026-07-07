use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 统一的代码块表示 — 由 Tree-sitter 解析生成一次，被 Dense (LanceDB) 和 Sparse (Tantivy BM25) 两个索引共享。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CodeChunk {
    /// 跨索引系统的统一主键，格式: "{file_path}#{line_start}#{symbol_name}"
    pub snippet_id: String,
    /// 代码文件的绝对路径
    pub file_path: String,
    /// 代码块文本内容
    pub content: String,
    /// 符号名（函数名 / 结构体名等）
    pub symbol_name: String,
    /// 符号类型（"function", "struct", "class", "method" 等）
    pub symbol_type: String,
    /// 编程语言标识（"rust", "python", "javascript" 等）
    pub language: String,
    /// 代码块起始行（1-indexed）
    pub line_start: usize,
    /// 代码块结束行（1-indexed）
    pub line_end: usize,
}

impl CodeChunk {
    /// 根据文件路径、行号和符号名生成稳定的 snippet_id
    pub fn generate_id(file_path: impl AsRef<Path>, line_start: usize, symbol_name: &str) -> String {
        format!(
            "{}#{}#{}",
            file_path.as_ref().to_string_lossy(),
            line_start,
            symbol_name
        )
    }

    /// 创建一个 CodeChunk 实例
    pub fn new(
        file_path: impl Into<String>,
        content: impl Into<String>,
        symbol_name: impl Into<String>,
        symbol_type: impl Into<String>,
        language: impl Into<String>,
        line_start: usize,
        line_end: usize,
    ) -> Self {
        let file_path = file_path.into();
        let symbol_name = symbol_name.into();
        Self {
            snippet_id: Self::generate_id(&file_path, line_start, &symbol_name),
            file_path,
            content: content.into(),
            symbol_name,
            symbol_type: symbol_type.into(),
            language: language.into(),
            line_start,
            line_end,
        }
    }
}

/// 稀疏通道（BM25）的搜索结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextSearchResult {
    /// 与 CodeChunk.snippet_id 对应的标识符
    pub snippet_id: String,
    /// BM25 相关分数（越高越相关）
    pub score: f32,
    /// 文件路径
    pub file_path: String,
    /// 符号名
    pub symbol_name: String,
    /// 语言
    pub language: String,
    /// 起始行（从原始索引恢复，如果不可用则为 0）
    pub line_start: usize,
}

/// 序列化辅助：保留3位小数的 f64
fn round_3_f64<S>(x: &f64, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let val = (x * 1000.0).round() / 1000.0;
    s.serialize_f64(val)
}

/// 混合搜索的最终融合结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FusedCandidate {
    pub snippet_id: String,
    #[serde(serialize_with = "round_3_f64")]
    pub final_score: f64,
    pub file_path: String,
    pub symbol_name: String,
    pub language: String,
    pub line_start: usize,
    pub line_end: usize,
    pub code_block: String,
    pub source: CandidateSource,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CandidateSource {
    DenseOnly,
    SparseOnly,
    Fused,
}

/// 文本搜索提供者 trait — 稀疏通道的抽象接口
///
/// 实现者可以是 Tantivy BM25 索引、Elasticsearch 等任何支持词项检索的后端。
#[async_trait]
pub trait TextSearchProvider: Send + Sync {
    /// 批量索引代码块。实现者内部应处理批处理和提交。
    async fn index_chunks(&self, chunks: Vec<CodeChunk>) -> anyhow::Result<()>;

    /// 在稀疏通道中搜索。返回按 BM25 相关性排序的结果列表。
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<TextSearchResult>>;

    /// 删除指定文件路径的所有索引条目。增量更新时使用。
    async fn remove_by_path(&self, file_path: &str) -> anyhow::Result<()>;

    /// 显式提交/刷新索引变更。
    async fn commit(&self) -> anyhow::Result<()>;

    /// 检查索引是否已就绪（目录存在且可读）。用于降级决策。
    async fn is_ready(&self) -> bool;

    /// 返回索引中的文档总数。用于判断索引是否为空。
    async fn document_count(&self) -> anyhow::Result<usize>;
}
