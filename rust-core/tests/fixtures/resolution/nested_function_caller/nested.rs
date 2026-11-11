//! 场景 6（嵌套函数 / 闭包的 caller 归属）：
//! `leaf(y)` 这一行同时落在 `inner` 和 `outer` 两个函数的行区间里，
//! 而 caller 查找是"行区间 first-wins"（`_find_caller_function_by_line`），
//! 所以归属取决于本文件函数列表的先后顺序，而不是"最内层优先"。

pub fn leaf(x: i32) -> i32 {
    x + 1
}

pub fn outer(x: i32) -> i32 {
    fn inner(y: i32) -> i32 {
        leaf(y)
    }

    inner(x)
}

pub fn closure_user(x: i32) -> i32 {
    let doubled = |y: i32| y * 2;
    doubled(x)
}
