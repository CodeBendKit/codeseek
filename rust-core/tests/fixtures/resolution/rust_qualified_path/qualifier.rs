//! 场景 3（Rust 限定名）：调用点写成限定名 `mod_a::pick(x)`。
//!
//! 期望：解析到 `mod_a.rs::pick`。
//! 实际：`src/codegraph/treesitter/parsers/rust.rs` 的 `scoped_identifier` 分支只把
//! 最后一段 `pick` 写进 `name`（限定前缀写进 `namespace`，而 `FunctionInfo`/调用边
//! 根本不看 namespace），于是这里退化得与"裸名 `pick`"完全一样。

pub fn call_qualified(x: i32) -> i32 {
    mod_a::pick(x)
}
