pub mod language_id;
pub mod parsers;
pub mod structs;
pub mod ast_instance_structs;
pub mod skeletonizer;
pub mod file_ast_markup;

use std::path::PathBuf;
use crate::codegraph::treesitter::parsers::{get_ast_parser_by_filename, ParserError};

pub use language_id::LanguageId;
pub use structs::*;
pub use ast_instance_structs::*;
pub use skeletonizer::*;
pub use file_ast_markup::*;

/// TreeSitter解析器的主要接口
pub struct TreeSitterParser;

impl TreeSitterParser {
    /// 创建新的TreeSitter解析器实例
    pub fn new() -> Self {
        TreeSitterParser
    }

    /// 解析文件并返回AST符号实例
    pub fn parse_file(&self, file_path: &PathBuf) -> Result<Vec<AstSymbolInstanceArc>, ParserError> {
        let (mut parser, _language_id) = get_ast_parser_by_filename(file_path)?;
        // 读取文件内容
        let code = std::fs::read_to_string(file_path)
            .map_err(|e| ParserError {
                message: format!("Failed to read file {}: {}", file_path.display(), e)
            })?;
        
        // 解析文件内容
        let symbols = parser.parse(&code, file_path);
        Ok(symbols)
    }
} 