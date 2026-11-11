//! Phase P1 交付物 3：解析层级（`ResolutionLevel`）与判定原因（`ResolutionReason`）。
//!
//! ## 本阶段定位：只建不消费
//! P1 只把"分层精度"这套数据模型建出来，**不接入任何调用点**。
//! P2 把 `parser.rs` / `incremental.rs` 里四处同名 first-wins 收敛为单一 resolver 时，
//! 才真正把这里的层级写进 `CallRelation`。
//!
//! ## 为什么必须 `#[repr(u8)]`
//! P2 要给 `CallRelation` 增加"解析元信息"字段，而图缓存是 **bincode** 编码
//! （`storage/petgraph_storage.rs`：`magic | schema_version(u32 LE) | bincode(PetGraphStorage)`）。
//! bincode 对枚举默认按"变体序号 + payload"编码，且 `#[repr]` 缺失时序号由编译器决定，
//! 一旦新增变体就可能整体错位。这里显式 `#[repr(u8)]` + `as_u8()/from_u8()`，
//! 是为了让 P2 落盘的是**稳定的显式数值**（而不是依赖枚举声明顺序），
//! 并把这次扩展和 `GRAPH_SCHEMA_VERSION` 1→2 绑定在同一个 commit 内。

use std::fmt::{Display, Formatter};

/// 一次 callee 解析所依据的**层级**。数值越小越可信。
///
/// 分层依据（借鉴 stack-graphs 的 scope/import 模型 —— **仅借模型，禁止引入该 crate**：
/// 它已被 GitHub 归档并挂 RUSTSEC-2026-0303）：
/// 词法作用域内的绑定 > 外围类型的成员 > receiver 类型成员 > import 显式绑定 >
/// 限定名前缀 > 同模块 > 全仓唯一同名 > 启发式兜底（即当前 first-wins 的等价物）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ResolutionLevel {
    /// 0：没有解析出任何候选。
    Unresolved = 0,
    /// 1：在调用点的词法作用域（同一文件/同一函数体及其外层）里找到同名定义。
    LocalLexical = 1,
    /// 2：在外围类型（struct/class/impl/trait）的成员里找到。
    EnclosingTypeMember = 2,
    /// 3：按 receiver 的类型定位到成员方法（`calc.add(x)` → `Calc::add`）。
    ReceiverTypeMember = 3,
    /// 4：沿 import 绑定解析（`use a::b as c; c()` / `from x import y as z` / `import a.b.C`）。
    ImportBinding = 4,
    /// 5：限定名前缀参与查找（`mod_a::pick` / `mod_y.helper`）。
    QualifiedPath = 5,
    /// 6：同一 module path 下唯一同名。
    SameModule = 6,
    /// 7：全仓同名唯一（当前 first-wins 在"只有一个候选"时与之等价）。
    GlobalUnique = 7,
    /// 8：启发式兜底 —— 名字相等就命中（当前实现的真实语义，P2 后必须显式标注）。
    Heuristic = 8,
}

impl ResolutionLevel {
    /// 该层级的置信度（0-100）。单调递减，与 `as_u8()` 的排序方向一致。
    ///
    /// 数值口径固定：`Unresolved=0 / LocalLexical=98 / EnclosingTypeMember=92 /
    /// ReceiverTypeMember=86 / ImportBinding=80 / QualifiedPath=76 / SameModule=65 /
    /// GlobalUnique=55 / Heuristic=35`。
    pub fn confidence(self) -> u8 {
        match self {
            ResolutionLevel::Unresolved => 0,
            ResolutionLevel::LocalLexical => 98,
            ResolutionLevel::EnclosingTypeMember => 92,
            ResolutionLevel::ReceiverTypeMember => 86,
            ResolutionLevel::ImportBinding => 80,
            ResolutionLevel::QualifiedPath => 76,
            ResolutionLevel::SameModule => 65,
            ResolutionLevel::GlobalUnique => 55,
            ResolutionLevel::Heuristic => 35,
        }
    }

    /// 落成稳定的 u8（供 P2 持久化）。
    /// Phase P2a：改成 `const fn` 以便 `ResolutionConfig::bit()` 在常量表达式里
    /// 构造 bitmask（`P1_LEVELS` / `L1_ONLY` / `L5_ONLY`）。**取值语义零变化**：
    /// body 仍是 `self as u8`，`#[repr(u8)]` 的判别式不变。
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// 从持久化的 u8 还原。**未知数值一律降级为 `Heuristic`** 而不是 panic：
    /// 缓存里读到未知层级应该让调用边显得"不可信"，绝不能让一次 `init` 崩溃。
    pub fn from_u8(value: u8) -> ResolutionLevel {
        match value {
            0 => ResolutionLevel::Unresolved,
            1 => ResolutionLevel::LocalLexical,
            2 => ResolutionLevel::EnclosingTypeMember,
            3 => ResolutionLevel::ReceiverTypeMember,
            4 => ResolutionLevel::ImportBinding,
            5 => ResolutionLevel::QualifiedPath,
            6 => ResolutionLevel::SameModule,
            7 => ResolutionLevel::GlobalUnique,
            // 8 以及任何未来的未知值都当作"启发式命中"，保守低可信。
            _ => ResolutionLevel::Heuristic,
        }
    }

    /// P1 实际实现了哪几层（其余是 P2 钩子）。
    pub fn is_implemented_in_p1(self) -> bool {
        matches!(
            self,
            ResolutionLevel::LocalLexical | ResolutionLevel::GlobalUnique
        )
    }
}

impl Display for ResolutionLevel {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            ResolutionLevel::Unresolved => "unresolved",
            ResolutionLevel::LocalLexical => "local-lexical",
            ResolutionLevel::EnclosingTypeMember => "enclosing-type-member",
            ResolutionLevel::ReceiverTypeMember => "receiver-type-member",
            ResolutionLevel::ImportBinding => "import-binding",
            ResolutionLevel::QualifiedPath => "qualified-path",
            ResolutionLevel::SameModule => "same-module",
            ResolutionLevel::GlobalUnique => "global-unique",
            ResolutionLevel::Heuristic => "heuristic",
        };
        write!(f, "{label}")
    }
}

/// 为什么给出这个候选 —— 供 P2 写进边元信息、也供测试断言"到底是哪条规则命中"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ResolutionReason {
    /// 名字在全仓只有一份定义。
    UniqueNameInRepo = 0,
    /// 名字在本文件内有定义（同文件词法命中）。
    DefinedInSameFile = 1,
    /// 名字落在调用点所在作用域链的某一层里。
    FoundInEnclosingScope = 2,
    /// 经由 import 的本地绑定名命中（含 `as` 别名）。
    BoundByImport = 3,
    /// 限定名前缀（模块/包）匹配到候选所在 module path。
    QualifiedPrefixMatchesModule = 4,
    /// receiver 文本与候选的类型名一致。
    ReceiverTypeNameMatches = 5,
    /// 同名多份、且支配规则无法消解（P2 才会真正产出）。
    AmbiguousSameName = 6,
    /// 没有任何候选。
    NotFound = 7,
    /// 未实现层级的占位说明（P2 钩子）。
    NotImplementedInP1 = 8,
}

impl ResolutionReason {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> ResolutionReason {
        match value {
            0 => ResolutionReason::UniqueNameInRepo,
            1 => ResolutionReason::DefinedInSameFile,
            2 => ResolutionReason::FoundInEnclosingScope,
            3 => ResolutionReason::BoundByImport,
            4 => ResolutionReason::QualifiedPrefixMatchesModule,
            5 => ResolutionReason::ReceiverTypeNameMatches,
            6 => ResolutionReason::AmbiguousSameName,
            7 => ResolutionReason::NotFound,
            8 => ResolutionReason::NotImplementedInP1,
            // 未知值保守处理为"没有候选"，与 `ResolutionLevel::from_u8` 同一取向。
            _ => ResolutionReason::NotFound,
        }
    }
}

impl Display for ResolutionReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            ResolutionReason::UniqueNameInRepo => "unique-name-in-repo",
            ResolutionReason::DefinedInSameFile => "defined-in-same-file",
            ResolutionReason::FoundInEnclosingScope => "found-in-enclosing-scope",
            ResolutionReason::BoundByImport => "bound-by-import",
            ResolutionReason::QualifiedPrefixMatchesModule => "qualified-prefix-matches-module",
            ResolutionReason::ReceiverTypeNameMatches => "receiver-type-name-matches",
            ResolutionReason::AmbiguousSameName => "ambiguous-same-name",
            ResolutionReason::NotFound => "not-found",
            ResolutionReason::NotImplementedInP1 => "not-implemented-in-p1",
        };
        write!(f, "{label}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `#[repr(u8)]` 的判别值必须与任务口径逐条一致（P2 直接落这个数值）。
    #[test]
    fn discriminants_are_the_exact_u8_contract_for_p2() {
        assert_eq!(ResolutionLevel::Unresolved.as_u8(), 0);
        assert_eq!(ResolutionLevel::LocalLexical.as_u8(), 1);
        assert_eq!(ResolutionLevel::EnclosingTypeMember.as_u8(), 2);
        assert_eq!(ResolutionLevel::ReceiverTypeMember.as_u8(), 3);
        assert_eq!(ResolutionLevel::ImportBinding.as_u8(), 4);
        assert_eq!(ResolutionLevel::QualifiedPath.as_u8(), 5);
        assert_eq!(ResolutionLevel::SameModule.as_u8(), 6);
        assert_eq!(ResolutionLevel::GlobalUnique.as_u8(), 7);
        assert_eq!(ResolutionLevel::Heuristic.as_u8(), 8);
    }

    #[test]
    fn confidence_table_matches_spec_and_is_monotonically_decreasing() {
        assert_eq!(ResolutionLevel::Unresolved.confidence(), 0);
        assert_eq!(ResolutionLevel::LocalLexical.confidence(), 98);
        assert_eq!(ResolutionLevel::EnclosingTypeMember.confidence(), 92);
        assert_eq!(ResolutionLevel::ReceiverTypeMember.confidence(), 86);
        assert_eq!(ResolutionLevel::ImportBinding.confidence(), 80);
        assert_eq!(ResolutionLevel::QualifiedPath.confidence(), 76);
        assert_eq!(ResolutionLevel::SameModule.confidence(), 65);
        assert_eq!(ResolutionLevel::GlobalUnique.confidence(), 55);
        assert_eq!(ResolutionLevel::Heuristic.confidence(), 35);

        // 层级序号递增 ⇒ 置信度严格递减（Unresolved 是 0 置信度，单独看）。
        let ordered = [
            ResolutionLevel::LocalLexical,
            ResolutionLevel::EnclosingTypeMember,
            ResolutionLevel::ReceiverTypeMember,
            ResolutionLevel::ImportBinding,
            ResolutionLevel::QualifiedPath,
            ResolutionLevel::SameModule,
            ResolutionLevel::GlobalUnique,
            ResolutionLevel::Heuristic,
        ];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].confidence() > pair[1].confidence(),
                "{}({}) 应比 {}({}) 更可信",
                pair[0],
                pair[0].confidence(),
                pair[1],
                pair[1].confidence()
            );
        }
    }

    #[test]
    fn from_u8_roundtrips_and_degrades_unknown_values() {
        for value in 0u8..=8 {
            let level = ResolutionLevel::from_u8(value);
            assert_eq!(level.as_u8(), value, "0..=8 必须无损往返");
        }
        // 未知数值不得 panic，必须降级为最低可信的启发式。
        assert_eq!(ResolutionLevel::from_u8(9), ResolutionLevel::Heuristic);
        assert_eq!(ResolutionLevel::from_u8(255), ResolutionLevel::Heuristic);
    }

    #[test]
    fn reason_roundtrips() {
        for value in 0u8..=8 {
            assert_eq!(ResolutionReason::from_u8(value).as_u8(), value);
        }
        assert_eq!(ResolutionReason::from_u8(200), ResolutionReason::NotFound);
    }

    /// P1 只落地两层，其余必须是 P2 钩子 —— 这条断言就是"只建不消费"的边界证明。
    #[test]
    fn only_two_levels_are_implemented_in_p1() {
        let implemented: Vec<ResolutionLevel> = [
            ResolutionLevel::Unresolved,
            ResolutionLevel::LocalLexical,
            ResolutionLevel::EnclosingTypeMember,
            ResolutionLevel::ReceiverTypeMember,
            ResolutionLevel::ImportBinding,
            ResolutionLevel::QualifiedPath,
            ResolutionLevel::SameModule,
            ResolutionLevel::GlobalUnique,
            ResolutionLevel::Heuristic,
        ]
        .into_iter()
        .filter(|level| level.is_implemented_in_p1())
        .collect();
        assert_eq!(
            implemented,
            vec![ResolutionLevel::LocalLexical, ResolutionLevel::GlobalUnique],
            "P1 只实现 L1 同文件词法与 L7 全仓唯一同名"
        );
    }

    #[test]
    fn display_labels_are_stable() {
        assert_eq!(ResolutionLevel::ImportBinding.to_string(), "import-binding");
        assert_eq!(ResolutionReason::BoundByImport.to_string(), "bound-by-import");
    }
}
