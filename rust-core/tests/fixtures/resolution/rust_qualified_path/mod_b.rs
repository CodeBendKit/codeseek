//! 场景 3（Rust 限定名）：`pick` 的第二份同名定义（逻辑上属于模块 `mod_b`）。

pub fn pick(x: i32) -> i32 {
    x * 7
}

pub fn pick_in_this_file(x: i32) -> i32 {
    pick(x)
}
