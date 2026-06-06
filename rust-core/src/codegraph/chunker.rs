//! Chunk extraction from Tree-sitter AST.
//!
//! This module converts AST symbol instances (functions, structs, etc.)
//! into `CodeChunk` objects that are used by both the dense vector index
//! and the sparse BM25 index.

use crate::storage::traits_bm25::CodeChunk;
use crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstanceArc;
use crate::codegraph::treesitter::structs::SymbolType;
use std::path::Path;

/// Extract code chunks from parsed AST symbols.
///
/// Each AST symbol (FunctionDeclaration, StructDeclaration, etc.) becomes
/// one CodeChunk. The chunk's content is the raw code text of that symbol.
///
/// # Arguments
/// * `file_path` - The path to the source file
/// * `symbols` - Parsed AST symbol instances from Tree-sitter
/// * `language` - The programming language identifier
///
/// # Returns
/// A vector of CodeChunk objects, one per AST symbol.
pub fn extract_chunks(
    file_path: &Path,
    symbols: &[AstSymbolInstanceArc],
    language: &str,
) -> Vec<CodeChunk> {
    symbols
        .iter()
        .filter_map(|sym| {
            // Only include StructDeclaration and FunctionDeclaration symbols
            let sym_inner = sym.read();
            let sym_ref = sym_inner.as_ref();

            match sym_ref.symbol_type() {
                SymbolType::StructDeclaration | SymbolType::FunctionDeclaration => {}
                _ => return None,
            }

            // Extract code content from file through SymbolInformation
            let symbol_info = sym_ref.symbol_info_struct();
            let code_block = match symbol_info.get_content_from_file_blocked() {
                Ok(content) if !content.is_empty() => content,
                _ => return None,
            };

            // Extract line numbers from full_range (0-indexed, convert to 1-indexed for CodeChunk)
            let line_start = sym_ref.full_range().start_point.row + 1;
            let line_end = sym_ref.full_range().end_point.row + 1;

            Some(CodeChunk::new(
                file_path.to_string_lossy().into_owned(),
                code_block,
                sym_ref.name().to_string(),
                format!("{:?}", sym_ref.symbol_type()),
                language.to_string(),
                line_start,
                line_end,
            ))
        })
        .collect()
}

/// Extract chunks from a raw source file using the provided symbols.
///
/// This is a convenience wrapper that delegates to `extract_chunks`.
pub fn extract_chunks_from_symbols(
    file_path: impl AsRef<Path>,
    symbols: &[AstSymbolInstanceArc],
    language: &str,
) -> Vec<CodeChunk> {
    extract_chunks(file_path.as_ref(), symbols, language)
}
