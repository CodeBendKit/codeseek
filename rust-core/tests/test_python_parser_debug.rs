use std::path::PathBuf;

#[test]
fn test_python_parser_file() {
    let file_path = PathBuf::from("tests/test_repos/simple_python_project/main.py");
    
    // 直接测试 TreeSitterParser
    use codeseek::codegraph::treesitter::TreeSitterParser;
    let ts_parser = TreeSitterParser::new();
    
    match ts_parser.parse_file(&file_path) {
        Ok(symbols) => {
            println!("\nParsed {} symbols from file:", symbols.len());
            for sym in &symbols {
                let sym_read = sym.read();
                println!("  - {:?}: {}", sym_read.symbol_type(), sym_read.name());
            }
            assert!(!symbols.is_empty(), "Should have parsed symbols from file");
        }
        Err(e) => {
            panic!("Failed to parse file: {:?}", e);
        }
    }
}
