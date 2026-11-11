//! 场景 1（跨文件同名函数）：第二份同名 `helper` 定义。
//! 它和 `alpha.rs::helper` 对 `gamma.rs::call_helper` 来说完全无法区分。

pub fn helper(x: i32) -> i32 {
    x * 2
}

pub fn other_beta_work(x: i32) -> i32 {
    helper(x)
}
