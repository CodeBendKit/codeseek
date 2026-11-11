//! 场景 5（方法调用 receiver 丢失）：调用点文件，只调用带 receiver 的方法 `calc.add(x)`。
//!
//! 期望：解析到 `calc.rs` 里的 `Calc::add`。
//! 实际：AST 层 `rust.rs` 的 `field_expression` 分支只把字段名 `add` 写进 name，
//! receiver 退化成一个 `VariableUsage` 符号（源码里那句 `// dirty hack`），
//! 于是本文件的两个同名候选（`Calc::add` / 自由函数 `add`）之间只能"猜一个"。

use crate::calc::Calc;

pub fn use_method(calc: &Calc, x: i32) -> i32 {
    calc.add(x)
}
