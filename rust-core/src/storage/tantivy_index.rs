//! Tantivy-based BM25 full-text search index for code.
//!
//! Reuses Tree-sitter parsed code chunks (AST symbols: functions, structs, etc.)
//! as the indexing unit — NOT whole files.

use crate::storage::traits_bm25::{
    parse_snippet_id, CodeChunk, TextSearchProvider, TextSearchResult,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use regex::Regex;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    Field, FieldEntry, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, STORED, STRING,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexWriter, TantivyDocument, Term};
use tokio::sync::Mutex;

// ─────────────────────────────────────────────
// Custom Code Tokenizer
// ─────────────────────────────────────────────

/// 自定义 code 分词器在 schema 中的注册名。
/// 所有「代码文本」字段（content / symbol_name）都必须绑定这个名字，
/// 否则自定义分词逻辑不会生效（这正是本次修复的系统性缺陷）。
const CODE_TOKENIZER: &str = "code";

/// 构建代码文本字段的索引选项。
///
/// - 绑定 [`CODE_TOKENIZER`]，使 `parse_snippet_id` / `parseSnippetId` 能互相命中；
/// - 记录词频与位置：BM25 打分需要词频，多 token 查询会退化为 phrase query，需要位置。
///
/// `stored` 为 true 时同时存储原文（用于回显）；content 字段必须保持非 STORED，
/// `hybrid_search` 依赖这一点（它只从 snippet_id 反解行号，不回读 content）。
fn code_text_options(stored: bool) -> TextOptions {
    let indexing = TextFieldIndexing::default()
        .set_tokenizer(CODE_TOKENIZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);

    let options = TextOptions::default().set_indexing_options(indexing);
    if stored {
        options.set_stored()
    } else {
        options
    }
}

/// Tantivy tokenizer for code identifiers.
/// Splits snake_case and CamelCase, emits sub-tokens + original.
///
/// 正则缓存为实例字段（`Regex: Clone`），避免每次 `token_stream()` 重新编译。
/// 正则使用 Unicode 属性类 `\p{L}\p{N}`：既覆盖 ASCII 标识符，也保留中文/日文等
/// CJK 字符（旧实现用 `[a-zA-Z0-9_]+` 会把中文注释、中文字符串整体丢弃）。
#[derive(Clone)]
pub struct CodeTokenizer {
    word_re: Regex,
}

impl Default for CodeTokenizer {
    fn default() -> Self {
        // 该模式是编译期常量，格式永远合法；若失败说明 regex crate 语义被破坏。
        let word_re = Regex::new(r"[\p{L}\p{N}_]+").expect("code tokenizer regex must compile");
        Self { word_re }
    }
}

impl CodeTokenizer {
    /// 把一段代码文本切成 code-aware token（全部小写）。
    ///
    /// 规则：
    /// 1. 用 `[\p{L}\p{N}_]+` 抓出片段（含 CJK）；
    /// 2. 纯 ASCII 片段 → `split_camel_snake`（snake_case 拆分 + CamelCase 拆分）；
    /// 3. 含非 ASCII 片段 → 按「ASCII run / 非 ASCII run」切分：ASCII run 走
    ///    `split_camel_snake`，非 ASCII run 整体小写作单个 token（不自行实现中文分词，
    ///    行为与 SimpleTokenizer 一致）；片段产出多于 1 个 token 时再附加整片段小写形式。
    fn split_text_into_code_tokens(&self, text: &str) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();

        for m in self.word_re.find_iter(text) {
            let fragment = m.as_str();
            if fragment.is_empty() {
                continue;
            }

            if fragment.is_ascii() {
                result.extend(split_camel_snake(fragment));
            } else {
                result.extend(split_mixed_fragment(fragment));
            }
        }

        // 兜底：不留下空 token
        result.retain(|token| !token.is_empty());
        result
    }
}

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream<'a>;

    fn token_stream<'a>(&mut self, text: &'a str) -> Self::TokenStream<'a> {
        let tokens = self.split_text_into_code_tokens(text);
        CodeTokenStream::new(tokens)
    }
}

/// 处理含非 ASCII 字符的片段：按 ASCII / 非 ASCII 连续段切分后分别产 token。
fn split_mixed_fragment(fragment: &str) -> Vec<String> {
    let mut sub_tokens: Vec<String> = Vec::new();
    let mut ascii_run = String::new();
    let mut cjk_run = String::new();

    // 先把一段连续 run 冲刷成 token（ASCII run 需进一步拆分标识符，CJK run 整体保留）
    fn flush(sub_tokens: &mut Vec<String>, ascii_run: &mut String, cjk_run: &mut String) {
        if !ascii_run.is_empty() {
            sub_tokens.extend(split_camel_snake(ascii_run));
            ascii_run.clear();
        }
        if !cjk_run.is_empty() {
            sub_tokens.push(cjk_run.to_lowercase());
            cjk_run.clear();
        }
    }

    for ch in fragment.chars() {
        if ch.is_ascii() {
            if !cjk_run.is_empty() {
                // 进入 ASCII 段前先把累积的非 ASCII 段落盘
                sub_tokens.push(cjk_run.to_lowercase());
                cjk_run.clear();
            }
            ascii_run.push(ch);
        } else {
            if !ascii_run.is_empty() {
                // 进入非 ASCII 段前先把累积的 ASCII 段落盘
                sub_tokens.extend(split_camel_snake(&ascii_run));
                ascii_run.clear();
            }
            cjk_run.push(ch);
        }
    }
    flush(&mut sub_tokens, &mut ascii_run, &mut cjk_run);

    // 片段被拆成多个 token 时，额外保留整片段的小写形式（对齐原实现「子 token + 原文」策略）
    if sub_tokens.len() > 1 {
        sub_tokens.push(fragment.to_lowercase());
    }

    sub_tokens
}

fn split_camel_snake(word: &str) -> Vec<String> {
    let mut sub_tokens: Vec<String> = Vec::new();

    for segment in word.split('_').filter(|s| !s.is_empty()) {
        let chars: Vec<char> = segment.chars().collect();
        let mut current = String::new();

        for (i, ch) in chars.into_iter().enumerate() {
            if i > 0 && ch.is_uppercase() {
                if !current.is_empty() {
                    sub_tokens.push(current.to_lowercase());
                }
                current = ch.to_lowercase().collect();
            } else {
                current.extend(ch.to_lowercase());
            }
        }

        if !current.is_empty() {
            sub_tokens.push(current);
        }
    }

    // 多 token 时追加规范化别名，使 camelCase 与 snake_case 产出**同一条 token 序列**：
    // - `_` 连接形式：snake_case 原文（parse_snippet_id）
    // - 去掉分隔符的小写形式：camelCase 原文小写（parsesnippetid）
    //
    // 这里刻意不用单一个 `word.to_lowercase()`（原实现做法）：QueryParser 对多 token
    // 查询生成 slop=0 的 PhraseQuery，要求整段 token 序列在文档中连续出现；若两条
    // 别名只取「原文小写」，`parseSnippetId` 与 `parse_snippet_id` 会得到不同的 token
    // 序列，phrase 必然匹配失败。两种形式都产出后，`[parse, snippet, id,
    // parse_snippet_id, parsesnippetid]` 对两种写法完全一致，才能双向互查。
    if sub_tokens.len() > 1 {
        let joined = sub_tokens.join("_");
        let jammed = sub_tokens.concat();
        for alias in [joined, jammed] {
            if !sub_tokens.iter().any(|token| *token == alias) {
                sub_tokens.push(alias);
            }
        }
    }

    if sub_tokens.is_empty() {
        vec![word.to_lowercase()]
    } else {
        sub_tokens
    }
}

/// Lightweight TokenStream backed by a pre-built Vec.
pub struct CodeTokenStream<'a> {
    tokens: Vec<Token>,
    pos: usize,
    /// Sentinel token returned when advance() has exhausted all tokens.
    /// Used for both token() and token_mut() after the last token.
    eof_token: Token,
    _marker: PhantomData<&'a str>,
}

impl<'a> CodeTokenStream<'a> {
    fn new(tokens: Vec<String>) -> Self {
        let token_vec: Vec<Token> = tokens
            .into_iter()
            .enumerate()
            .map(|(pos, text)| {
                let mut token = Token::default();
                token.position = pos;
                token.text = text;
                token.position_length = 1;
                token
            })
            .collect();

        let eof_token = Token::default();
        CodeTokenStream {
            tokens: token_vec,
            pos: 0,
            eof_token,
            _marker: PhantomData,
        }
    }
}

impl<'a> TokenStream for CodeTokenStream<'a> {
    fn advance(&mut self) -> bool {
        if self.pos < self.tokens.len() {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        // pos == 0 表示尚未 advance()（空 token 流也停在这里），此时必须返回哨兵，
        // 不能用 pos - 1 直接下标（usize 下溢会 panic）。
        if let Some(idx) = self.pos.checked_sub(1) {
            if let Some(token) = self.tokens.get(idx) {
                return token;
            }
        }
        &self.eof_token
    }

    fn token_mut(&mut self) -> &mut Token {
        if let Some(idx) = self.pos.checked_sub(1) {
            if idx < self.tokens.len() {
                return &mut self.tokens[idx];
            }
        }
        &mut self.eof_token
    }
}

// ─────────────────────────────────────────────
// Tantivy BM25 Index
// ─────────────────────────────────────────────

/// Tantivy-backed BM25 text search index.
///
/// Indexes code at the **chunk level** (each AST symbol — function, struct, etc.)
/// rather than at the whole-file level. This ensures alignment with the
/// dense vector index and enables precise RRF fusion.
pub struct TantivyBm25Index {
    index: Index,
    writer: Arc<Mutex<Option<IndexWriter>>>,
    schema: Schema,
    snippet_id_field: Field,
    file_path_field: Field,
    content_field: Field,
    symbol_name_field: Field,
    language_field: Field,
    /// 本次 `open_or_create` 是否因 schema/tokenizer 变更重建了索引目录。
    /// 重建后磁盘上的稀疏索引为空，调用方必须触发一次全量索引（传 `None`）才能回填。
    rebuilt: bool,
}

impl TantivyBm25Index {
    /// Open an existing index or create a new one at the given directory.
    ///
    /// 与旧实现的关键差异：**不再盲目复用磁盘上的索引**。打开成功后会先用
    /// [`Self::schema_matches`] 比较 schema，不一致（例如老索引的 content 还是默认
    /// `TEXT` 分词器）就整目录重建 —— 否则后续写入仍按旧 tokenizer 建索引，
    /// 改本地 schema 构建代码对老索引完全无效。
    pub fn open_or_create<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        // snippet_id / file_path / language 必须保持 raw（STRING）：
        // remove_by_path() 用 Term::from_field_text 精确匹配，分词会让删除失效。
        let snippet_id_field = schema_builder.add_text_field("snippet_id", STRING | STORED);
        let file_path_field = schema_builder.add_text_field("file_path", STRING | STORED);
        // 代码文本字段绑定自定义 code 分词器（content 不可 STORED，hybrid_search 依赖）
        let content_field = schema_builder.add_text_field("content", code_text_options(false));
        let symbol_name_field =
            schema_builder.add_text_field("symbol_name", code_text_options(true));
        let language_field = schema_builder.add_text_field("language", STRING | STORED);

        let schema = schema_builder.build();

        // Create or open the index directory
        let dir_path = dir.as_ref();
        std::fs::create_dir_all(dir_path)
            .map_err(|e| anyhow!("Failed to create Tantivy index dir {:?}: {}", dir_path, e))?;

        let has_entries = std::fs::read_dir(dir_path)
            .map_err(|e| anyhow!("Failed to read Tantivy index dir {:?}: {}", dir_path, e))?
            .next()
            .is_some();

        let (index, rebuilt) = if has_entries {
            match Index::open_in_dir(dir_path) {
                Ok(existing) => {
                    if Self::schema_matches(&schema, &existing.schema()) {
                        // schema 一致 —— 直接复用磁盘索引
                        (existing, false)
                    } else {
                        tracing::warn!(
                            "Tantivy BM25 索引 schema/tokenizer 已变更（磁盘索引未使用 code 分词器），重建目录 {:?}",
                            dir_path
                        );
                        (Self::recreate_index(dir_path, &schema)?, true)
                    }
                }
                Err(e) => {
                    // 损坏或版本不兼容：不能直接 create_in_dir（目录非空会返回
                    // IndexAlreadyExists），必须先清空再建。
                    tracing::warn!(
                        "打开已有 Tantivy BM25 索引失败（可能损坏或版本不兼容）：{}，重建目录 {:?}",
                        e,
                        dir_path
                    );
                    (Self::recreate_index(dir_path, &schema)?, true)
                }
            }
        } else {
            // Empty or new directory
            let index = Index::create_in_dir(dir_path, schema.clone())
                .map_err(|e| anyhow!("Failed to create Tantivy index at {:?}: {}", dir_path, e))?;
            (index, false)
        };

        if rebuilt {
            tracing::warn!(
                "BM25 索引已因 schema/tokenizer 变更重建，当前稀疏通道为空；\
                 需要重新执行全量索引（codeseek index）才能回填稀疏通道"
            );
        }

        // Register the custom code tokenizer
        // Note: LowercaseFilter was removed in tantivy 0.22.
        // split_camel_snake() already outputs lowercase tokens, so no filter needed.
        // 注册必须早于 writer 创建：写入时按 schema 里记录的 tokenizer 名字查找分析器。
        let code_analyzer =
            tantivy::tokenizer::TextAnalyzer::builder(CodeTokenizer::default()).build();
        index.tokenizers().register(CODE_TOKENIZER, code_analyzer);

        let writer = index
            .writer(50_000_000) // 50MB heap
            .map_err(|e| anyhow!("Failed to create Tantivy IndexWriter: {}", e))?;

        Ok(Self {
            index,
            schema,
            writer: Arc::new(Mutex::new(Some(writer))),
            snippet_id_field,
            file_path_field,
            content_field,
            symbol_name_field,
            language_field,
            rebuilt,
        })
    }

    /// 本次打开是否发生过索引重建（schema/tokenizer 变更导致）。
    ///
    /// 为 `true` 时调用方应放弃增量索引、改为全量重建，否则稀疏通道会一直是空的。
    pub fn was_rebuilt(&self) -> bool {
        self.rebuilt
    }

    /// 当前索引使用的 schema（只读，供诊断与测试使用）。
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// 比较两份 schema 是否等价。
    ///
    /// 字段数量必须相同，并按 [`Schema::fields`] 顺序逐项比较：字段句柄、字段名、
    /// 字段类型。`FieldType` 的 `PartialEq` 覆盖了 tokenizer 名、index record option、
    /// stored 标志等全部索引配置，因此「换分词器」这种变更也能被检出。
    fn schema_matches(expected: &Schema, actual: &Schema) -> bool {
        let expected_fields: Vec<(Field, &FieldEntry)> = expected.fields().collect();
        let actual_fields: Vec<(Field, &FieldEntry)> = actual.fields().collect();

        if expected_fields.len() != actual_fields.len() {
            return false;
        }

        expected_fields.iter().zip(actual_fields.iter()).all(
            |((expected_field, expected_entry), (actual_field, actual_entry))| {
                expected_field == actual_field
                    && expected_entry.name() == actual_entry.name()
                    && expected_entry.field_type() == actual_entry.field_type()
            },
        )
    }

    /// 删除并按新 schema 重建索引目录。全部走 `anyhow` 上下文，不 panic。
    fn recreate_index(dir_path: &Path, schema: &Schema) -> Result<Index> {
        if dir_path.exists() {
            std::fs::remove_dir_all(dir_path).map_err(|e| {
                anyhow!(
                    "Failed to remove stale Tantivy index dir {:?}: {}",
                    dir_path,
                    e
                )
            })?;
        }
        std::fs::create_dir_all(dir_path)
            .map_err(|e| anyhow!("Failed to recreate Tantivy index dir {:?}: {}", dir_path, e))?;
        Index::create_in_dir(dir_path, schema.clone())
            .map_err(|e| anyhow!("Failed to create Tantivy index at {:?}: {}", dir_path, e))
    }

    /// Helper: extract a stored string field from a TantivyDocument.
    fn get_stored_string(doc: &TantivyDocument, field: Field, schema: &Schema) -> String {
        schema
            .get_field_entry(field)
            .is_stored()
            .then(|| {
                doc.get_first(field).and_then(|val| match val {
                    tantivy::schema::OwnedValue::Str(s) => Some(s.clone()),
                    _ => None,
                })
            })
            .flatten()
            .unwrap_or_default()
    }
}

#[async_trait]
impl TextSearchProvider for TantivyBm25Index {
    async fn index_chunks(&self, chunks: Vec<CodeChunk>) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let writer = self.writer.clone();
        let snippet_id_f = self.snippet_id_field;
        let file_path_f = self.file_path_field;
        let content_f = self.content_field;
        let symbol_f = self.symbol_name_field;
        let lang_f = self.language_field;

        tokio::task::spawn_blocking(move || {
            let mut w = writer.blocking_lock();
            let writer = w
                .as_mut()
                .ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;

            for chunk in chunks {
                let mut doc = TantivyDocument::default();
                doc.add_text(snippet_id_f, &chunk.snippet_id);
                doc.add_text(file_path_f, &chunk.file_path);
                doc.add_text(content_f, &chunk.content);
                doc.add_text(symbol_f, &chunk.symbol_name);
                doc.add_text(lang_f, &chunk.language);
                writer
                    .add_document(doc)
                    .map_err(|e| anyhow!("Failed to add document: {}", e))?;
            }

            writer
                .commit()
                .map_err(|e| anyhow!("Tantivy commit failed: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Tantivy spawn_blocking panicked: {:?}", e))?
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<TextSearchResult>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self
            .index
            .reader()
            .map_err(|e| anyhow!("Tantivy reader error: {}", e))?;
        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![self.content_field, self.symbol_name_field],
        );

        // Sanitize query: wrap in quotes for exact phrase, catch parse errors
        let parsed_query = query_parser.parse_query(query).unwrap_or_else(|_| {
            let sanitized = query.replace(['"', '*', '?'], "");
            if sanitized.contains(' ') {
                query_parser
                    .parse_query(&format!("\"{}\"", sanitized))
                    .unwrap_or_else(|_| query_parser.parse_query(&sanitized).unwrap())
            } else {
                query_parser.parse_query(&sanitized).unwrap()
            }
        });

        let top_docs = searcher
            .search(&parsed_query, &TopDocs::with_limit(limit.max(1)))
            .map_err(|e| anyhow!("Tantivy search error: {}", e))?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher
                .doc::<TantivyDocument>(doc_address)
                .map_err(|e| anyhow!("Tantivy doc fetch error: {}", e))?;

            // snippet_id 格式 `{file_path}#{line_start}#{symbol_name}`（CodeChunk::generate_id），
            // 是唯一携带真实行号的载体 —— Tantivy schema 未存 line_start 字段。
            let snippet_id = Self::get_stored_string(&doc, self.snippet_id_field, &self.schema);
            let line_start = parse_snippet_id(&snippet_id)
                .map(|(_, line, _)| line)
                .unwrap_or(0);

            results.push(TextSearchResult {
                snippet_id,
                score: _score,
                file_path: Self::get_stored_string(&doc, self.file_path_field, &self.schema),
                symbol_name: Self::get_stored_string(&doc, self.symbol_name_field, &self.schema),
                language: Self::get_stored_string(&doc, self.language_field, &self.schema),
                line_start,
            });
        }

        Ok(results)
    }

    async fn remove_by_path(&self, file_path: &str) -> Result<()> {
        let writer = self.writer.clone();
        let field = self.file_path_field;
        let term = Term::from_field_text(field, file_path);

        tokio::task::spawn_blocking(move || {
            let mut w = writer.blocking_lock();
            let writer = w
                .as_mut()
                .ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;
            writer.delete_term(term);
            writer
                .commit()
                .map_err(|e| anyhow!("Tantivy commit after delete failed: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Tantivy spawn_blocking panicked: {:?}", e))?
    }

    async fn commit(&self) -> Result<()> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let mut w = writer.blocking_lock();
            let writer = w
                .as_mut()
                .ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;
            writer
                .commit()
                .map_err(|e| anyhow!("Tantivy commit failed: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Tantivy spawn_blocking panicked: {:?}", e))?
    }

    async fn is_ready(&self) -> bool {
        // Check if we can read from the index
        self.index.reader().is_ok()
    }

    async fn document_count(&self) -> anyhow::Result<usize> {
        let reader = self
            .index
            .reader()
            .map_err(|e| anyhow!("Tantivy reader error: {}", e))?;
        let searcher = reader.searcher();
        Ok(searcher.num_docs() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::{FieldType, TEXT};
    use tempfile::tempdir;

    /// 取出某个文本字段的索引配置（断言专用）。
    fn text_indexing(schema: &Schema, field_name: &str) -> TextFieldIndexing {
        let field = schema
            .get_field(field_name)
            .unwrap_or_else(|_| panic!("schema 中应存在字段 {}", field_name));
        match schema.get_field_entry(field).field_type() {
            FieldType::Str(options) => options
                .get_indexing_options()
                .cloned()
                .unwrap_or_else(|| panic!("字段 {} 应被索引", field_name)),
            other => panic!("字段 {} 不是文本字段: {:?}", field_name, other),
        }
    }

    #[test]
    fn schema_uses_code_tokenizer_for_code_fields() {
        let dir = tempdir().unwrap();
        let index = TantivyBm25Index::open_or_create(dir.path()).unwrap();
        let schema = index.schema();

        // content / symbol_name 必须绑定 code 分词器并保留词频与位置
        for field_name in ["content", "symbol_name"] {
            let indexing = text_indexing(schema, field_name);
            assert_eq!(
                indexing.tokenizer(),
                CODE_TOKENIZER,
                "字段 {} 应使用 code 分词器（否则自定义分词是死代码）",
                field_name
            );
            assert_eq!(
                indexing.index_option(),
                IndexRecordOption::WithFreqsAndPositions,
                "字段 {} 需要词频（BM25）与位置（phrase query）",
                field_name
            );
        }

        // 元数据字段必须保持 raw，remove_by_path 依赖其精确 Term 匹配
        for field_name in ["file_path", "snippet_id", "language"] {
            assert_eq!(
                text_indexing(schema, field_name).tokenizer(),
                "raw",
                "字段 {} 必须保持 raw 分词器",
                field_name
            );
        }

        // content 不可 STORED（hybrid_search 依赖这一点）
        let content_field = schema.get_field("content").unwrap();
        assert!(
            !schema.get_field_entry(content_field).is_stored(),
            "content 字段不可 STORED"
        );
        let symbol_field = schema.get_field("symbol_name").unwrap();
        assert!(
            schema.get_field_entry(symbol_field).is_stored(),
            "symbol_name 字段应 STORED 以便回显"
        );
    }

    #[tokio::test]
    async fn camel_case_query_matches_snake_case_chunk() {
        let dir = tempdir().unwrap();
        let index = TantivyBm25Index::open_or_create(dir.path()).unwrap();

        let chunk = CodeChunk::new(
            "/tmp/b.rs",
            "fn parse_snippet_id(raw: &str) -> Option<(Str, usize, Str)> { let s = raw; }",
            "parse_snippet_id",
            "function",
            "rust",
            7,
            12,
        );
        index.index_chunks(vec![chunk]).await.unwrap();
        index.commit().await.unwrap();

        // 默认 TEXT 分词器下 "parseSnippetId" 只会产生单 token `parsesnippetid`，
        // 永远匹配不到 `parse_snippet_id`；能命中即证明 code 分词器真的生效了。
        let results = index.search("parseSnippetId", 5).await.unwrap();
        assert_eq!(
            results.len(),
            1,
            "camelCase 查询应命中 snake_case 标识符，说明 code 分词器已生效"
        );
        assert_eq!(results[0].symbol_name, "parse_snippet_id");
    }

    #[test]
    fn cjk_text_is_preserved() {
        let tokenizer = CodeTokenizer::default();
        let tokens = tokenizer.split_text_into_code_tokens("// 用户认证 user_login 逻辑");

        // 中文不能被丢弃（旧正则 [a-zA-Z0-9_]+ 会把整段中文丢光）
        assert!(
            tokens.iter().any(|t| t == "用户认证"),
            "中文连续串应整体保留为一个 token，实际: {:?}",
            tokens
        );
        // ASCII 标识符仍需按 snake_case 拆分并保留整体
        for expected in ["user", "login", "user_login"] {
            assert!(
                tokens.iter().any(|t| t == expected),
                "应包含 token {}，实际: {:?}",
                expected,
                tokens
            );
        }
        // 全部小写、无空 token、不丢字符
        assert!(tokens.iter().all(|t| !t.is_empty()), "不应出现空 token");
        assert!(
            tokens.iter().all(|t| *t == t.to_lowercase()),
            "所有 token 应为小写，实际: {:?}",
            tokens
        );
    }

    #[tokio::test]
    async fn legacy_default_tokenizer_index_is_rebuilt() {
        let dir = tempdir().unwrap();

        // 手工构造旧 schema（content/symbol_name 使用默认 TEXT 分词器）并写入一条文档，
        // 模拟升级前已在磁盘上的索引。
        {
            let mut builder = Schema::builder();
            let snippet_id_f = builder.add_text_field("snippet_id", STRING | STORED);
            let file_path_f = builder.add_text_field("file_path", STRING | STORED);
            let content_f = builder.add_text_field("content", TEXT);
            let symbol_f = builder.add_text_field("symbol_name", TEXT | STORED);
            let lang_f = builder.add_text_field("language", STRING | STORED);
            let legacy_schema = builder.build();

            let legacy = Index::create_in_dir(dir.path(), legacy_schema).unwrap();
            let mut writer = legacy.writer(50_000_000).unwrap();
            let mut doc = TantivyDocument::default();
            doc.add_text(snippet_id_f, "/tmp/legacy.rs#1#legacy_fn");
            doc.add_text(file_path_f, "/tmp/legacy.rs");
            doc.add_text(content_f, "fn legacy_fn() {}");
            doc.add_text(symbol_f, "legacy_fn");
            doc.add_text(lang_f, "rust");
            writer.add_document(doc).unwrap();
            writer.commit().unwrap();
            // 释放目录锁，便于后续整目录删除
            drop(writer);
            drop(legacy);
        }

        let index = TantivyBm25Index::open_or_create(dir.path()).unwrap();

        assert!(index.was_rebuilt(), "旧 schema（默认 TEXT）应触发重建");
        assert_eq!(
            text_indexing(index.schema(), "content").tokenizer(),
            CODE_TOKENIZER,
            "重建后的 schema 应绑定 code 分词器"
        );
        assert_eq!(
            index.document_count().await.unwrap(),
            0,
            "重建后旧文档应被清空（稀疏通道需全量回填）"
        );
    }

    #[tokio::test]
    async fn search_recovers_line_start_from_snippet_id() {
        let dir = tempdir().unwrap();
        let index = TantivyBm25Index::open_or_create(dir.path()).unwrap();

        // schema 未存 line_start 字段，真实行号只编码在 snippet_id 里
        let chunk = CodeChunk::new(
            "/tmp/a.rs",
            "fn foo() { return 1; }",
            "foo",
            "function",
            "rust",
            42,
            50,
        );
        assert_eq!(
            chunk.snippet_id,
            CodeChunk::generate_id("/tmp/a.rs", 42, "foo")
        );
        index.index_chunks(vec![chunk]).await.unwrap();
        index.commit().await.unwrap();

        let results = index.search("foo", 5).await.unwrap();
        assert_eq!(results.len(), 1, "BM25 应命中刚索引的代码块");
        assert_eq!(
            results[0].line_start, 42,
            "行号应从 snippet_id 反解得到，而不是 0"
        );
        assert_eq!(results[0].file_path, "/tmp/a.rs");
        assert_eq!(results[0].symbol_name, "foo");
    }
}
