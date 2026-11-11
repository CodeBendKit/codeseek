//! Phase P1 交付物 3：全仓函数索引（`ResolutionIndex` / `FunctionTag`）。
//!
//! ## 它要替掉的是什么
//! `parser.rs:1174` `_build_function_name_index` 现在是
//! ```ignore
//! type FunctionNameIndex<'a> = HashMap<&'a str, &'a FunctionInfo>;
//! index.entry(function.name.as_str()).or_insert(function);   // parser.rs:1179
//! ```
//! 一个名字只留**第一个**函数 —— 4 组同名歧义名（helper/pick/add/run，共 13 个候选）
//! 在图上只体现为 11 条已解析边，候选信息被静默丢弃（P0 基线已量化过这个差值）。
//! `by_name: HashMap<String, Vec<FunctionTag>>` 就是把"不塌缩"这件事在数据层先做实：
//! **P1 里这个索引就已经能给出全部候选**，P2 只需要改成消费它。
//!
//! ## 关联键的硬事实（P2 必须知道）
//! `FunctionInfo.id` 是在 `parser.rs:197` / `parser.rs:499` 现场 `Uuid::new_v4()` 生成的，
//! 与 AST 符号的 `guid` **没有任何对应关系**。因此图节点与 AST 事实（如形参个数）
//! 只能用 `(file_path, line_start)` 关联 —— 两处 `line_start` 都等于
//! `full_range.start_point.row + 1`，口径一致，可安全对齐。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::codegraph::types::{FunctionInfo, PetCodeGraph};

use super::FileScopeCtx;

/// 一个函数节点在解析索引里的画像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionTag {
    /// 图节点 id（`FunctionInfo.id`）。
    pub id: Uuid,
    pub file: PathBuf,
    /// 推导出的 module path（见 [`derive_module_path`]）；无法确定时为**空串**。
    pub module_path: String,
    /// 裸函数名。
    pub name: String,
    /// `module_path` + 分隔符 + `name`。
    pub qualified_name: String,
    /// 形参个数（来自 AST `FunctionDeclaration.args.len()`）。
    /// `None` = 没能与 AST 事实对齐（函数不是从 FunctionDeclaration 来的，或行号对不上）。
    pub param_count: Option<usize>,
    /// 语言（`FunctionInfo.language`，当前是 `_detect_language` 产出的小写字符串）。
    pub language: String,
    /// 1-based 起始行 —— 与 AST 事实对齐用的关联键之一。
    pub line_start: usize,
    // ============================================================================================
    // Phase P2a 交付物 1：分层判定需要的两条"成员身份"事实。
    //
    // `enclosing_type` 是 L2/L3 的核心：`obj.m()` 只能命中**成员**，绝不能命中同名的
    // 自由函数（`method_receiver` 场景里 `calc.add(x)` 与 `misc.rs::add` 的区别全在这里）。
    // `has_body` 是 L3 的"抽象签名拒判"：AST 里没有 impl→trait 反向链接，命中一条
    // 无体签名（trait 方法）时无法枚举实现者，只能拒绝唯一判定并下沉。
    //
    // 数据来源：`FileScopeCtx::callable_facts()`（作用域树，只读符号），
    // 与 `param_count` 同样靠 `(file, line_start)` 回填，**不新增任何序列化字段**。
    // ============================================================================================
    /// 所属类型作用域（`impl`/`class`/`struct`/`trait` 块名）；顶层自由函数为 `None`。
    /// `None` 同时表示"该文件没有 ctx，无从判断"—— 调用方必须按信息不足处理。
    pub enclosing_type: Option<String>,
    /// 是否带函数体（`false` = trait/抽象签名）。没有 AST 事实时保守取 `true`
    /// （等价于"不参与 L3 拒判"，不会凭空制造候选，也不会凭空否掉候选）。
    pub has_body: bool,
}

/// 从作用域树里抽出来的"可调用事实"，用于给 `FunctionTag.param_count` 回填。
///
/// 由 `FileScopeCtx::callable_facts()` 产出（见 `mod.rs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableFact {
    pub name: String,
    pub line_start: usize,
    pub param_count: Option<usize>,
    /// 所属类型作用域名（若能确定），P2 的 L2/L3 需要。
    pub enclosing_type: Option<String>,
    /// Phase P2a：是否带函数体（`false` = trait/抽象签名，L3 据此拒绝唯一判定）。
    pub has_body: bool,
}

/// module path 的分隔符：Rust 用 `::`，其余语言用 `.`。
pub fn module_separator(language: &str) -> &'static str {
    match language {
        "rust" => "::",
        _ => ".",
    }
}

/// 由文件路径推导 module path。
///
/// 规则（尽量保守、可解释，**不猜**）：
/// 1. 必须有仓库根才能推导：`root == None` 或 `file` 不在 `root` 之下 ⇒ **返回空串**
///    （任务口径："无法确定时降级为空串并记 TODO"），并由调用方计数；
/// 2. 相对路径去掉扩展名；Java 若给了 `declared_package`（P1 交付物 2(b) 的通路）则**以包名为准**，
///    不再拼接目录（Java 的目录结构只是惯例，不是语义）；
/// 3. 其余语言 = `目录段 + 文件 stem`，按语言分隔符连接（Rust `::`，其它 `.`）；
/// 4. Java 无 package 声明时只给目录链、不拼文件 stem（类名才是命名空间，而类名不在路径里）。
///
/// TODO(P2)：真正的 module path 需要读构建配置（`Cargo.toml` 的 crate 名、
/// `go.mod` 的 module 路径、Java 的 source root、tsconfig 的 `baseUrl`）。
/// P1 只做"相对根的路径推导 + Java 包名"这一层，绝不假装能解析工程配置。
pub fn derive_module_path(
    file: &Path,
    root: Option<&Path>,
    language: &str,
    declared_package: Option<&[String]>,
) -> String {
    if let Some(pkg) = declared_package {
        if !pkg.is_empty() && language == "java" {
            return pkg.join(".");
        }
    }

    // 没有根 / 文件不在根之下 ⇒ 不推导（空串 + 由上层计数告警）。
    let Some(base) = root else {
        return String::new();
    };
    let Ok(relative) = file.strip_prefix(base) else {
        return String::new();
    };

    let mut segments: Vec<String> = relative
        .parent()
        .map(|dir| {
            dir.components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .filter(|s| !s.is_empty() && s != ".")
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
        if !stem.is_empty() {
            segments.push(stem.to_string());
        }
    }

    // Java 无 package 时只给目录链。
    if language == "java" {
        segments.pop();
    }

    segments.join(module_separator(language))
}

/// 全仓解析索引。
#[derive(Debug, Clone, Default)]
pub struct ResolutionIndex {
    /// 裸名 → **全部**同名函数。绝不做 `or_insert` / `first()` 塌缩。
    by_name: HashMap<String, Vec<FunctionTag>>,
    /// 限定名 → 函数。同名限定冲突时保留**确定性最优的一条**，
    /// 并把冲突记进 `qualified_collisions`（不静默丢弃）。
    by_qualified: HashMap<String, FunctionTag>,
    /// module path → 该模块内全部函数。
    by_module: HashMap<String, Vec<FunctionTag>>,
    /// Phase P2a：`(类型名, 裸函数名)` → 该类型的同名成员（L2/L3 的查询入口，**不塌缩**）。
    by_type: HashMap<(String, String), Vec<FunctionTag>>,
    /// 文件 → module path。
    file_module: HashMap<PathBuf, String>,
    /// `by_qualified` 发生过覆盖的限定名（P2 决定是否要把它升级为多候选）。
    qualified_collisions: Vec<String>,
    /// 无法推导 module path 的文件数（root 缺失或路径不在 root 下）。
    files_without_module_path: usize,
    total_tags: usize,
}

impl ResolutionIndex {
    /// 建索引。
    ///
    /// - `functions`：全仓 `FunctionInfo`（顺序任意 —— 内部会重排成确定性顺序）
    /// - `root`：仓库根（`None` 时 module path 退化为空串并计数）
    /// - `packages`：文件 → Java `package` 声明分段（来自 `TreeSitterParser::parse_file_with_metadata`）
    /// - `callables`：文件 → AST 侧的"可调用事实"（用于回填 `param_count`）
    pub fn build(
        functions: &[FunctionInfo],
        root: Option<&Path>,
        packages: &HashMap<PathBuf, Vec<String>>,
        callables: &HashMap<PathBuf, Vec<CallableFact>>,
    ) -> Self {
        let mut index = Self::default();

        // 先把输入按 (file, line_start, name, ...) 排成确定顺序：
        // `functions` 来自 HashMap 迭代，顺序随机；不排序的话 by_qualified 的
        // "冲突保留哪一条"就会随进程变化（当前 first-wins 的病灶之一）。
        //
        // Phase P3 收口 C：**末位 tie-break 不再是 `id.as_u128()`**。
        // `FunctionInfo.id` 是每次建图重新生成的 v4，用它只能保证"同一进程内可复现"，
        // 保证不了"同一份源码两次独立建图可复现"（实测见 `tag_source_key` 的注释）。
        // 现在键覆盖到投影看得见的全部源码派生字段；键完全相同的两个函数在可观测层面
        // 等价，谁先谁后都不再改变输出。
        let mut ordered: Vec<&FunctionInfo> = functions.iter().collect();
        ordered.sort_by(|a, b| {
            (
                &a.file_path,
                a.line_start,
                &a.name,
                a.line_end,
                &a.namespace,
                &a.qualified_name,
                &a.module_path,
            )
                .cmp(&(
                    &b.file_path,
                    b.line_start,
                    &b.name,
                    b.line_end,
                    &b.namespace,
                    &b.qualified_name,
                    &b.module_path,
                ))
        });

        for function in ordered {
            let declared = packages.get(&function.file_path).map(Vec::as_slice);
            let module_path = derive_module_path(
                &function.file_path,
                root,
                &function.language,
                declared,
            );
            let separator = module_separator(&function.language);
            let qualified_name = if module_path.is_empty() {
                function.name.clone()
            } else {
                format!("{module_path}{separator}{}", function.name)
            };

            // (file, line_start) 关联 AST 事实 —— 见本文件开头"关联键的硬事实"。
            let fact = callables
                .get(&function.file_path)
                .and_then(|facts| {
                    facts
                        .iter()
                        .find(|fact| fact.line_start == function.line_start && fact.name == function.name)
                });

            let tag = FunctionTag {
                id: function.id,
                file: function.file_path.clone(),
                module_path: module_path.clone(),
                name: function.name.clone(),
                qualified_name: qualified_name.clone(),
                param_count: fact.and_then(|f| f.param_count),
                language: function.language.clone(),
                line_start: function.line_start,
                // Phase P2a：成员身份 + 是否有函数体，同样靠 (file, line_start) 回填。
                // 没有 ctx（文件被 MD5 跳过、或本轮没解析过）⇒ `None` / `true`，
                // 上层 L2/L3 必须把它当"信息不足"下沉，不得凭空造候选。
                enclosing_type: fact.and_then(|f| f.enclosing_type.clone()),
                has_body: fact.map(|f| f.has_body).unwrap_or(true),
            };

            index.total_tags += 1;
            index
                .by_name
                .entry(function.name.clone())
                .or_default()
                .push(tag.clone());
            if !module_path.is_empty() {
                index
                    .by_module
                    .entry(module_path.clone())
                    .or_default()
                    .push(tag.clone());
            }
            // Phase P2a：类型成员索引（`Calc::add` 这类"L2/L3 只在该类型内部找"的查询入口）。
            // 排序后存放，保证 `candidates_of_type` 与 HashMap 种子无关。
            if let Some(type_name) = tag.enclosing_type.as_ref() {
                if !type_name.is_empty() {
                    index
                        .by_type
                        .entry((type_name.clone(), function.name.clone()))
                        .or_default()
                        .push(tag.clone());
                }
            }
            match index.by_qualified.get(&qualified_name) {
                Some(_) => {
                    // 已有同限定名 —— 保留排序靠前的（确定性），但**记下冲突**。
                    index.qualified_collisions.push(qualified_name);
                }
                None => {
                    index.by_qualified.insert(qualified_name, tag);
                }
            }
            // 文件 → module path：同文件多个函数只记第一次（不覆盖），
            // 空串按"文件"为单位计数一次，而不是按函数。
            let seen_file = index.file_module.contains_key(&function.file_path);
            if !seen_file && module_path.is_empty() {
                index.files_without_module_path += 1;
            }
            index
                .file_module
                .entry(function.file_path.clone())
                .or_insert_with(|| module_path.clone());
        }

        index
    }

    /// 某个裸名的**全部**候选（按插入时的确定性顺序）。
    pub fn candidates_by_name(&self, name: &str) -> &[FunctionTag] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// 恰好一个候选时才返回；多个候选返回 `None`（**这就是"不塌缩"的体现**）。
    pub fn unique_by_name(&self, name: &str) -> Option<&FunctionTag> {
        let hits = self.candidates_by_name(name);
        if hits.len() == 1 {
            hits.first()
        } else {
            None
        }
    }

    pub fn by_name(&self) -> &HashMap<String, Vec<FunctionTag>> {
        &self.by_name
    }

    pub fn by_qualified(&self) -> &HashMap<String, FunctionTag> {
        &self.by_qualified
    }

    pub fn by_module(&self) -> &HashMap<String, Vec<FunctionTag>> {
        &self.by_module
    }

    pub fn file_module(&self) -> &HashMap<PathBuf, String> {
        &self.file_module
    }

    /// 限定名精确命中。
    pub fn qualified(&self, qualified_name: &str) -> Option<&FunctionTag> {
        self.by_qualified.get(qualified_name)
    }

    /// module path 以 `prefix` 开头（按分隔符边界匹配）的全部函数，稳定排序。
    pub fn candidates_under_module(&self, prefix: &str, separator: &str) -> Vec<&FunctionTag> {
        let mut out: Vec<&FunctionTag> = self
            .by_module
            .iter()
            .filter(|(module, _)| *module == prefix || module.starts_with(&format!("{prefix}{separator}")))
            .flat_map(|(_, tags)| tags.iter())
            .collect();
        out.sort_by(|a, b| {
            (&a.module_path, &a.name, a.line_start)
                .cmp(&(&b.module_path, &b.name, b.line_start))
                .then_with(|| tag_source_key(a).cmp(&tag_source_key(b)))
        });
        out
    }

    /// Phase P2a：某类型的同名成员（`("Calc", "add")` → `Calc::add` 的全部候选）。
    ///
    /// L2 `EnclosingTypeMember` 与 L3 `ReceiverTypeMember` **只走这个入口**，
    /// 因此自由函数（`enclosing_type == None`）永远不可能被当成成员命中
    /// —— 这正是 `method_receiver` 场景里 `calc.add(x)` 与 `misc.rs::add` 的区分依据。
    pub fn candidates_of_type(&self, type_name: &str, name: &str) -> &[FunctionTag] {
        self.by_type
            .get(&(type_name.to_string(), name.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Phase P2a：全部"成员"候选（`enclosing_type` 有值），按确定性顺序。
    ///
    /// 用途：调用点带 receiver 但类型推不出来时，候选集必须**收缩到成员**，
    /// 不能把同名自由函数混进来（那会制造假候选）。
    pub fn member_candidates_by_name(&self, name: &str) -> Vec<&FunctionTag> {
        let mut out: Vec<&FunctionTag> = self
            .by_name
            .get(name)
            .map(|tags| tags.iter().filter(|t| t.enclosing_type.is_some()).collect())
            .unwrap_or_default();
        out.sort_by(|a, b| tag_sort_key(a).cmp(&tag_sort_key(b)));
        out
    }

    /// Phase P2a：本文件内的全部候选（L1 词法作用域层的基础）。
    pub fn candidates_in_file(&self, file: &Path) -> Vec<&FunctionTag> {
        let mut out: Vec<&FunctionTag> = self
            .by_name
            .values()
            .flatten()
            .filter(|tag| tag.file == file)
            .collect();
        out.sort_by(|a, b| {
            (a.line_start, &a.name)
                .cmp(&(b.line_start, &b.name))
                .then_with(|| tag_source_key(a).cmp(&tag_source_key(b)))
        });
        out
    }

    /// 节点 id → tag（O(全部候选) 扫描；P2 若需要可加 by_id 哈希）。
    pub fn tag_by_id(&self, id: &Uuid) -> Option<&FunctionTag> {
        self.by_name.values().flatten().find(|tag| &tag.id == id)
    }

    /// 发生过限定名冲突的名字（升序去重）。
    pub fn qualified_collisions(&self) -> Vec<String> {
        let mut out = self.qualified_collisions.clone();
        out.sort();
        out.dedup();
        out
    }

    pub fn files_without_module_path(&self) -> usize {
        self.files_without_module_path
    }

    pub fn len(&self) -> usize {
        self.total_tags
    }

    pub fn is_empty(&self) -> bool {
        self.total_tags == 0
    }

    /// 同名函数数量 >1 的名字集合（歧义面，P2 的 precision 指标分母）。
    pub fn ambiguous_names(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .by_name
            .iter()
            .filter(|(_, tags)| tags.len() > 1)
            .map(|(name, tags)| (name.clone(), tags.len()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// 候选的**确定性排序键**：`(module_path, name, line_start, file, qualified_name)`。
///
/// 抽出成自由函数，供 `candidates_under_module` / `candidates_in_file` /
/// `member_candidates_by_name` 共用 —— 同一个仓库里三套"自己排自己的序"正是
/// 当前 first-wins 病灶的形态，P2a 在数据层先收敛成一个键。
///
/// Phase P3 收口 C：末位的 `id.as_u128()` 换成源码派生字段，理由见 [`tag_source_key`]。
fn tag_sort_key(tag: &FunctionTag) -> (String, String, usize, PathBuf, String) {
    (
        tag.module_path.clone(),
        tag.name.clone(),
        tag.line_start,
        tag.file.clone(),
        tag.qualified_name.clone(),
    )
}

/// Phase P3 收口 C：候选的**源码派生全序键**（取代"每次建图都重新生成"的 Uuid tie-break）。
///
/// `FunctionTag.id` 就是 `FunctionInfo.id` —— 每次建图都是新的 Uuid v4。拿它做末位
/// tie-break 只保证"同一进程内可复现"，**不保证"同一份源码、两次独立建图可复现"**。
/// 实测（`tests/test_p3_final.rs::p3b_layered_is_deterministic_over_all_four_repos` 在
/// `simple_js_project` 那个被压成单行的 JS 上）：同一份源码、两个互相独立的沙箱、两次全量
/// 建图，每次运行翻转的边都不一样，甚至 caller 归属会在 `onClick` 与 `Va` 之间来回跳 ——
/// 因为几十个函数同处第 2 行且同名，比较器一路走到 Uuid 那一位才被拆开。
///
/// 本键覆盖投影里**看得见**的全部源码派生字段：两候选键相同 ⇒ 它们在可观测层面完全等价，
/// 谁先谁后都不再改变任何一条边/一个节点，因此不需要 Uuid 补位。
fn tag_source_key(
    tag: &FunctionTag,
) -> (PathBuf, usize, String, String, String, Option<String>) {
    (
        tag.file.clone(),
        tag.line_start,
        tag.name.clone(),
        tag.qualified_name.clone(),
        tag.module_path.clone(),
        tag.enclosing_type.clone(),
    )
}

/// Phase P2a 排序规则：**模块距离**（只排序、绝不淘汰）。
///
/// `0` 同文件 / `1` 同目录 / `2` 同 module path / `3` 全仓其它。
/// 刻意保守：路径信息缺失（例如候选来自被 MD5 跳过的文件）时一律给 `3`，
/// 绝不因为"看起来近"就加分。
pub fn module_distance_between(call_file: &Path, candidate_file: &Path) -> u8 {
    if call_file == candidate_file {
        return 0;
    }
    let call_dir = call_file.parent();
    let cand_dir = candidate_file.parent();
    if call_dir.is_some() && call_dir == cand_dir {
        return 1;
    }
    // 同 module path 前缀（一段以上才算，避免空串/根目录误判成"同包"）。
    let split = |path: &Path| -> Vec<String> {
        path.components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect()
    };
    let a = split(call_file);
    let b = split(candidate_file);
    let shared = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    if shared >= 2 {
        return 2;
    }
    3
}

/// 声明是否位于调用点**之前**（同文件才有意义，跨文件一律 `false` = 中性）。
pub fn declared_before_call_site(
    call_file: &Path,
    call_line: usize,
    tag: &FunctionTag,
) -> bool {
    tag.file == call_file && tag.line_start <= call_line
}

/// Phase P2a：图里"未解析调用"造出的假函数节点所用的 namespace 标记。
///
/// 出处（逐字）：`codegraph/parser.rs` 的 `_create_unresolved_call_relation` 里
/// `namespace: "unresolved".to_string()`；`_handle_unresolved_call` 同一套。
/// 这里**不**改那两个函数（红线 1），只是让分层引擎的候选池不把它们当成定义。
pub const GHOST_FUNCTION_NAMESPACE: &str = "unresolved";

/// 是否幽灵节点（`namespace == "unresolved"`）。
///
/// 判据只用 namespace，不用行区间 `(n, n)`：后者是副作用而非契约，
/// 而 P0/P1 的基线指标 `ghost_nodes` 用的也正是 namespace 这一条
/// （`tests/test_p1_edges_unchanged.rs` 的 `Metrics::ghost_nodes`）。
pub fn is_ghost_function(function: &FunctionInfo) -> bool {
    function.namespace == GHOST_FUNCTION_NAMESPACE
}

/// Phase P2a 交付物 2：**仅凭图节点** + 可用的文件 ctx 构建全局索引。
///
/// 为什么必须存在这个入口（任务口径 C）：增量构建里被 `_should_skip_file`（MD5 未变）
/// 跳过的文件**不会重新解析**，因此 `CodeParser::file_scopes` 里没有它的 ctx；
/// 但它的函数节点仍然在图里，仍然必须是候选来源 —— 否则"同一个文件在全量与增量下
/// 产出不同候选集"，直接违反 P3 的一致性硬约束。
///
/// 因此这里的取数口径是：
/// - `name` ← `FunctionInfo.name`（图节点自带）；
/// - `module_path` ← 由 `file_path` 相对仓库根推导（`derive_module_path`，与全量一致）；
/// - `param_count` / `enclosing_type` / `has_body` ← 该文件**若有** ctx 就用
///   `callable_facts()` 按 `(file, line_start)` 回填，没有就留中性值（`None` / `None` / `true`）。
///
/// 中性值的后果是**下沉**（L2/L3 判不出成员身份 → 不产出候选），不是误命中 ——
/// 这是"正确性绝不依赖 ctx 是否命中"的具体体现。
///
/// ## Phase P2a：幽灵节点必须**排除在候选池之外**
/// 现状（口径 A 明确"本轮不修"）：未解析调用会在图里造一个
/// `namespace == "unresolved"`、行区间压成 `(n, n)` 的**假函数节点**
/// （见 `codegraph/parser.rs` 的 `_create_unresolved_call_relation` /
/// `_handle_unresolved_call`）。它们是图的产物，不是源码里的定义。
/// 实测（P2a 探针）：`tests/fixtures/resolution` 的 2 个幽灵节点
/// （Python `alias()`、Rust 闭包 `doubled()`）如果留在索引里，会被 L1 当成
/// "本文件的同名定义"命中，于是 layered 路径**复制**了 legacy 的错误而不是修掉它。
/// 因此本函数过滤掉它们：`alias()` 于是下沉到 L4（import 绑定 → `mod_x.py::func`），
/// `doubled()` 因为确实没有任何定义而得到 `Unresolved`（level=0），
/// **而图里的幽灵节点本身一个字都不改**（红线 1）。
pub fn build_resolution_index_from_graph(
    graph: &PetCodeGraph,
    per_file: &HashMap<PathBuf, Arc<FileScopeCtx>>,
    root: Option<&Path>,
) -> ResolutionIndex {
    let functions: Vec<FunctionInfo> = graph
        .get_all_functions()
        .into_iter()
        .filter(|f| !is_ghost_function(f))
        .cloned()
        .collect();
    let mut packages: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut callables: HashMap<PathBuf, Vec<CallableFact>> = HashMap::new();
    for (path, ctx) in per_file {
        if let Some(pkg) = ctx.declared_package.as_ref() {
            packages.insert(path.clone(), pkg.clone());
        }
        callables.insert(path.clone(), ctx.callable_facts());
    }
    ResolutionIndex::build(&functions, root, &packages, &callables)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid(byte: u8) -> Uuid {
        Uuid::from_bytes([byte; 16])
    }

    fn func(name: &str, file: &str, line: usize, language: &str, id: u8) -> FunctionInfo {
        FunctionInfo {
            id: fid(id),
            name: name.to_string(),
            file_path: PathBuf::from(file),
            line_start: line,
            line_end: line + 2,
            namespace: String::new(),
            language: language.to_string(),
            signature: Some(name.to_string()),
            module_path: None,
            qualified_name: None,
        }
    }

    fn empty_packages() -> HashMap<PathBuf, Vec<String>> {
        HashMap::new()
    }

    fn empty_callables() -> HashMap<PathBuf, Vec<CallableFact>> {
        HashMap::new()
    }

    /// 任务的核心要求：**同名多函数不得被塌缩**。
    #[test]
    fn by_name_keeps_every_same_name_function_instead_of_collapsing() {
        let root = Path::new("/repo");
        let functions = vec![
            func("helper", "/repo/alpha.rs", 3, "rust", 1),
            func("helper", "/repo/beta.rs", 4, "rust", 2),
            func("helper", "/repo/gamma.rs", 7, "rust", 3),
            func("solo", "/repo/alpha.rs", 10, "rust", 4),
        ];
        let index = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &empty_callables());

        assert_eq!(index.candidates_by_name("helper").len(), 3, "三个同名 helper 必须全部留住");
        assert_eq!(index.by_name().get("helper").unwrap().len(), 3);
        assert_eq!(index.candidates_by_name("solo").len(), 1);
        assert_eq!(index.len(), 4);
        // 歧义面：只有 helper 是同名多份。
        assert_eq!(index.ambiguous_names(), vec![("helper".to_string(), 3usize)]);
        // 歧义名绝不能被"唯一命中"API 返回。
        assert!(index.unique_by_name("helper").is_none());
        assert!(index.unique_by_name("solo").is_some());
    }

    /// 输入顺序（来自 HashMap 迭代）绝不能改变索引结果 —— 当前 first-wins 的病灶就是这条不成立。
    #[test]
    fn index_content_is_independent_of_input_order() {
        let root = Path::new("/repo");
        let functions = vec![
            func("pick", "/repo/mod_a.rs", 3, "rust", 11),
            func("pick", "/repo/mod_b.rs", 3, "rust", 12),
        ];
        let forward = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &empty_callables());
        let reversed_inputs: Vec<FunctionInfo> = functions.iter().rev().cloned().collect();
        let backward =
            ResolutionIndex::build(&reversed_inputs, Some(root), &empty_packages(), &empty_callables());

        let names = |idx: &ResolutionIndex| -> Vec<Uuid> {
            idx.candidates_by_name("pick").iter().map(|t| t.id).collect()
        };
        assert_eq!(names(&forward), names(&backward), "候选顺序必须与输入顺序无关");
        assert_eq!(
            forward.qualified("mod_b::pick").map(|t| t.id),
            backward.qualified("mod_b::pick").map(|t| t.id)
        );
    }

    #[test]
    fn module_path_uses_double_colon_for_rust_and_dot_for_others() {
        let root = Path::new("/repo");
        assert_eq!(
            derive_module_path(Path::new("/repo/src/engine/runner.rs"), Some(root), "rust", None),
            "src.engine.runner"
                .replace('.', "::")
        );
        assert_eq!(
            derive_module_path(Path::new("/repo/pkg/mod_y.py"), Some(root), "python", None),
            "pkg.mod_y"
        );
        assert_eq!(
            derive_module_path(Path::new("/repo/a/b/C.java"), Some(root), "java", None),
            "a.b",
            "Java 没有 package 声明时只给目录链，不把文件名当包"
        );
        assert_eq!(module_separator("rust"), "::");
        assert_eq!(module_separator("python"), ".");
    }

    /// P1 交付物 2(b) 的 Java 包名必须真的参与 module path 推导。
    #[test]
    fn java_declared_package_overrides_directory_derivation() {
        let root = Path::new("/repo");
        let mut packages = HashMap::new();
        packages.insert(PathBuf::from("/repo/src/main/java/ignored/Runner.java"), vec![
            "com".to_string(),
            "example".to_string(),
        ]);
        let functions = vec![func(
            "run",
            "/repo/src/main/java/ignored/Runner.java",
            5,
            "java",
            21,
        )];
        let index = ResolutionIndex::build(&functions, Some(root), &packages, &empty_callables());
        assert_eq!(index.qualified("com.example.run").map(|t| t.id), Some(fid(21)));
        assert_eq!(index.file_module().values().next().map(String::as_str), Some("com.example"));
    }

    #[test]
    fn missing_root_degrades_to_empty_module_path_and_is_counted() {
        let functions = vec![func("a", "/somewhere/a.rs", 1, "rust", 31)];
        // root = None ⇒ 不推导（宁缺毋滥），并计数留给上层告警。
        let index = ResolutionIndex::build(&functions, None, &empty_packages(), &empty_callables());
        assert_eq!(index.files_without_module_path(), 1);
        assert_eq!(index.qualified("a").map(|t| t.id), Some(fid(31)), "退化路径下裸名限定仍可用");
        assert!(index.by_module().is_empty(), "module path 为空时不进 by_module");
    }

    #[test]
    fn param_count_is_backfilled_through_file_and_line_not_uuid() {
        let root = Path::new("/repo");
        let functions = vec![func("add", "/repo/calc.rs", 5, "rust", 41)];
        let mut callables: HashMap<PathBuf, Vec<CallableFact>> = HashMap::new();
        callables.insert(
            PathBuf::from("/repo/calc.rs"),
            vec![CallableFact {
                name: "add".to_string(),
                line_start: 5,
                param_count: Some(2),
                enclosing_type: Some("Calc".to_string()),
                has_body: true,
            }],
        );
        let index = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &callables);
        assert_eq!(
            index.candidates_by_name("add")[0].param_count,
            Some(2),
            "(file, line_start) 关联必须生效"
        );

        // 行号对不上 ⇒ None（绝不瞎猜）。
        let mut mismatched = callables.clone();
        mismatched.insert(
            PathBuf::from("/repo/calc.rs"),
            vec![CallableFact {
                name: "add".to_string(),
                line_start: 99,
                param_count: Some(2),
                enclosing_type: None,
                has_body: true,
            }],
        );
        let index2 = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &mismatched);
        assert_eq!(index2.candidates_by_name("add")[0].param_count, None);
    }

    #[test]
    fn module_and_file_scoped_queries_are_stably_sorted() {
        let root = Path::new("/repo");
        let functions = vec![
            func("z", "/repo/pkg/b.rs", 9, "rust", 51),
            func("a", "/repo/pkg/a.rs", 3, "rust", 52),
            func("m", "/repo/pkg/a.rs", 1, "rust", 53),
        ];
        let index = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &empty_callables());
        let under_pkg = index.candidates_under_module("pkg", "::");
        assert_eq!(under_pkg.len(), 3);
        assert_eq!(
            under_pkg.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "m", "z"],
            "排序键是 (module_path, name, line_start, id)：pkg::a 里 a(line3) 在 m(line1) 之前，因为 name 比行号优先级高"
        );
        let in_file = index.candidates_in_file(Path::new("/repo/pkg/a.rs"));
        assert_eq!(
            in_file.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["m", "a"]
        );
    }

    #[test]
    fn qualified_name_collision_is_recorded_not_silently_dropped() {
        let root = Path::new("/repo");
        // 同文件同名两次（重载/宏展开）⇒ 限定名相同。
        let functions = vec![
            func("dup", "/repo/x.rs", 1, "rust", 61),
            func("dup", "/repo/x.rs", 1, "rust", 62),
        ];
        let index = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &empty_callables());
        assert_eq!(index.len(), 2, "两个节点都要进索引");
        assert_eq!(index.candidates_by_name("dup").len(), 2);
        assert_eq!(
            index.qualified_collisions(),
            vec!["x::dup".to_string()],
            "限定名冲突必须被记录（P2 决定是否改为多候选）"
        );
        assert_eq!(index.qualified("x::dup").map(|t| t.id), Some(fid(61)), "保留确定性最小 id");
    }

    #[test]
    fn tag_by_id_finds_nodes_across_name_buckets() {
        let root = Path::new("/repo");
        let functions = vec![func("k", "/repo/k.rs", 1, "rust", 71)];
        let index = ResolutionIndex::build(&functions, Some(root), &empty_packages(), &empty_callables());
        assert_eq!(index.tag_by_id(&fid(71)).map(|t| t.name.as_str()), Some("k"));
        assert!(index.tag_by_id(&fid(99)).is_none());
        assert_eq!(index.files_without_module_path(), 0);
    }
}
