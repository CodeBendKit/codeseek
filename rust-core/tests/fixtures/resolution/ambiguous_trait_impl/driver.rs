//! 场景 7（同名但应判为歧义）：调用点。`slow.run()` 在语法上无法确定类型，
//! 但即使不看类型，`run` 也有 3 个候选 —— 正确行为是保留 3 个候选（或标记歧义），
//! 而当前实现只产出一条 `is_resolved=true` 的边。

use crate::runners::Runner;

pub fn drive(runner: &dyn Runner) -> i32 {
    runner.run()
}

pub fn drive_loud(loud: &Loudish) -> i32 {
    loud.run()
}

pub struct Loudish;

impl Loudish {
    pub fn run(&self) -> i32 {
        4
    }
}
