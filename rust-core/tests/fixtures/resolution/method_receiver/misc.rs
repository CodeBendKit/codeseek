//! 场景 5（方法调用 receiver 丢失）：与 `Calc::add` **同名**的自由函数。
//! 因为调用符号里 receiver 已被丢弃，`calc.add(x)` 与 `add(x)` 在解析器眼里一模一样。

pub fn add(x: i32) -> i32 {
    x + 100
}
