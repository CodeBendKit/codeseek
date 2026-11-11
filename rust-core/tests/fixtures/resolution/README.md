# 名称解析基线 fixture（Phase P0）

这些目录是**被 parser 真实读取的源码样本**，专门用来暴露"同名即命中、命中取第一个"的
调用边解析缺陷。每个 case 都不要求能被编译器编译通过（我们只用 tree-sitter 做语法解析）。

| case 目录 | 场景 | 当前解析结果 | 正确解析结果 |
|---|---|---|---|
| `cross_file_same_name/` | 跨文件同名函数 | `call_helper → helper` 的 callee 取决于 HashMap 迭代顺序（`alpha.rs` 或 `beta.rs`），且被标记为 `is_resolved=true` | 两个候选无法由语法信息区分 → 应保留 **2 个候选**（歧义），不能宣称已解析 |
| `python_import_alias/` | `from mod_x import func as alias` 后调用 `alias()` | `alias` 不是任何函数名 → 产生 `is_resolved=false` 的未解析边，`mod_x.func` 永远没有 caller | 应沿 import 解析到 `mod_x.func` |
| `rust_qualified_path/` | `mod_a::pick(x)` | AST 层就把 name 切成裸名 `pick`、限定前缀丢失 → 与"裸名同名"同一套 first-wins | 应解析到 `mod_a::pick`（前缀参与作用域查找） |
| `python_dotted_qualified/` | `mod_y.helper(value)` | 前缀要么在 AST 丢失、要么走 `namespace.contains(prefix) \|\| name == method` 的伪校验 → 仍然 first-wins | 应解析到 `mod_y.helper` |
| `method_receiver/` | `calc.add(x)`，同时存在自由函数 `add` | receiver 在 AST 层退化成一个 `VariableUsage`，调用符号的 name 只剩 `add` → 与自由函数 `add` 无法区分 | 应按 receiver 类型解析到 `Calc::add` |
| `nested_function_caller/` | 外层函数里定义嵌套函数，嵌套函数体内发起调用；文件里另有一个闭包调用 | 第 12 行同时落在 `inner(11..13)` 与 `outer(10..16)` 两个函数区间内，caller 由"本文件函数列表行区间 first-wins"决定 —— 实测取到 `inner`，但那是 tree-sitter 先产出内层符号的**顺序巧合**，不是"最内层优先"的保证；另外 `let doubled = \|y\| y*2;` 只产出 `VariableDefinition` 而非 `FunctionDeclaration`，于是 `doubled(x)` 解析失败并造出 `namespace="unresolved"` 的幽灵节点 | caller 应记给**最内层**包含该行的函数；闭包应作为可调用实体挂到定义它的外层函数 |
| `ambiguous_trait_impl/` | `run` 有 5 份同名定义（trait 声明 + 2 个 trait impl + 2 个 inherent impl），`drive(runner: &dyn Runner)` 里调用 `runner.run()` | "本文件同名 first-wins"把它**稳定地**错接到 `driver.rs::run`（`Loudish` 的 inherent 方法，与 `dyn Runner` 无关），且 `is_resolved=true`；`runners.rs` 里的 4 份候选一条 caller 都没有 | 应按参数类型 `dyn Runner` 解析到 `runners.rs` 的 trait/impl 候选集（无法唯一确定时保留多候选），绝不该指向 `driver.rs::run` |

配套的基线测试见 `rust-core/tests/test_resolution_baseline.rs`；
每个用例都用 `// KNOWN-DEFECT(P2 修复): ...` 注释锁定"当前实际行为"，改造后需同步更新断言。

---

# Phase P2b 收口：legacy / layered **分叉**对照表（不是覆盖旧表）

`--resolver` 两挡：`legacy`（默认，五处同名 first-wins）与 `layered`（L1–L8 分层解析引擎，
`--max-candidates=8`、全层开启）。同一份 `tests/fixtures/resolution` 整目录建图的实测：

| 指标 | legacy（= P0/P1/P2a 基线，逐字节不变） | layered（P2b 新基线） | 差值来源（逐项归因） |
|---|---|---|---|
| 函数节点 | **32** | **31** | 恰好少 `python_import_alias/consumer.py:10#alias(ghost=true)` 这**一个幽灵节点**：legacy 把 `alias(value)` 判未解析 ⇒ 造 `namespace="unresolved"` 临时节点；layered 用 L4-ImportBinding 命中 `mod_x.py:4 def func` 真定义 ⇒ 不需要造幽灵。**真实定义节点两种模式恒等 30 个**（`p2b_layered_guard_table_is_the_forked_baseline` 用非幽灵子集逐条断言），`layered minus legacy = []` ⇒ 这是统计口径差异，不是节点丢失。 |
| 调用边 | **13** | **20** | 全部来自**同一调用点的候选展开**，调用点数不变（13 → 13）：`ambiguous_trait_impl/driver.rs:8 drive` 1→5 条、`cross_file_same_name/gamma.rs:6 call_helper` 1→4 条，其余 11 个调用点仍各 1 条。13+4+3=20。 |
| `is_resolved=true` | **11** | **10** | 三处变化：`consumer.py:10` 幽灵边→真定义边 **+1**；`driver.rs:8`、`gamma.rs:6` 因 `candidate_count>1` 不再谎报已解析 **−1** 各（新语义 `is_resolved = level>0 && 唯一候选`）。11−1−1+1=10。 |
| `is_resolved=false` | **2** | **10** | 上面两个歧义调用点展开出的 5+4 条候选边都记未解析（`nested.rs:20 doubled` 那条闭包幽灵边两模式都有）。2−2+9=10。 |
| 跨文件边 | 4（聚合口径可锁；逐仓会抖） | **12** | 候选展开后同一调用点同时连到本文件与其它文件的候选 ⇒ 跨文件计数上升。legacy 侧该维与哈希种子有关，见下面"legacy 抖动实测"。 |
| 同名歧义调用点（口径：callee 名在图索引里候选 >1 的**边数**） | **9** | **16** | 分子是边数不是调用点数：边 13→20，新增的 7 条边全落在 `helper`/`run` 这类歧义名上。9+7=16。 |
| 幽灵节点 | **2**（`consumer.py:10 alias`、`nested.rs:20 doubled`） | **1**（只剩 `doubled`） | `alias` 被真定义取代（同第一行）。`doubled` 是闭包 —— 当前 L1–L8 不把 `VariableDefinition` 绑到闭包体，留作 P3。 |
| 同名歧义名的候选总数（`helper/pick/add/run`） | **13** | **13** | `--resolver` 只改变"保留几个候选"，**不增删定义节点**，所以这一格两模式必须相同（守卫已断言）。 |

## 七个小场景的两挡对照（实测，`cargo test --test test_precision_report -- --nocapture`）

| case | 模式 | 节点 | 边 | resolved | unresolved | 同名歧义调用点 | 有边调用点 |
|---|---|---|---|---|---|---|---|
| `cross_file_same_name` | legacy | 5 | 3 | 3 | 0 | 3 | 3 |
| | layered | 5 | 4 | 2 | 2 | 4 | 3 |
| `python_import_alias` | legacy | 4 | 1 | 0 | 1 | 0 | 1 |
| | layered | 3 | 1 | **1** | **0** | 0 | 1 |
| `rust_qualified_path` | legacy | 4 | 2 | 2 | 0 | 2 | 2 |
| | layered | 4 | 2 | 2 | 0 | 2 | 2 |
| `python_dotted_qualified` | legacy | 3 | 1 | 1 | 0 | 1 | 1 |
| | layered | 3 | 1 | 1 | 0 | 1 | 1 |
| `method_receiver` | legacy | 4 | 1 | 1 | 0 | 1 | 1 |
| | layered | 4 | 1 | 1 | 0 | 1 | 1 |
| `nested_function_caller` | legacy | 5 | 3 | 2 | 1 | 0 | 3 |
| | layered | 5 | 3 | 2 | 1 | 0 | 3 |
| `ambiguous_trait_impl` | legacy | 7 | 2 | 2 | 0 | 2 | 2 |
| | layered | 7 | 6 | 1 | 5 | 6 | 2 |

`rust_qualified_path` / `python_dotted_qualified` / `method_receiver` 三个场景数字相同但**连的不是同一个定义**
（表里看不出来的那部分记在下面"被修正的错指"）。

## 被修正的错指（legacy → layered，逐条）

| 调用点 | legacy 连到 | layered 连到 | 依据 |
|---|---|---|---|
| `python_import_alias/consumer.py:10 alias(value)` | 本文件幽灵节点 `alias`（`is_resolved=false`） | `mod_x.py:4 func`（`is_resolved=true`） | L4 ImportBinding：`from mod_x import func as alias` |
| `rust_qualified_path/qualifier.rs:9 mod_a::pick(x)` | 猜中 `mod_b.rs:3 pick`（跨文件错指） | `mod_a.rs:3 pick` | 限定前缀参与候选过滤 |
| `python_dotted_qualified/dotted_consumer.py:10 mod_y.helper(v)` | 猜中 `other_mod.py:4 helper` | `mod_y.py:4 helper` | 点号前缀参与候选过滤 |
| `method_receiver/receiver_user.rs:11 calc.add(x)` | 猜中自由函数 `misc.rs:4 add` | 方法 `calc.rs:9 add` | receiver/enclosing-type 参与候选过滤 |

合计：**幽灵→真定义 2 条**（聚合口径 + 单场景口径各计一次），
**layered 唯一解析目标 ≠ legacy 选择 7 条**（含上面 3 条改指 + `simple_ts_project/src/main.ts:53`
`processData` 从本文件 `main.ts:48` 改指到 `dataProcessor.ts:15`）。

## legacy 抖动实测（为什么有些维度只能"记录"不能"锁定"）

legacy 的同名 first-wins 取值来自 `_build_function_name_index` 的 `HashMap` 迭代顺序，
其种子是每进程（甚至同进程内每轮建图）随机的。实测：同一份 `tests/fixtures/resolution`、
**16 轮独立全量建图**，`边数/节点数` 恒为 `13/32`（与种子无关的量），但 4 个歧义调用点的
callee 文件分布如下（一次运行的样例；换进程分布就变）：

```text
cross_file_same_name/gamma.rs:6            call_helper    -> alpha.rs x2 | beta.rs x3 | mod_y.py x4 | other_mod.py x7
python_dotted_qualified/dotted_consumer.py:10 call_dotted -> alpha.rs x2 | beta.rs x3 | mod_y.py x4 | other_mod.py x7
method_receiver/receiver_user.rs:11        use_method     -> calc.rs:9 x9 | misc.rs:4 x7
rust_qualified_path/qualifier.rs:9         call_qualified -> mod_a.rs:3 x8 | mod_b.rs:3 x8
```

**关键对照（红线 1 的证据）**：把同一个探针原样跑在 P2a 基线提交 `8e62033` 上
（`git worktree add /tmp/p2a_probe 8e62033`），得到**完全相同的抖动集合**
（同样这 4 个点 FLAKY、候选集合一模一样、`edges/nodes` 同样恒为 13/32，
分布为 `alpha x2|beta x3|mod_y x7|other_mod x4`、`calc x11|misc x5`、`mod_a x5|mod_b x11`）。
⇒ "legacy 在同名候选里选哪一个"从来就不是可锁定的性质，P2b 及其收口**没有**改变它。

因此本仓库所有"跨两次独立建图比较 legacy 产出"的断言，判据一律**去掉 callee 的身份维**
（`callee_file` + `callee_line_start`），其余维度照旧逐条锁死；被放开的那一维由
"`callee` 必须是该裸名在图索引里的候选之一（或该调用点自己的幽灵节点）"补回约束。
见 `tests/test_p1_edges_unchanged.rs` 守卫 4 的 (c)/(c2) 与
`tests/test_p2b_wiring.rs::structural_edge_multiset`。layered 是确定性链路，比较时继续用全维。

## 已知差异（整仓尺度，P3 处理）

四个整仓小项目在 layered 下**函数节点数上升**（`simple_rust` 46→49、`simple_python` 45→55、
`simple_ts` 92→121、`simple_js` 1577→1607），增加的全部是幽灵（未解析）节点：
layered 复用幽灵节点的条件是"同文件 + 同裸名 + 同调用行"，而 legacy 只要
`find_functions_by_name(name)` 能命中**任意行**造过的同名幽灵就直接复用。
⇒ 同一批未解析调用点在 layered 下拆得更细（每条调用点自己的幽灵），
`unresolved` 边数随之上升（`simple_js` 200→797 与候选展开同源）。
边覆盖（有边调用点数）两模式在四个整仓上实测**完全相等**，已由
`p2b_precision_report_legacy_vs_layered` 断言。幽灵合并粒度**已在 P3 收口 A 统一为
「同裸名复用」**（实现见 `src/codegraph/resolution/mod.rs` 的 `emit_layered_resolution`
先查再建分支：同一裸名在全仓只留一个幽灵节点，忽略文件与行号），
该口径的跨轮稳定性由 `tests/test_p3_final.rs::p3b_layered_is_deterministic_over_all_four_repos`
与 `::p3b_full_vs_incremental_are_equivalent_over_four_repos_both_modes` 守住。

