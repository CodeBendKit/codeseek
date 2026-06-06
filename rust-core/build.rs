fn main() {
    // 为tree-sitter语言支持编译
    println!("cargo:rerun-if-changed=build.rs");
    
    // 确保tree-sitter语言库被正确链接
    println!("cargo:rustc-link-lib=tree-sitter");
} 