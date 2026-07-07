//! JavaScript compiled/obfuscated code detection module.
//!
//! Multi-level short-circuit hybrid heuristic architecture, detecting from shallow to deep:
//! - L1: Zero-cost fingerprint matching (SIMD-optimized string containment check)
//! - L2: Structural density analysis (average line length)
//! - L3: Identifier semantic analysis (short identifier ratio)

use regex::Regex;
use std::sync::LazyLock;

// Pre-compiled regex, compiled once at first use with zero overhead on subsequent calls
static IDENTIFIER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[a-zA-Z_$][a-zA-Z0-9_$]*").unwrap()
});

/// Detection result classification.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeType {
    /// Source code (normal, hand-written code)
    SourceCode,
    /// Compiled/minified/obfuscated code
    CompiledCode,
    /// Empty file
    Empty,
}

/// Detection report.
#[derive(Debug, Clone)]
pub struct DetectionReport {
    pub code_type: CodeType,
    pub reason: String,
}

/// Main detection function.
///
/// Analyzes JS code content and returns a detection report.
/// Uses short-circuit strategy: L1 → L2 → L3, returns immediately on match.
pub fn analyze_js_code(code: &str) -> DetectionReport {
    // 0. Empty file handling
    if code.trim().is_empty() {
        return DetectionReport {
            code_type: CodeType::Empty,
            reason: "File content is empty".to_string(),
        };
    }

    // 1. L1: Zero-cost fingerprint matching
    // Uses str::contains, which internally uses memchr with SIMD acceleration
    let fingerprints = [
        "__webpack_require__",
        "webpackChunk",
        "webpackJsonp",
        "_classCallCheck",
        "_interopRequireDefault",
        "//# sourceMappingURL=",
        "var _0x",           // OB obfuscator hex-encoded identifiers
        "function _0x",      // OB obfuscator function definitions
        "(function(_0x",     // OB obfuscator IIFE pattern
        "while (!![]) {",    // Control flow flattening characteristic
        "eval(function(p,a,c,k,e,d)",  // Dean Edwards packer
        "eval(function(p,a,c,k,e,r)",
        "[][(![]+",          // JSFuck characteristic
    ];

    for fp in fingerprints.iter() {
        if code.contains(fp) {
            return DetectionReport {
                code_type: CodeType::CompiledCode,
                reason: format!("Hit compiler/obfuscator fingerprint: '{}'", fp),
            };
        }
    }

    // 2. L2: Structural density analysis (average line length)
    let total_chars = code.len();
    let non_empty_lines: Vec<&str> = code.lines().filter(|l| !l.trim().is_empty()).collect();

    if non_empty_lines.is_empty() {
        return DetectionReport {
            code_type: CodeType::Empty,
            reason: "No valid code lines".to_string(),
        };
    }

    let line_count = non_empty_lines.len();
    let avg_line_length = total_chars as f64 / line_count as f64;

    // Empirical threshold: average line length > 400 chars indicates minified code
    if avg_line_length > 400.0 {
        return DetectionReport {
            code_type: CodeType::CompiledCode,
            reason: format!("Code density too high, average line length: {:.0} chars", avg_line_length),
        };
    }

    // 3. L3: Identifier semantic analysis
    let identifiers: Vec<&str> = IDENTIFIER_RE
        .find_iter(code)
        .map(|m| m.as_str())
        .collect();

    // Too few identifiers for statistical significance, skip analysis
    if identifiers.len() < 50 {
        return DetectionReport {
            code_type: CodeType::SourceCode,
            reason: "Too few identifiers, skipping semantic analysis".to_string(),
        };
    }

    let short_identifiers_count = identifiers.iter().filter(|id| id.len() <= 2).count();
    let short_ratio = short_identifiers_count as f64 / identifiers.len() as f64;

    // Empirical threshold: short identifier ratio > 55% indicates obfuscated code
    if short_ratio > 0.55 {
        return DetectionReport {
            code_type: CodeType::CompiledCode,
            reason: format!("Short identifier ratio too high: {:.2}%", short_ratio * 100.0),
        };
    }

    // Default: classified as source code
    DetectionReport {
        code_type: CodeType::SourceCode,
        reason: "Meets source code characteristics".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_code() {
        let code = "function add(a, b) { return a + b; }\nlet result = add(1, 2);\nconsole.log(result);";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::SourceCode);
    }

    #[test]
    fn test_webpack_code() {
        let code = "const __webpack_require__ = () => { /* ... */ };\n";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_minified_code() {
        // 模拟极长的压缩代码（多行重复，每行都很长）
        let line = "function a(b,c){return b+c}function d(e,f){return e*f}let g=a(1,2);let h=d(3,4);";
        let code = line.repeat(100);
        let report = analyze_js_code(&code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_obfuscator_code() {
        let code = "var _0x1a2b = ['hello']; console.log(_0x1a2b[0]);";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_empty_code() {
        let report = analyze_js_code("   \n  \n  ");
        assert_eq!(report.code_type, CodeType::Empty);
    }

    #[test]
    fn test_short_code_skipped() {
        let code = "let x = 1;";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::SourceCode);
    }

    #[test]
    fn test_control_flow_flattening() {
        let code = r#"
function something() {
    while (!![]) {
        console.log("hello");
    }
}
"#;
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_source_map_url() {
        let code = "// Some code\n//# sourceMappingURL=app.js.map\n";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_webpack_chunk() {
        let code = r#"webpackChunkapp = function() { console.log("chunk"); }"#;
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_dean_edwards_packer() {
        let code = "eval(function(p,a,c,k,e,d){while(c--)if(k[c])p=p.replace(new RegExp('\\\\b'+c.toString(a)+'\\\\b','g'),k[c]);return p}('...',1,1,'x'.split('|'),0,{}))";
        let report = analyze_js_code(code);
        assert_eq!(report.code_type, CodeType::CompiledCode);
    }

    #[test]
    fn test_webpack_bundle_detection() {
        // Use the real webpack bundle file from test fixtures
        let code = include_str!("../../tests/test_repos/simple_js_project/main.9d1c33d4.js");
        let report = analyze_js_code(code);

        // This file is a webpack-bundled, minified React production build.
        // It should be detected as CompiledCode (not indexed).
        assert_eq!(
            report.code_type,
            CodeType::CompiledCode,
            "Expected webpack bundle to be detected as CompiledCode, but got {:?}: {}",
            report.code_type,
            report.reason
        );

        // Verify the detection reason mentions a fingerprint match
        let has_fingerprint = report.reason.contains("fingerprint")
            || report.reason.contains("compiler")
            || report.reason.contains("obfuscat");
        let has_density = report.reason.contains("density");
        assert!(
            has_fingerprint || has_density,
            "Detection reason should mention fingerprint or density, got: {}",
            report.reason
        );
    }
}
