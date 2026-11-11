# 调用图分层精度解析（call-graph-resolution）

> 本文是 codeseek 调用边解析引擎的**权威文档**：改造动机、九级分层口径、边的元信息、
> 幽灵节点规则、图缓存契约、唯一真源链路、已知缺口、回滚与验证。所有 `文件:行号`
> 均为 `rg -n` 实测所得，并已在本仓库 HEAD `ee91b65` 上**逐条重校**：全文 94 个唯一 `文件:行号`
> 锚点中 **70** 处原样保留（行号即真）、**23** 处按真实行号修正（多为 `993465f` 在 `parser.rs`
> 净插入 9 行造成的整体漂移，以及 `args.rs` +1 / `scope.rs` 重排造成的局部漂移）、
> **1** 处修正的是区间端点（`src/codegraph/resolution/mod.rs:579`–`584`：原端点没盖住 `site.line`）、
> **0** 处降级为无行号（每一条都能定位到唯一对应项，故无需"只留符号名不留行号"）；
> 文中引用的 8 个测试文件名当前均存在。改解析行为之前先读这里，就能改对地方。

CLI 默认 `--resolver=layered`；`--resolver=legacy` 是逐字节回滚逃生口
（`src/cli/args.rs:94` 的 `default_value = "layered"`、`src/cli/args.rs:97` 的 `pub resolver`，
默认值实现见 `src/cli/args.rs:46`）。`--max-candidates` 默认 8（`src/cli/args.rs:105`），
`--levels` 诊断开关默认空串=全层开启（`src/cli/args.rs:128`），`--force-reindex`（`src/cli/args.rs:79`）。

---

## 1. 改造前的问题（legacy 行为，各配一处代码锚点）

1. **名字相等就命中、多候选取第一个**：callee 解析走四级回退（本文件裸名 first-wins →
   全仓 `find_functions_by_name().first()` → 限定名子串），见 `src/codegraph/parser.rs:161`
   的口径注释与 `src/codegraph/parser.rs:2021`–`2024` 的实现描述。后果：同名函数必然只命中其一，
   且命中谁取决于扫描顺序。
2. **同名在名字索引里被 `or_insert` 塌缩**：全量 legacy 每轮预建 `FunctionNameIndex`，
   同名第二个及以后被 `or_insert` 静默丢弃 —— 定义在 `src/codegraph/parser.rs:1710`
   （`_build_function_name_index`），塌缩发生在本文件 `src/codegraph/parser.rs:1715`
   （`index.entry(function.name.as_str()).or_insert(function)`）。
3. **行区间 first-wins 决定 caller（闭包误归外层）**：caller 归属在"本文件函数切片"上
   按行区间取第一个覆盖它的函数，见 `src/codegraph/parser.rs:160`、`src/codegraph/parser.rs:185`。
   后果：内层闭包/嵌套函数里的调用点被误归到最外层函数。
4. **`namespace.contains(prefix)` 子串伪限定名**：限定名解析用裸子串匹配，
   实现在 `src/codegraph/parser.rs:2097`
   （`if func.namespace.contains(prefix) || func.name == method_name`），缺陷描述见
   `src/codegraph/parser.rs:2072`。后果：`"ab"` 能命中 `"abc.rs::x"`，把不相干的模块算作限定命中。

> ⚠ 这四点是 P0 记录在案的"三套并行 first-wins 实现，改一处漏一处"缺陷的根源
> （`src/codegraph/parser.rs:59`）。layered 引擎把上述判定**全部重做**，但 legacy 路径
> 被有意保留作回滚（见 §6 副本登记），其数值口径不得改变（红线）。

---

## 2. 九级分层表

枚举与置信度的唯一定义在 `src/codegraph/resolution/level.rs`（`#[repr(u8)]` 固定判别式，
`src/codegraph/resolution/level.rs:25`）；从持久化 u8 还原时未知值一律降级为 `Heuristic` 而非 panic
（`src/codegraph/resolution/level.rs:77` 的 `from_u8`）。

| 层 | 名称 | confidence | 判定依据（能否产出"唯一"） |
|----|------|-----------|----------------------------|
| 0 | `Unresolved` | 0 | 占位层，表示"没解析出来"；引擎从不消费它产边。 |
| 1 | `LocalLexical` | 98 | 调用点最内层作用域里就有一个同名可调用绑定（局部函数/参数/同作用域声明）。 |
| 2 | `EnclosingTypeMember` | 92 | 调用点最内层外围类型的成员；`self.x()`/`this.x()`。顶层自由函数无外围类型 ⇒ 空候选。 |
| 3 | `ReceiverTypeMember` | 86 | `obj.m()` 且 receiver 类型可推断（只允许形参标注 / `let v: T` / `T::new()`·`new T()`）。推不出类型 ⇒ 空候选；唯一命中但为抽象签名（trait 方法）⇒ 拒绝唯一判定并下沉。 |
| 4 | `ImportBinding` | 80 | 裸名命中 `ImportMap::unique_binding` ⇒ 展开成 (module, original_name) 并在索引里验证。别名不唯一 / 目标模块查无此名 ⇒ 空候选。 |
| 5 | `QualifiedPath` | 76 | 分量链**逐分量按分隔符边界精确**匹配（含 ImportMap 展开）。0 个或 ≥2 个模块命中 ⇒ 不唯一。 |
| 6 | `SameModule` | 65 | 同模块（同 `module_path`）内的同名候选。 |
| 7 | `GlobalUnique` | 55 | 全仓同名唯一；带 receiver 的调用点只允许成员候选。多候选 ⇒ 交 L8。 |
| 8 | `Heuristic` | 35 | **只排序，绝不作为唯一性来源**：保留全部候选并判 `Ambiguous`。 |

confidence 数值表见 `src/codegraph/resolution/level.rs:53`（`confidence()`），并有单测
`src/codegraph/resolution/level.rs:200` 钉住"单调递减且与规格一致"。各层判定依据的对照表
同时维护在 `src/codegraph/resolution/resolver.rs:13`–`28` 的模块注释。

### 支配规则 vs 排序规则（分界，务必分清）

- **支配规则（可淘汰低层候选）**：层的**先后次序本身** —— 高层一旦产出候选就不再看低层；
  加上逐层写的准入判据（带 receiver 绝不能靠裸名命中，`src/codegraph/resolution/resolver.rs:151`）。
- **排序规则（只排序、不淘汰）**：只发生在**同一层内部**，全部经
  `src/codegraph/resolution/candidate.rs:243` 的 `rank_candidates`（按模块距离 / 声明先后 / arity）。
  arity 打分 `src/codegraph/resolution/resolver.rs:795` 一带明确"只排序、不作为唯一性来源"。
- 分工总述见 `src/codegraph/resolution/resolver.rs:790`–`792`。
- **生成阶段禁止 first-wins；排序后仍 ≥2 即 `Ambiguous` 且全部保留**：
  `src/codegraph/resolution/resolver.rs:24`、`src/codegraph/resolution/resolver.rs:28`；
  发射时按候选逐个落边，`src/codegraph/resolution/mod.rs:609`–`612`（口径 A 分支的 `for candidate ... .take(max_candidates)`）。

---

## 3. 边的四元信息

`CallRelation` 定义在 `src/codegraph/types.rs:70`，四个解析标量字段：

- `resolution_level: u8`（`src/codegraph/types.rs:95`）：`ResolutionLevel as u8`，0=Unresolved .. 8=Heuristic。
- `confidence: u8`（`src/codegraph/types.rs:98`）：与 `resolution_level` 同步冗余存一份，便于落盘后不重算。
- `rank: u8`（`src/codegraph/types.rs:101`）：同层内的确定性排序位次。
- `candidate_count: u8`（`src/codegraph/types.rs:104`）：该调用点**截断前**的真实候选个数（u8 饱和到 255）。

**`is_resolved` 的派生公式**（`src/codegraph/types.rs:78`，layered 构造里落地在
`src/codegraph/types.rs:174`，判定方法见 `src/codegraph/types.rs:214`）：

```
is_resolved = (resolution_level > 0 && candidate_count == 1)
```

歧义判定：`src/codegraph/types.rs:219`（`resolution_level > 0 && candidate_count >= 2`）。

- **legacy 构造入口** `src/codegraph/types.rs:112`：四个新标量一律 `0`，`is_resolved` 由调用方按旧语义传入。
- **layered 幽灵构造入口** `src/codegraph/types.rs:187`（`layered_ghost`）：与 legacy 未解析边逐字段同形
  （level/confidence/candidate_count = 0、`is_resolved=false`）。

**`--max-candidates` 只截断落盘条数、不把歧义伪装成唯一**：超出上限的候选丢弃但仍计入
`candidate_count`，`is_resolved` 仍按真实数判定 —— 见 `src/codegraph/types.rs:150`–`151`、
`src/codegraph/resolution/resolver.rs:180`–`182`，以及状态判定必须由真实候选数决定的修复说明
`src/codegraph/resolution/resolver.rs:838`–`841`（曾有 `take(1)` 谎报 `Resolved` 的缺陷）。

---

## 4. 幽灵节点规则（layered 未解析调用）

实现集中在 `src/codegraph/resolution/mod.rs:531` 的 `emit_layered_resolution`。

- **现行行为 = 全仓同裸名复用**：发射前先"先查再建"，谓词只按裸名 + `namespace == "unresolved"`
  复用、忽略行号与文件（`src/codegraph/resolution/mod.rs:553` 说明动机、
  `src/codegraph/resolution/mod.rs:571`–`573` 是实现：
  `.find_functions_by_name(&site.callee_name).into_iter().find(|f| f.namespace == "unresolved")`）。
- **`file_path`/`line` 归属首个调用点（跨文件共享）**：查不到才新建幽灵节点，用**当前**调用点的
  `site.file`/`site.line` 造节点（`src/codegraph/resolution/mod.rs:579`–`584`）；后续同裸名调用点复用该 id，
  因此节点身份来自**首个**把它造出来的调用点，可能被跨文件共享。
- **边侧 `callee_file` 仍取 `site.file`**：`CallRelation::layered_ghost(...)` 传入
  `site.file.clone()`（`src/codegraph/resolution/mod.rs:592`–`598`），再走 `upsert_call_site_edges`
  （`src/codegraph/resolution/mod.rs:601`）。⇒ 节点全仓共用一个、但每条幽灵边的 `callee_file`
  仍是发起调用的文件，合并不引入假的跨文件边（取舍说明 `src/codegraph/resolution/mod.rs:566`–`567`）。
- 索引侧对幽灵的识别谓词（`namespace == "unresolved"`、行区间压成 `(n,n)` 的假函数节点）见
  `src/codegraph/resolution/index.rs:520`、`src/codegraph/resolution/index.rs:547`。
- **确定性前提（实测校正，勿只写 `files.sort()`）**：同一份源码两次独立建图要**逐字段相等**，
  三件事缺一不可，只靠 `files.sort()` 是被实测证伪的旧口径 ——
  1. **文件发现顺序与文件系统无关**：`scan_directory` 末尾 `files.sort()`
     （`src/codegraph/parser.rs:558`），消除 `fs::read_dir` 的物理布局抖动。
  2. **节点入图顺序不依赖 HashMap 迭代**：全量分支过去直接 `for (_, fns) in &self.file_functions`
     （`HashMap`，迭代序 = 每进程 `RandomState` 种子）把函数灌进 `code_graph`，节点/
     `function_names` 的相对顺序随之抖动。现改为**先 `keys().collect() + sort()` 再入图**
     （`src/codegraph/parser.rs:1493`–`1494` 起的 `ordered_files`），入图顺序只由文件路径决定。
  3. **作用域/caller 的 tie-break 用"匿名安全名"**：tree-sitter 给匿名闭包/函数造的名字是
     `format!("anon-{guid}", guid = Uuid::new_v4())`（`src/codegraph/treesitter/parsers/js.rs:173`
     等），每次建图都是新随机串。`innermost_scope_containing` / `scopes_containing` /
     `ScopeTree::from_items` 的反推父候选排序若拿这个 `name` 当比较键，压缩 JS 里大量
     "同 span、同 depth、同为匿名"的作用域就会随机翻转最内层选择，使 caller 在 `onClick` 与
     `Va` 之间来回跳。现统一经 `scope_sort_name`（`src/codegraph/resolution/scope.rs:33`）把
     `anon-*` 归一为空串 ⇒ 这些作用域在 name 档相等，落到 `self.scopes` 的稳定插入顺序
     （= tree-sitter 符号产出顺序）上，与进程种子彻底无关。

---

## 5. 图缓存契约

定义在 `src/storage/petgraph_storage.rs`。

- `GRAPH_SCHEMA_VERSION: u32 = 3`（`src/storage/petgraph_storage.rs:28`）。本任务**不升 schema**（保持 3）。
- **文件头 `CGSP`**：magic 4 字节 ASCII `b"CGSP"`（`src/storage/petgraph_storage.rs:50`），
  总长度 = magic(4) + `schema_version`(4, u32 小端固定宽度)（`src/storage/petgraph_storage.rs:52`），
  写入见 `src/storage/petgraph_storage.rs:58`，加载校验见 `src/storage/petgraph_storage.rs:375`。
- **解析档位戳两字段**：`resolver_mode: String`（`src/storage/petgraph_storage.rs:144`）+
  `max_candidates: u16`（`src/storage/petgraph_storage.rs:153`），落盘时经
  `PetGraphStorage::from_petgraph(..., resolver_mode, max_candidates)` 写入
  （`src/storage/petgraph_storage.rs:178`）。
- **版本/档位不符即判缓存失效、全量重建，不 panic**：
  版本门禁 `src/storage/petgraph_storage.rs:206`；档位门禁
  `validate_resolution_stamp(expected_mode, expected_max_candidates)`
  （`src/storage/petgraph_storage.rs:228`，`resolver_mode` 不符或 `max_candidates` 不符均判失效）。
  两个读者调用点：`src/codegraph/parser.rs:1542`–`1543`（`_load_existing_graph` 的期望档位取自
  `self.resolver_mode.as_str()` / `self.max_candidates`）与 `src/main.rs:103`（`init` 直接读缓存，用 `cli.resolver`/`cli.max_candidates`）。
  空串/0 档位同样过不了门禁，语义与"版本不符"一致。
- **`--levels` 的 fail-fast 约束（本任务交付物 1）**：`--levels` 会改变产边结果，但它**不在**档位戳里
  （戳只有 `resolver_mode`/`max_candidates`）。为避免"改了诊断层却复用旧图"得到混档图，采取
  **显式拒绝**而非再升 schema：`--levels` 取**非默认值**（默认=空=全层开启）且**未同时传 `--force-reindex`**
  时，CLI 在任何缓存/图读取之前就返回可读 `Err(String)` 并退出非零码。守卫本体
  `Cli::validate_levels_against_cache`（`src/cli/args.rs:186`），非默认判定
  `Cli::levels_deviates_from_default`（`src/cli/args.rs:147`，比较时忽略 bit0 `Unresolved` 占位层，
  故 `all`/`l1-l8` 判为默认、不触发）。**派发点两处，均早于缓存读取**：`src/main.rs:63`（`Cli::parse()` 之后立即调用）
  与 `src/cli/runner.rs:30`（穷尽 match 分发入口同样先调用一次）—— 二者是并行的真实入口，都必须覆盖。
  端到端行为由 `tests/test_p3_final.rs` 的 `p3b_levels_flag_fail_fast_requires_force_reindex` 钉住。
- **`--force-reindex` 逃生口**：置位时放行，强制全量重建（`src/cli/args.rs:79`）。
- **运维提示**：首次从 `GRAPH_SCHEMA_VERSION < 3` 的旧缓存升级、或首次切到 layered，都会触发一次
  全量重建（不报错、不 panic，只是变慢）；这是预期的一次性成本。

---

## 6. 唯一真源链路（每步给 文件:行号）

layered 全量建图的产边链路（定义 → 在全量主循环里的调用点）：

1. **AST 符号 → 调用点**：`call_sites_from_symbols_with_source`
   （定义 `src/codegraph/resolution/resolver.rs:391`；全量调用点 `src/codegraph/parser.rs:1875`，
   刷新链路 `src/codegraph/parser.rs:259`）。用 `_with_source` 变体：AST 的 `namespace` 对调用符号
   恒为空，限定名前缀只能从源码文本还原（口径注释 `src/codegraph/parser.rs:1871`–`1872`）。
2. **不塌缩的候选索引**：`build_resolution_index_from_graph`
   （定义 `src/codegraph/resolution/index.rs:556`；全量调用点 `src/codegraph/parser.rs:1844`，
   增量 `src/codegraph/parser.rs:898`，刷新 `src/codegraph/parser.rs:254`）。
3. **caller 归属**：`layered_caller_node`（定义 `src/codegraph/resolution/mod.rs:463`）→
   作用域树 `innermost_scope_containing`（`src/codegraph/resolution/scope.rs:472`），
   这是闭包/嵌套函数不再被误归外层的手段（`src/codegraph/resolution/mod.rs:454`）。
4. **解析**：`resolve_call`（定义 `src/codegraph/resolution/resolver.rs:1352`；带配置的
   `resolve_call_with_config` `src/codegraph/resolution/resolver.rs:1296`，全量在
   `src/codegraph/parser.rs:1890` 调用），内部 L0–L8 支配/排序分离 + `rank_candidates`
   （`src/codegraph/resolution/candidate.rs:243`）。
5. **唯一边写入入口**：`PetCodeGraph::upsert_call_site_edges`
   （定义 `src/codegraph/types.rs:551`）—— 是 legacy/layered、全量/增量**四条产边路径共用**的
   唯一入口；先删同调用点(`caller,line, callee 名族`)旧边再写新候选，按 `(caller,line)` 全删会抹掉
   同行其它调用点（缺陷记录 `src/codegraph/resolution/mod.rs:541`–`546`）。

### 有意保留的 legacy 副本（登记）

- 全量 legacy 路径仍用 `_analyze_file_calls_for_petgraph`（定义 `src/codegraph/parser.rs:1941`，
  调用点 `src/codegraph/parser.rs:1784`）。**保留理由**（见 `src/codegraph/parser.rs:71`–`76`）：
  它带 `FunctionNameIndex` 的四级 callee 回退、且幽灵会**同时落成节点**，形成 legacy 特有的
  "同名幽灵跨行复用"，与刷新档口径本就不同；强行合并会改变 legacy 数值（红线 1）。它只服务
  `--resolver=legacy` 逃生口，layered 链路完全不经过它。

---

## 7. 已知缺口（诚实记录，不美化）

- **闭包 `let f = |x| ...` 不产生可调用绑定**：layered 下这类调用点解析为 `level=0`（未解析）。
  根因是 AST 拿不到 lambda 事实 —— `ScopeKind::Closure` 目前只能由 `from_items` 手工建出
  （`src/codegraph/resolution/scope.rs:70`–`72`），真实解析不产出；Rust 闭包也不进候选索引
  （`src/codegraph/resolution/index.rs:551`）。**本轮不修**。
- **js/ts 的 `const {x} = require("m")` 不产出 `ImportDeclaration`**：CJS 解构 require 未被识别为
  import，L4 拿不到绑定，调用点下沉到更粗的层（L6/L7）。见 `src/codegraph/resolution/imports.rs:42`
  的缺口标注。**本轮不修**（口径 D）。
- **module path 缺 crate/module 根**：`derive_module_path`（`src/codegraph/resolution/index.rs:103`）
  由"文件相对仓库根"推导，不含语言意义的 crate/module 根。因此 L5 不用子串、改用
  **右对齐分段精确**比较 —— `module_chain_matches`（`src/codegraph/resolution/resolver.rs:1181`，
  每段必须是 `module_path` 按分隔符切开后的整段相等）。
- **~~压缩成单行的 JS fixture 有 caller 归属残余抖动~~（本已修复）**：此前 `simple_js_project/layered`
  的 `main.9d1c33d4.js` 在"有哈希、无图 ⇒ 全量重解析"这一轮（`p3b_incremental_rounds…` 的 R3）约 14%
  翻红：同一批幽灵边的 `caller_name` 在 `onClick` 与 `Va` 之间翻转。根因见 §4「确定性前提」第 2/3 条
  （HashMap 入图顺序 + 匿名作用域名内嵌 `Uuid::new_v4()` 参与 tie-break）。现已通过 `parser.rs:1493` 的
  `ordered_files` 排序与 `scope.rs` 的 `scope_sort_name` 归一消除，并由新增守卫
  `tests/test_p3_final.rs::p3b_layered_reparse_is_deterministic_over_all_four_repos` 锁死（该守卫在修复前
  实测 7/10 翻红、修复后恒绿）。

---

## 8. 如何回滚

`--resolver=legacy` 是逃生口：怀疑分层引擎把某仓产边弄坏时，一键退回与 P1 逐字节一致的行为
（`src/cli/args.rs:94`/`97`）。**回滚必须搭配 `--force-reindex`**：因为 `resolver_mode` 在缓存档位戳里，
mode 不符会被 `validate_resolution_stamp` 判失效并全量重建 —— 也就是说 legacy 与 layered 之间切换
**天然要求重建一次**才能得到一致结果；带 `--force-reindex` 可显式表达意图、避免"半新半旧"的混档图。
（注：仅切 mode 本身已会因档位戳不匹配触发重建；`--force-reindex` 用于在既有缓存上强制重算。）

---

## 9. 如何验证（测试文件 → 守护的不变量）

| 测试文件 | 守护的不变量 |
|----------|--------------|
| `tests/test_p1_edges_unchanged.rs` | legacy 红线：基线段 `32/13/11/2/4/9/2/13` 逐位不变（改造没动 legacy 口径）。 |
| `tests/test_resolution_baseline.rs` | 解析引擎基线 9 条：分层结果对既有基准不回退。 |
| `tests/test_resolution_engine.rs` | `resolve_call` 核心引擎：支配/排序分离、`Ambiguous` 全保留、`is_resolved` 派生。 |
| `tests/test_resolver_layers.rs` | 逐层（L1–L8）判定与 confidence；`--levels` 逐层关闭的诊断行为。 |
| `tests/test_p2b_wiring.rs` | 真源链路端到端接线：全量/刷新/增量共用发射与 `upsert` 唯一入口、结果一致。 |
| `tests/test_precision_report.rs` | 精度报告：resolved/ambiguous/unresolved 统计口径与候选数真实（不被截断谎报）。 |
| `tests/test_p3_final.rs` | 缓存档位戳端到端 + **`--levels` fail-fast**（`p3b_levels_flag_fail_fast_requires_force_reindex`，覆盖非默认报错/带 force-reindex 放行/默认放行/等价全开放行）+ 决定性（`p3b_layered_is_deterministic_over_all_four_repos`，两次 force 全量建图逐字段相等）+ **重解析路径确定性**（`p3b_layered_reparse_is_deterministic_over_all_four_repos`：同线程内多轮"有哈希、无图 ⇒ 全量重解析"交叉比较边全维度多重集，锁死 §4 的三条确定性前提；修复前实测会红）。 |
| `tests/test_graph_cache_gate.rs` | 版本/档位门禁：`CGSP` 头、`schema_version` 不符、`resolver_mode`/`max_candidates` 不符一律判失效、不 panic。 |

运行口径（红线）：`cargo build --tests` 保持 0 warning / 0 error；
`cargo test --no-fail-fast` → 0 failed；`cargo test --test test_p1_edges_unchanged -- --nocapture`
应打印 legacy `32/13/11/2/4/9/2/13`。
