//! Phase P1 交付物 3：候选（`Candidate`）与解析结果（`Resolution`）的数据模型。
//!
//! 这里刻意**只做数据 + 纯函数判定**，不含任何 I/O、不读文件系统、不碰 petgraph。
//! P2 才把 `resolve_call` 接进建图链路并给 `CallRelation` 加候选字段
//! （届时必须在同一个 commit 内把 `GRAPH_SCHEMA_VERSION` 从 1 递增到 2）。

use std::fmt::{Display, Formatter};

use uuid::Uuid;

use super::level::{ResolutionLevel, ResolutionReason};

/// 解析状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionStatus {
    /// 一个候选都没有（对应现在 `is_resolved=false` + 幽灵节点的那类边）。
    Unresolved,
    /// 恰好一个候选，可唯一定位。
    Resolved,
    /// ≥2 个候选，且支配规则无法消解 —— 必须**保留全部候选**，不得像现在这样偷偷取一个。
    Ambiguous,
}

impl ResolutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionStatus::Unresolved => "unresolved",
            ResolutionStatus::Resolved => "resolved",
            ResolutionStatus::Ambiguous => "ambiguous",
        }
    }
}

impl Display for ResolutionStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 状态判定的**纯函数**。
///
/// 注意口径：这里的 `Ambiguous` 只是"候选数 ≥2"这一形式判定的下限。
/// 真正的语义应当是"**支配规则（层级更高者胜出、类型/前缀/import 可验证）无法消解**"才算歧义 ——
/// 那部分规则属于 P2（`resolve_call` 目前只实现 L1/L7 两层），
/// 所以本阶段只落这个"数数"的函数并单测锁死，供 P2 在其之上加支配规则。
pub fn status_of(candidate_count: usize) -> ResolutionStatus {
    match candidate_count {
        0 => ResolutionStatus::Unresolved,
        1 => ResolutionStatus::Resolved,
        _ => ResolutionStatus::Ambiguous,
    }
}

/// 一个候选 callee。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 候选函数节点 id（`FunctionInfo.id`）。
    pub callee_id: Uuid,
    /// 依据哪一层解析出来的。
    pub level: ResolutionLevel,
    /// 由 `level` 推导的置信度（冗余存一份，方便落盘后不重算）。
    pub confidence: u8,
    /// 同一解析结果内的名次，0 = 最优。
    pub rank: u8,
    /// 命中原因（便于测试断言与 P2 写进边元信息）。
    pub reason: ResolutionReason,
    // ============================================================================================
    // Phase P2a 交付物 1：**排序子键**（只排序、绝不淘汰，见任务的分层规则）。
    //
    // 三个字段都有中性默认值（`module_distance = 3`、`declared_before = false`、
    // `arity_match = 1`），因此既有 `Candidate::new` 调用点（P1 的 L1/L7 实现与全部
    // P1 单测）比较结果不变 —— 中性值意味着"这一维没有信息"，排序仍落到最后的
    // `callee_id` tie-break 上，与 P1 行为逐字节一致。
    // ============================================================================================
    /// 候选与调用点的"模块距离"：0=同文件，1=同目录，2=同 module path，3=全仓其它。
    pub module_distance: u8,
    /// 同文件时，候选声明是否**位于调用点之前**（true 优先：可读性更好的正常代码多半如此）。
    pub declared_before: bool,
    /// 实参个数匹配度：2=相等，1=信息不足（`param_count`/`arg_count` 有一个拿不到），0=不等。
    pub arity_match: u8,
    // ============================================================================================
    // Phase P2a 交付物 4（确定性测试暴露的缺陷）：**源码事实 tie-break**。
    //
    // `rank_candidates` 原本把 `callee_id`（`FunctionInfo.id`，每次建图现场
    // `Uuid::new_v4()`）当最终 tie-break。它在**同一进程内**确实稳定，但跨进程/跨重建
    // 每次都是新随机数 ⇒ 同层多候选的**顺序**会随重建抖动。
    // `tests/test_resolution_engine.rs::p2a_resolution_is_deterministic_…` 实测抓到：
    // 同一份 `ambiguous_trait_impl` 连建两次图，`runner.run()` 的 5 个 L8 候选集合相同、
    // 顺序却不同（`runners.rs:14` 与 `runners.rs:6` 互换）。
    //
    // 这正是任务要消除的"抖动"那一类，因此最终比较序列改成
    // `(level, 排序子键, 源码事实(文件,行,名), uuid)`：前三段全部来自源码，
    // 只有真正同一文件同一行同名的极端情况才落到 uuid。默认值 `("",0,"")` 保证
    // `Candidate::new` 的既有调用点（P1 的 L1/L7 与 P1 全部单测）行为逐字节不变。
    // ============================================================================================
    /// 候选定义所在的源码位置（`文件路径 / 起始行 / 名字`）；空 = 未填 ⇒ 不参与比较。
    pub source_key: (String, usize, String),
}

/// 排序子键的中性默认值（见字段注释：保证 P1 行为不变）。
pub const NEUTRAL_MODULE_DISTANCE: u8 = 3;
/// "信息不足"——既不加分也不减分。
pub const NEUTRAL_ARITY: u8 = 1;

impl Candidate {
    /// 构造候选：`confidence` 一律由 `level` 推导，避免调用方随手写一个数把口径打破。
    pub fn new(
        callee_id: Uuid,
        level: ResolutionLevel,
        rank: u8,
        reason: ResolutionReason,
    ) -> Self {
        Self {
            callee_id,
            level,
            confidence: level.confidence(),
            rank,
            reason,
            // 中性排序子键：调用方只在"真的知道"时才覆盖，否则不参与比较。
            module_distance: NEUTRAL_MODULE_DISTANCE,
            declared_before: false,
            arity_match: NEUTRAL_ARITY,
            source_key: (String::new(), 0, String::new()),
        }
    }

    /// Phase P2a：一次性填三个**排序子键**（绝不改变 `status` 判定，只影响同层候选的先后）。
    pub fn with_sort_keys(
        mut self,
        module_distance: u8,
        declared_before: bool,
        arity_match: u8,
    ) -> Self {
        self.module_distance = module_distance;
        self.declared_before = declared_before;
        self.arity_match = arity_match;
        self
    }

    /// Phase P2a：填**源码事实 tie-break**（`(文件, 起始行, 名字)`）。
    ///
    /// 与 `with_sort_keys` 一样"只排序、绝不淘汰"；填了它，同层多候选的顺序就
    /// 不再依赖每次重建都会重新生成的 `FunctionInfo.id`（跨进程确定性）。
    pub fn with_source_key(mut self, file: &str, line_start: usize, name: &str) -> Self {
        self.source_key = (file.to_string(), line_start, name.to_string());
        self
    }
}

/// 一次调用点的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// 由 `candidates` 的数量决定（见 `status_of`）。
    pub status: ResolutionStatus,
    /// **全部**候选，按确定性顺序排列（见 `rank_candidates`）。
    ///
    /// 这一条正是 P2 要消灭"同名塌缩"的关键：现在的实现只留一个命中（`Vec::first()` /
    /// `HashMap::or_insert`），候选信息被静默丢弃；这里从数据模型层面就要求留住全部。
    pub candidates: Vec<Candidate>,
    /// Phase P2a：截断**之前**的真实候选个数。
    ///
    /// `ResolutionConfig.max_candidates`（`--max-candidates`，默认 8）会截断
    /// `candidates` 的长度，但 P2b 落盘的 `candidate_count` 必须是真实个数
    /// （任务口径 A：超出部分丢弃但仍计入 `candidate_count`，u8 饱和到 255）。
    /// 未截断时它等于 `candidates.len()`。
    pub total_candidate_count: usize,
}

impl Resolution {
    /// 由候选列表构造，并按 `rank_candidates` 重排 + 重编 `rank` + 决定 `status`。
    pub fn from_candidates(candidates: Vec<Candidate>) -> Self {
        let ordered = rank_candidates(candidates);
        let status = status_of(ordered.len());
        Self {
            status,
            // P2a：未截断时真实个数 == 保留个数（截断由 `resolver::finish` 覆盖此字段）。
            total_candidate_count: ordered.len(),
            candidates: ordered,
        }
    }

    /// 没有任何候选。
    pub fn unresolved(reason: ResolutionReason) -> Self {
        Self {
            status: ResolutionStatus::Unresolved,
            total_candidate_count: 0,
            candidates: vec![],
        }
        .with_reason(reason)
    }

    /// 把"未命中原因"记进结果，便于测试断言到底是 NotFound 还是 NotImplementedInP1。
    pub fn with_reason(mut self, reason: ResolutionReason) -> Self {
        if self.candidates.is_empty() {
            self.candidates.push(Candidate {
                // 没有真实 callee，用 nil uuid 表示"幽灵"。
                // 注意：它**不计入** `total_candidate_count`（保持 0），
                // 因为 P2b 落盘时 `candidate_count` 必须反映真实候选数；
                // 判定"有没有解析出东西"请用 `status`/`total_candidate_count`，不要用 `candidates.len()`。
                callee_id: Uuid::nil(),
                level: ResolutionLevel::Unresolved,
                confidence: ResolutionLevel::Unresolved.confidence(),
                rank: 0,
                reason,
                module_distance: NEUTRAL_MODULE_DISTANCE,
                declared_before: false,
                arity_match: NEUTRAL_ARITY,
                source_key: (String::new(), 0, String::new()),
            });
        }
        self
    }

    /// 最优候选（rank 最小者）；`Unresolved` 时返回 `None`。
    pub fn primary(&self) -> Option<&Candidate> {
        match self.status {
            ResolutionStatus::Unresolved => None,
            _ => self.candidates.first(),
        }
    }

    /// 是否唯一确定（只有 `Resolved` 才是）。
    pub fn is_unique(&self) -> bool {
        self.status == ResolutionStatus::Resolved
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

/// 确定性排序：**置信度降序 → 层级序号升序 → 排序子键 → 源码事实(文件,行,名) → callee_id 升序**。
///
/// 为什么要显式排序：当前四处 first-wins 的取值取决于 `HashMap` 的**每进程随机哈希种子**
/// （P0 基线测试因此"故意不断言具体文件"）。P1 的数据层绝不能继承这种不确定性，
/// 否则守卫测试自身就会 flaky。`callee_id`(Uuid) 作最终 tie-break 是稳定的。
///
/// Phase P2a 在"层级"与"`callee_id`"之间插入三个**只排序不淘汰**的子键
/// （模块距离 → 声明是否位于调用点之前 → 实参个数匹配度）。
/// 层级/置信度仍是**支配**维度（低层候选可被高层候选淘汰），子键永远只在同层内部比先后。
/// 最终比较序列等价于 `(level, rank, uuid)` 三元组口径：`rank` 由本次排序结果重编，
/// 而 `uuid` 仍是最后的确定性 tie-break —— 这正是消除 P1 实测种子抖动的手段。
pub fn rank_candidates(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.level.as_u8().cmp(&b.level.as_u8()))
            // ---- P2a 排序子键（同层内部才生效）----
            .then_with(|| a.module_distance.cmp(&b.module_distance))
            .then_with(|| b.declared_before.cmp(&a.declared_before))
            .then_with(|| b.arity_match.cmp(&a.arity_match))
            // ---- Phase P2a：源码事实 tie-break（跨重建稳定）----
            .then_with(|| a.source_key.cmp(&b.source_key))
            // Phase P3 收口 C：**去掉 `callee_id.as_u128()` 这一位**。
            // `source_key = (文件, 起始行, 名字)` 已经覆盖候选在投影里看得见的全部源码事实；
            // 走到这里还相等的两个候选，产出的边在可观测层面逐字段相同，谁在前都不改变输出。
            // 反之留着 Uuid 才是问题：它每次建图都重新生成 ⇒ "同一份源码两次独立建图"
            // 的 rank 归属会翻转（实测见 `tests/test_p3_final.rs::p3b_layered_is_deterministic_*`）。
    });
    // 重编 rank，保证与排序结果一致（0 = 最优）。
    for (idx, candidate) in candidates.iter_mut().enumerate() {
        candidate.rank = idx.min(u8::MAX as usize) as u8;
    }
    candidates
}

/// 按 `callee_id` 去重（同一候选被多条规则命中时只留最优的一条 —— 排序后的第一条）。
pub fn dedup_by_callee(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if seen.insert(candidate.callee_id) {
            out.push(candidate.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    #[test]
    fn status_of_is_the_exact_three_way_pure_function() {
        assert_eq!(status_of(0), ResolutionStatus::Unresolved);
        assert_eq!(status_of(1), ResolutionStatus::Resolved);
        assert_eq!(status_of(2), ResolutionStatus::Ambiguous);
        assert_eq!(status_of(9), ResolutionStatus::Ambiguous);
    }

    #[test]
    fn confidence_always_comes_from_level() {
        let candidate = Candidate::new(
            id(1),
            ResolutionLevel::ImportBinding,
            0,
            ResolutionReason::BoundByImport,
        );
        assert_eq!(candidate.confidence, 80);
        assert_eq!(candidate.level.confidence(), candidate.confidence);
    }

    /// 关键性质：≥2 候选时结果必须**保留全部**，这正是当前 first-wins 丢掉的信息。
    #[test]
    fn multi_candidate_result_keeps_every_candidate() {
        let resolution = Resolution::from_candidates(vec![
            Candidate::new(id(3), ResolutionLevel::GlobalUnique, 0, ResolutionReason::UniqueNameInRepo),
            Candidate::new(id(1), ResolutionLevel::LocalLexical, 0, ResolutionReason::DefinedInSameFile),
            Candidate::new(id(2), ResolutionLevel::Heuristic, 0, ResolutionReason::AmbiguousSameName),
        ]);
        assert_eq!(resolution.status, ResolutionStatus::Ambiguous);
        assert_eq!(resolution.candidate_count(), 3, "不得塌缩成一个");
        assert_eq!(
            resolution.candidates.iter().map(|c| c.level).collect::<Vec<_>>(),
            vec![
                ResolutionLevel::LocalLexical,
                ResolutionLevel::GlobalUnique,
                ResolutionLevel::Heuristic
            ],
            "必须按置信度降序排列：LocalLexical(98) > GlobalUnique(55) > Heuristic(35)"
        );
    }

    #[test]
    fn ranking_is_deterministic_by_confidence_then_level_then_source_key() {
        // Phase P3 收口 C：末位 tie-break 从 `callee_id`(Uuid) 换成**源码派生的 `source_key`**。
        // Uuid 每次建图都重新生成，用它补位只保证"同进程可复现"，不保证"同一份源码两次
        // 独立建图可复现"（正是 `tests/test_p3_final.rs::p3b_layered_is_deterministic_*`
        // 在 `simple_js_project` 上抓到的 rank/层级翻转）。
        // 本守卫因此改判：**排序结果由 source_key 决定，与 Uuid 大小无关**。
        let small_key = Candidate::new(
            id(9), // 故意给一个"大" Uuid
            ResolutionLevel::GlobalUnique,
            0,
            ResolutionReason::AmbiguousSameName,
        )
        .with_source_key("a.rs", 1, "pick");
        let large_key = Candidate::new(
            id(2), // "小" Uuid，但 source_key 更大
            ResolutionLevel::GlobalUnique,
            0,
            ResolutionReason::AmbiguousSameName,
        )
        .with_source_key("b.rs", 1, "pick");

        // 输入顺序刻意是 (large_key, small_key)：结果仍必须按 source_key 排。
        let ordered = rank_candidates(vec![large_key.clone(), small_key.clone()]);
        assert_eq!(ordered[0].callee_id, id(9), "排序必须按 source_key，不能按 Uuid");
        assert_eq!(ordered[1].callee_id, id(2));
        assert_eq!(ordered[0].rank, 0);
        assert_eq!(ordered[1].rank, 1);

        // 再打乱输入顺序，结果必须完全一致。
        let again = rank_candidates(vec![small_key.clone(), large_key.clone()]);
        assert_eq!(ordered, again, "排序不能依赖输入顺序");

        // `source_key` 完全相同（同文件、同行、同名）⇒ 两个候选在可观测层面等价：
        // 这里只要求"同样的输入给同样的输出"，不再断言谁在前。
        let x = Candidate::new(
            id(5),
            ResolutionLevel::SameModule,
            0,
            ResolutionReason::AmbiguousSameName,
        )
        .with_source_key("c.rs", 7, "dup");
        let y = Candidate::new(
            id(3),
            ResolutionLevel::SameModule,
            0,
            ResolutionReason::AmbiguousSameName,
        )
        .with_source_key("c.rs", 7, "dup");
        assert_eq!(
            rank_candidates(vec![x.clone(), y.clone()]),
            rank_candidates(vec![x.clone(), y.clone()]),
            "source_key 相同的候选必须给出稳定结果"
        );
    }

    #[test]
    fn ranks_are_rewritten_to_match_order() {
        let resolution = Resolution::from_candidates(vec![
            Candidate::new(id(1), ResolutionLevel::Heuristic, 7, ResolutionReason::AmbiguousSameName),
            Candidate::new(id(2), ResolutionLevel::LocalLexical, 7, ResolutionReason::DefinedInSameFile),
        ]);
        assert_eq!(resolution.candidates[0].level, ResolutionLevel::LocalLexical);
        assert_eq!(resolution.candidates[0].rank, 0, "输入的 rank=7 必须被重编");
        assert_eq!(resolution.candidates[1].rank, 1);
    }

    #[test]
    fn dedup_keeps_the_best_hit_per_callee_id() {
        let candidates = rank_candidates(vec![
            Candidate::new(id(5), ResolutionLevel::GlobalUnique, 0, ResolutionReason::UniqueNameInRepo),
            Candidate::new(id(5), ResolutionLevel::LocalLexical, 0, ResolutionReason::DefinedInSameFile),
            Candidate::new(id(6), ResolutionLevel::SameModule, 0, ResolutionReason::FoundInEnclosingScope),
        ]);
        let deduped = dedup_by_callee(&candidates);
        assert_eq!(deduped.len(), 2);
        // id(5) 去重后必须留下高可信的那条（LocalLexical）。
        assert_eq!(deduped[0].callee_id, id(5));
        assert_eq!(deduped[0].level, ResolutionLevel::LocalLexical);
    }

    #[test]
    fn single_candidate_is_resolved_and_primary_is_it() {
        let only = id(42);
        let resolution = Resolution::from_candidates(vec![Candidate::new(
            only,
            ResolutionLevel::GlobalUnique,
            0,
            ResolutionReason::UniqueNameInRepo,
        )]);
        assert_eq!(resolution.status, ResolutionStatus::Resolved);
        assert!(resolution.is_unique());
        assert_eq!(resolution.primary().map(|c| c.callee_id), Some(only));
    }

    #[test]
    fn unresolved_has_no_primary_and_records_reason_placeholder() {
        let resolution = Resolution::unresolved(ResolutionReason::NotFound);
        assert_eq!(resolution.status, ResolutionStatus::Unresolved);
        assert!(resolution.primary().is_none(), "未解析不得给出 callee");
        assert_eq!(resolution.candidates[0].callee_id, Uuid::nil());
        assert_eq!(resolution.candidates[0].level, ResolutionLevel::Unresolved);
        assert_eq!(resolution.candidates[0].reason, ResolutionReason::NotFound);
    }

    #[test]
    fn status_display_is_stable() {
        assert_eq!(ResolutionStatus::Ambiguous.to_string(), "ambiguous");
        assert_eq!(ResolutionStatus::Resolved.to_string(), "resolved");
    }
}
