//! 场景 7（同名但应判为歧义）：`run` 有 3 份互不相同的定义
//! （trait 声明 + `Fast` impl + `Slow` impl + `Loud` inherent 方法）。
//! 正确的处理是"保留多候选 / 标记歧义"，而不是猜一个并声称已解析。

pub trait Runner {
    fn run(&self) -> i32;
}

pub struct Fast;

pub struct Slow;

impl Runner for Fast {
    fn run(&self) -> i32 {
        1
    }
}

impl Runner for Slow {
    fn run(&self) -> i32 {
        2
    }
}

pub struct Loud;

impl Loud {
    pub fn run(&self) -> i32 {
        3
    }
}
