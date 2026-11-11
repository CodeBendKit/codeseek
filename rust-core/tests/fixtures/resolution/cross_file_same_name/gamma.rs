//! 场景 1（跨文件同名函数）：调用点所在文件，**本文件没有 `helper` 定义**。
//! 因此解析会落到"全局按名字找、取第一个"，`alpha.rs::helper` 与 `beta.rs::helper`
//! 之间选谁取决于 HashMap 迭代顺序 —— 这正是 P2 要消除的不确定性。

pub fn call_helper(x: i32) -> i32 {
    helper(x)
}
