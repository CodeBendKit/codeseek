use std::fmt::Display;
use std::path::PathBuf;
use std::error::Error;

use tracing::error;

use crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstanceArc;
use crate::codegraph::treesitter::language_id::LanguageId;


pub(crate) mod python;
pub(crate) mod rust;
#[cfg(test)]
#[path = "../../../tests/parsers/mod.rs"]
mod tests;
mod utils;
mod java;
pub(crate) mod cpp;
pub(crate) mod ts;
mod js;
pub(crate) mod go;


#[derive(Debug, PartialEq, Eq)]
pub struct ParserError {
    pub message: String,
}

impl Display for ParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Parser error: {}", self.message)
    }
}

impl Error for ParserError {}

pub trait AstLanguageParser: Send {
    fn parse(&mut self, code: &str, path: &PathBuf) -> Vec<AstSymbolInstanceArc>;

    /// Phase P1 交付物 2(b)：源文件显式声明的模块/包路径分段（Java 的 `package a.b.c;`）。
    ///
    /// 默认 `None` —— 其余 6 个语言 parser 没有"文件级包声明"这一概念（Rust 的 module path
    /// 由文件路径推导，Python/TS 同理），因此无需改动它们的实现。
    /// 由 `TreeSitterParser::parse_file_with_metadata` 透出给上层，供 `resolution::ScopeBuilder`
    /// / `ResolutionIndex::file_module` 推导 module path。
    fn parsed_package(&self) -> Option<Vec<String>> {
        None
    }
}

fn internal_error<E: Display>(err: E) -> ParserError {
    let err_msg = err.to_string();
    error!(err_msg);
    ParserError {
        message: err_msg.into(),
    }
}

pub(crate) fn get_ast_parser(language_id: LanguageId) -> Result<Box<dyn AstLanguageParser + 'static>, ParserError> {
    match language_id {
        LanguageId::Rust => {
            let parser = rust::RustParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::Python => {
            let parser = python::PythonParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::Java => {
            let parser = java::JavaParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::Cpp => {
            let parser = cpp::CppParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::TypeScript => {
            let parser = ts::TSParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::JavaScript => {
            let parser = js::JSParser::new()?;
            Ok(Box::new(parser))
        }
        LanguageId::TypeScriptReact => {
            let parser = ts::TSParser::new()?; //quick fix untill we have a dedicated parser for TypeScriptReact
            Ok(Box::new(parser))
        }
        LanguageId::Go => {
            let parser = go::GoParser::new()?;
            Ok(Box::new(parser))
        }
        other => Err(ParserError {
            message: "Unsupported language id: ".to_string() + &other.to_string()
        }),
    }
}


pub fn get_ast_parser_by_filename(filename: &PathBuf) -> Result<(Box<dyn AstLanguageParser + 'static>, LanguageId), ParserError> {
    let suffix = filename.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let maybe_language_id = get_language_id_by_filename(filename);
    match maybe_language_id {
        Some(language_id) => {
            let parser = get_ast_parser(language_id)?;
            Ok((parser, language_id))
        }
        None => Err(ParserError { message: format!("not supported {}", suffix) }),
    }
}

pub fn get_language_id_by_filename(filename: &PathBuf) -> Option<LanguageId> {
    let suffix = filename.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match suffix.as_str() {
        "cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "hh" => Some(LanguageId::Cpp),
        "inl" | "inc" | "tpp" | "tpl" => Some(LanguageId::Cpp),
        "py" | "py3" | "pyx" => Some(LanguageId::Python),
        "java" => Some(LanguageId::Java),
        "rs" => Some(LanguageId::Rust),
        "ts" => Some(LanguageId::TypeScript),
        "tsx" => Some(LanguageId::TypeScriptReact),
        "js" | "jsx" => Some(LanguageId::JavaScript),
        "go" => Some(LanguageId::Go),
        _ => None
    }
}

