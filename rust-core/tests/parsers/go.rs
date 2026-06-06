#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::codegraph::treesitter::language_id::LanguageId;
    use crate::codegraph::treesitter::parsers::AstLanguageParser;
    use crate::codegraph::treesitter::parsers::go::GoParser;
    use crate::codegraph::treesitter::parsers::tests::{base_declaration_formatter_test, base_parser_test, base_skeletonizer_test};
    use crate::codegraph::treesitter::structs::SymbolType;

    const MAIN_GO_CODE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/main.go"));
    const MAIN_GO_SYMBOLS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/main.go.json"));

    const SHAPE_GO_CODE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/shape.go"));
    const SHAPE_GO_SKELETON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/shape.go.skeleton"));
    const SHAPE_GO_DECLS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/shape.go.decl_json"));

    #[test]
    fn parser_test() {
        let code = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/main.go"));
        let symbols_str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/main.go.json"));
        let path = std::path::PathBuf::from("/main.go");
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        
        base_parser_test(&mut parser, &path, code, symbols_str);
    }

    #[test]
    fn skeletonizer_test() {
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases/go/shape.go");
        assert!(file.exists());

        base_skeletonizer_test(&LanguageId::Go, &mut parser, &file, SHAPE_GO_CODE, SHAPE_GO_SKELETON);
    }

    #[test]
    fn declaration_formatter_test() {
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases/go/shape.go");
        assert!(file.exists());
        base_declaration_formatter_test(&LanguageId::Go, &mut parser, &file, SHAPE_GO_CODE, SHAPE_GO_DECLS);
    }

    #[test]
    fn basic_functionality_test() {
        // 基本功能测试：验证GoParser能够成功解析Go代码并提取符号
        let code = r#"
package main

import "fmt"

type Point struct {
    X int
    Y int
}

func NewPoint(x int, y int) Point {
    return Point{X: x, Y: y}
}

func main() {
    p := NewPoint(1, 2)
    fmt.Println(p.X, p.Y)
}
"#;
        let path = std::path::PathBuf::from("/test.go");
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        
        let symbols = parser.parse(code, &path);
        
        // 验证解析出了一些符号
        assert!(!symbols.is_empty(), "GoParser should extract some symbols from Go code");
        
        // 验证符号类型
        let symbol_types: Vec<_> = symbols.iter()
            .map(|s| s.read().symbol_type())
            .collect();
        
        // 应该包含导入、结构体、函数等声明
        let has_import = symbol_types.iter().any(|t| matches!(t, SymbolType::ImportDeclaration));
        let has_struct = symbol_types.iter().any(|t| matches!(t, SymbolType::StructDeclaration));
        let has_function = symbol_types.iter().any(|t| matches!(t, SymbolType::FunctionDeclaration));
        
        assert!(has_import, "Should have import declarations");
        assert!(has_struct, "Should have struct declarations"); 
        assert!(has_function, "Should have function declarations");
        
        println!("GoParser successfully parsed {} symbols", symbols.len());
    }

    #[test]
    fn debug_shape_parsing() {
        let code = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/go/shape.go"));
        let path = std::path::PathBuf::from("/shape.go");
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        
        let symbols = parser.parse(code, &path);
        
        println!("Parsed {} symbols from shape.go:", symbols.len());
        for (i, symbol) in symbols.iter().enumerate() {
            let symbol_info = symbol.read().symbol_info_struct();
            println!("  {}: {:?} - {}", i, symbol_info.symbol_type, symbol_info.name);
        }
        
        // Check if we have struct declarations
        let struct_symbols: Vec<_> = symbols.iter()
            .filter(|s| s.read().symbol_type() == SymbolType::StructDeclaration)
            .collect();
        
        println!("Found {} struct declarations:", struct_symbols.len());
        for struct_symbol in struct_symbols {
            let symbol_info = struct_symbol.read().symbol_info_struct();
            println!("  - {}: {:?}", symbol_info.name, symbol_info.symbol_type);
            println!("    Full range: {:?}", symbol_info.full_range);
            println!("    Declaration range: {:?}", symbol_info.declaration_range);
            println!("    Definition range: {:?}", symbol_info.definition_range);
        }
        
        assert!(!symbols.is_empty(), "Should parse some symbols");
    }

    #[test]
    fn debug_import_parsing() {
        let code = r#"
package main

import (
	"fmt"
)

type Point struct {
    X int
    Y int
}
"#;
        let path = std::path::PathBuf::from("/test.go");
        let mut parser: Box<dyn AstLanguageParser> = Box::new(GoParser::new().expect("GoParser::new"));
        
        let symbols = parser.parse(code, &path);
        
        println!("Parsed {} symbols:", symbols.len());
        for (i, symbol) in symbols.iter().enumerate() {
            let symbol_info = symbol.read().symbol_info_struct();
            println!("  {}: {:?} - {}", i, symbol_info.symbol_type, symbol_info.name);
            println!("    Full range: {:?}", symbol_info.full_range);
        }
        
        // Check if we have import declarations
        let import_symbols: Vec<_> = symbols.iter()
            .filter(|s| s.read().symbol_type() == SymbolType::ImportDeclaration)
            .collect();
        
        println!("Found {} import declarations:", import_symbols.len());
        for import_symbol in import_symbols {
            let symbol_info = import_symbol.read().symbol_info_struct();
            println!("  - {}: {:?}", symbol_info.name, symbol_info.symbol_type);
            println!("    Full range: {:?}", symbol_info.full_range);
        }
        
        assert!(!symbols.is_empty(), "Should parse some symbols");
    }

    #[test]
    fn debug_tree_sitter_output() {
        let code = r#"
package main

import (
	"fmt"
)

type Point struct {
    X int
    Y int
}
"#;
        
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();
        
        println!("Root node: {} ({} children)", root.kind(), root.child_count());
        
        // Print the tree structure
        fn print_tree(node: tree_sitter::Node, depth: usize) {
            let indent = "  ".repeat(depth);
            println!("{}{}: {:?} ({} children)", indent, node.kind(), node.range(), node.child_count());
            
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    print_tree(child, depth + 1);
                }
            }
        }
        
        print_tree(root, 0);
    }
} 