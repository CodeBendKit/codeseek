//! Tantivy-based BM25 full-text search index for code.
//!
//! Reuses Tree-sitter parsed code chunks (AST symbols: functions, structs, etc.)
//! as the indexing unit — NOT whole files.

use crate::storage::traits_bm25::{CodeChunk, TextSearchResult, TextSearchProvider};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use regex::Regex;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, STORED, STRING, TEXT};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{Index, IndexWriter, TantivyDocument, Term};
use tokio::sync::Mutex;

// ─────────────────────────────────────────────
// Custom Code Tokenizer
// ─────────────────────────────────────────────

/// Tantivy tokenizer for code identifiers.
/// Splits snake_case and CamelCase, emits sub-tokens + original.
#[derive(Clone)]
pub struct CodeTokenizer;

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream<'a>;

    fn token_stream<'a>(&mut self, text: &'a str) -> Self::TokenStream<'a> {
        let tokens = split_text_into_code_tokens(text);
        CodeTokenStream::new(tokens)
    }
}

/// Split text into code-aware tokens (snake_case + CamelCase).
fn split_text_into_code_tokens(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let word_re = Regex::new(r"[a-zA-Z0-9_]+").unwrap();

    for m in word_re.find_iter(text) {
        let raw = m.as_str();
        let sub_tokens = split_camel_snake(raw);
        result.extend(sub_tokens);
    }
    result
}

fn split_camel_snake(word: &str) -> Vec<String> {
    let mut sub_tokens: Vec<String> = Vec::new();

    for segment in word.split('_').filter(|s| !s.is_empty()) {
        let chars: Vec<char> = segment.chars().collect();
        let _len = chars.len();
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

    // Emit full original (lowercased) if we have sub-tokens
    if sub_tokens.len() > 1 {
        sub_tokens.push(word.to_lowercase());
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
        if self.pos <= self.tokens.len() {
            &self.tokens[self.pos - 1]
        } else {
            &self.eof_token
        }
    }

    fn token_mut(&mut self) -> &mut Token {
        if self.pos <= self.tokens.len() {
            &mut self.tokens[self.pos - 1]
        } else {
            &mut self.eof_token
        }
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
    snippet_id_field: tantivy::schema::Field,
    file_path_field: tantivy::schema::Field,
    content_field: tantivy::schema::Field,
    symbol_name_field: tantivy::schema::Field,
    language_field: tantivy::schema::Field,
}

impl TantivyBm25Index {
    /// Open an existing index or create a new one at the given directory.
    pub fn open_or_create<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        let snippet_id_field = schema_builder.add_text_field("snippet_id", STRING | STORED);
        let file_path_field = schema_builder.add_text_field("file_path", STRING | STORED);
        let content_field = schema_builder.add_text_field("content", TEXT);
        let symbol_name_field = schema_builder.add_text_field("symbol_name", TEXT | STORED);
        let language_field = schema_builder.add_text_field("language", STRING | STORED);

        let schema = schema_builder.build();

        // Create or open the index directory
        let dir_path = dir.as_ref();
        std::fs::create_dir_all(dir_path).map_err(|e| anyhow!("Failed to create Tantivy index dir {:?}: {}", dir_path, e))?;

        let index = if dir_path.read_dir().map_or(false, |mut dir| dir.next().is_some()) {
            // Try to open existing (may have content)
            Index::open_in_dir(dir_path).unwrap_or_else(|_| {
                // Corrupt or incompatible — create fresh
                Index::create_in_dir(dir_path, schema.clone())
                    .expect("Failed to create fresh Tantivy index")
            })
        } else {
            // Empty or new directory
            Index::create_in_dir(dir_path, schema.clone())
                .map_err(|e| anyhow!("Failed to create Tantivy index at {:?}: {}", dir_path, e))?
        };

        // Register the custom code tokenizer
        // Note: LowercaseFilter was removed in tantivy 0.22.
        // split_camel_snake() already outputs lowercase tokens, so no filter needed.
        let code_analyzer = tantivy::tokenizer::TextAnalyzer::builder(CodeTokenizer).build();
        index.tokenizers().register("code", code_analyzer);

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
        })
    }

    /// Helper: extract a stored string field from a TantivyDocument.
    fn get_stored_string(doc: &TantivyDocument, field: tantivy::schema::Field, schema: &Schema) -> String {
        schema
            .get_field_entry(field)
            .is_stored()
            .then(|| {
                doc.get_first(field)
                    .and_then(|val| match val {
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
            let writer = w.as_mut().ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;

            for chunk in chunks {
                let mut doc = TantivyDocument::default();
                doc.add_text(snippet_id_f, &chunk.snippet_id);
                doc.add_text(file_path_f, &chunk.file_path);
                doc.add_text(content_f, &chunk.content);
                doc.add_text(symbol_f, &chunk.symbol_name);
                doc.add_text(lang_f, &chunk.language);
                writer.add_document(doc).map_err(|e| anyhow!("Failed to add document: {}", e))?;
            }

            writer.commit().map_err(|e| anyhow!("Tantivy commit failed: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Tantivy spawn_blocking panicked: {:?}", e))?
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<TextSearchResult>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self.index.reader().map_err(|e| anyhow!("Tantivy reader error: {}", e))?;
        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(
            &self.index,
            vec![self.content_field, self.symbol_name_field],
        );

        // Sanitize query: wrap in quotes for exact phrase, catch parse errors
        let parsed_query = query_parser.parse_query(query).unwrap_or_else(|_| {
            let sanitized = query.replace(['"', '*', '?'], "");
            if sanitized.contains(' ') {
                query_parser.parse_query(&format!("\"{}\"", sanitized)).unwrap_or_else(|_| {
                    query_parser.parse_query(&sanitized).unwrap()
                })
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

            results.push(TextSearchResult {
                snippet_id: Self::get_stored_string(&doc, self.snippet_id_field, &self.schema),
                score: _score,
                file_path: Self::get_stored_string(&doc, self.file_path_field, &self.schema),
                symbol_name: Self::get_stored_string(&doc, self.symbol_name_field, &self.schema),
                language: Self::get_stored_string(&doc, self.language_field, &self.schema),
                line_start: 0, // Not stored in Tantivy; filled by dense result during fusion
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
            let writer = w.as_mut().ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;
            writer.delete_term(term);
            writer.commit().map_err(|e| anyhow!("Tantivy commit after delete failed: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow!("Tantivy spawn_blocking panicked: {:?}", e))?
    }

    async fn commit(&self) -> Result<()> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let mut w = writer.blocking_lock();
            let writer = w.as_mut().ok_or_else(|| anyhow!("Tantivy writer is dropped"))?;
            writer.commit().map_err(|e| anyhow!("Tantivy commit failed: {}", e))?;
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
        let reader = self.index.reader().map_err(|e| anyhow!("Tantivy reader error: {}", e))?;
        let searcher = reader.searcher();
        Ok(searcher.num_docs() as usize)
    }
}
