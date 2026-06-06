#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::codegraph::treesitter::language_id::LanguageId;
    use crate::codegraph::treesitter::parsers::AstLanguageParser;
    use crate::codegraph::treesitter::parsers::java::JavaParser;
    use crate::codegraph::treesitter::parsers::tests::{base_declaration_formatter_test, base_parser_test, base_skeletonizer_test};

    const MAIN_JAVA_CODE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/java/main.java"));
    const MAIN_JAVA_SYMBOLS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/java/main.java.json"));

    const PERSON_JAVA_CODE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/java/person.java"));
    const PERSON_JAVA_SKELETON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/java/person.java.skeleton"));
    const PERSON_JAVA_DECLS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cases/java/person.java.decl_json"));

    #[test]
    fn parser_test() {
        let mut parser: Box<dyn AstLanguageParser> = Box::new(JavaParser::new().expect("JavaParser::new"));
        let path = PathBuf::from("file:///main.java");
        base_parser_test(&mut parser, &path, MAIN_JAVA_CODE, MAIN_JAVA_SYMBOLS);
    }

    #[test]
    fn skeletonizer_test() {
        let mut parser: Box<dyn AstLanguageParser> = Box::new(JavaParser::new().expect("JavaParser::new"));
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases/java/person.java");
        assert!(file.exists());

        base_skeletonizer_test(&LanguageId::Java, &mut parser, &file, PERSON_JAVA_CODE, PERSON_JAVA_SKELETON);
    }

    #[test]
    fn declaration_formatter_test() {
        let mut parser: Box<dyn AstLanguageParser> = Box::new(JavaParser::new().expect("JavaParser::new"));
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cases/java/person.java");
        assert!(file.exists());
        base_declaration_formatter_test(&LanguageId::Java, &mut parser, &file, PERSON_JAVA_CODE, PERSON_JAVA_DECLS);
    }
}
