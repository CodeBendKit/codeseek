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

/// Phase P1 交付物 2(b)：一次文件解析的完整产物 —— 符号列表 + 语言级模块元数据。
///
/// 之所以新开一个结构体而不是改 `parse_file` 的返回类型：`parse_file` 已被
/// `codegraph/parser.rs`（3 处）、`storage/incremental.rs`（2 处）、
/// `services/embedding_service.rs` 等调用点依赖，改签名会波及全仓；
/// 新结构体只服务新增的 `parse_file_with_metadata`，旧路径行为逐字节不变。
#[derive(Debug, Clone)]
pub struct ParsedFileAst {
    /// AST 符号列表（与 `parse_file` 的返回完全同一份数据）
    pub symbols: Vec<AstSymbolInstanceArc>,
    /// 源文件显式声明的包/模块路径分段（目前仅 Java 的 `package a.b.c;` 会产出）
    pub declared_package: Option<Vec<String>>,
}

impl TreeSitterParser {
    /// 创建新的TreeSitter解析器实例
    pub fn new() -> Self {
        TreeSitterParser
    }

    /// 解析文件并返回AST符号实例
    pub fn parse_file(&self, file_path: &PathBuf) -> Result<Vec<AstSymbolInstanceArc>, ParserError> {
        Ok(self.parse_file_with_metadata(file_path)?.symbols)
    }

    /// Phase P1 交付物 2(b)：解析文件并同时拿到"语言级模块元数据"。
    ///
    /// 改造前 Java 的 `package_declaration` 是空分支、包名直接丢弃，上层没有任何通路读到它；
    /// 现在通过 `AstLanguageParser::parsed_package` 透出。
    ///
    /// TODO(P1b/P2 接入点)：`codegraph::resolution::ScopeBuilder` 建 `ScopeKind::Module` 根、
    /// 以及 `ResolutionIndex::file_module` 推导 Java 文件的 module path 时，应当消费这里的
    /// `declared_package`（而不是仅按文件相对路径推导）；本阶段"只建不消费"，
    /// `CodeParser` 的 file_scopes 仍走 `parse_file`，因此这里只保证信息**可达**。
    pub fn parse_file_with_metadata(&self, file_path: &PathBuf) -> Result<ParsedFileAst, ParserError> {
        let (mut parser, _language_id) = get_ast_parser_by_filename(file_path)?;
        // 读取文件内容
        let code = std::fs::read_to_string(file_path)
            .map_err(|e| ParserError {
                message: format!("Failed to read file {}: {}", file_path.display(), e)
            })?;

        // 解析文件内容
        let symbols = parser.parse(&code, file_path);
        let declared_package = parser.parsed_package();
        Ok(ParsedFileAst {
            symbols,
            declared_package,
        })
    }
} 