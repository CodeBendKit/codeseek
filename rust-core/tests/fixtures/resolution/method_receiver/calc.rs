//! 场景 5（方法调用 receiver 丢失）：`Calc::add` 是"带 receiver 的方法"。
//! 与之同名的自由函数 `add` 在 `misc.rs` 里，用来检验 receiver 是否真的参与了消歧。

pub struct Calc {
    pub base: i32,
}

impl Calc {
    pub fn add(&self, x: i32) -> i32 {
        self.base + x
    }

    pub fn value(&self) -> i32 {
        self.base
    }
}
