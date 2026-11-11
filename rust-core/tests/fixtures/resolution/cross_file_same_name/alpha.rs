//! 场景 1（跨文件同名函数）：`helper` 在本文件定义一次。
//! 同名的另一份定义在 `beta.rs`，而真正调用 `helper` 的 `call_helper` 在 `gamma.rs`。

pub fn helper(x: i32) -> i32 {
    x + 1
}

/// 本文件内就有一份 `helper`：按"先查本文件"的规则这次是对的，
/// 用作对照组，证明 first-wins 只在"本文件没有定义"时才暴露出不确定性。
pub fn local_helper_call(x: i32) -> i32 {
    helper(x)
}
