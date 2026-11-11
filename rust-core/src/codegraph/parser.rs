use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::fs;
use uuid::Uuid;
use tracing::{info, warn, debug};

use crate::codegraph::types::{
    FunctionInfo, CallRelation, PetCodeGraph, EntityGraph, ClassInfo, ClassType,
    FileIndex, SnippetIndex
};
use crate::codegraph::graph::CodeGraph;
// Phase P2a：`ResolverMode` 与 CLI 共用同一个枚举（先例：`storage/mod.rs` 与
// `storage/persistence.rs` 都用 `crate::cli::args::StorageMode`），避免出现第二套"模式"类型。
use crate::cli::args::ResolverMode;
use crate::codegraph::resolution::resolver::{DEFAULT_MAX_CANDIDATES, ResolutionConfig};
// Phase P2b 交付物 2：分层接线所需的引擎入口（P2a 时这些只在 `resolution/` 内部被引用，
// 接线之后 `parser.rs` 成为它们的第一个生产消费者 —— 与 `tests/test_p1_edges_unchanged.rs`
// 里"未接线证明"的白名单同步更新）。
use crate::codegraph::resolution::index::module_separator;
use crate::codegraph::resolution::resolver::{
    call_sites_from_symbols_with_source, resolve_call_with_config, CallSite,
};
use crate::codegraph::resolution::{
    build_resolution_index_from_graph, emit_layered_resolution as resolution_emit,
    layered_caller_node as resolution_caller_node, Resolution, ResolutionStatus,
};
use crate::codegraph::resolution::{build_file_scope_ctx, FileScopeCtx};
use crate::codegraph::treesitter::TreeSitterParser;
use crate::detector;

/// 全仓函数名索引：函数名 -> 函数信息。
///
/// 每轮调用关系分析 pass 开始前一次性预建（见 `_build_function_name_index`），
/// 把原本"每个 FunctionCall 符号都遍历全部文件 × 全部函数"的 O(calls × N_total)
/// 降为 O(1) 查询。
type FunctionNameIndex<'a> = HashMap<&'a str, &'a FunctionInfo>;

/// 函数去重键：`(文件路径, 起始行, 结束行)` 三元组。
///
/// 与增量合并（`_merge_new_functions` / `_merge_new_functions_to_code_graph`）
/// 原有的判定条件**完全一致**，只是把它从"每个候选函数都线性扫一遍全图"改成
/// "一次遍历预建集合 + O(1) 查表"。
///
/// 这里用 **owned `PathBuf`** 而不是 `&'a Path`：`seen` 需要在整个合并循环内存活，
/// 而循环体内要 `code_graph.add_function(...)` 可变借用同一个图；若 `seen` 借用了
/// 图内节点权重（`code_graph.functions.values()` / `graph.node_weights()`），
/// 会与可变借用冲突。借用 `self.file_functions` 一侧不冲突，但同一个集合必须
/// 用统一的所有权形式，所以两边都克隆 PathBuf —— 成本 O(路径长度)，换来
/// 由 O(k_new × N_total) 降到 O(k_new + N_total)。
type FunctionKey = (PathBuf, usize, usize);

// ============================================================================================
// Phase P3 交付物 1（唯一真源）：单文件"AST → 实体"与"AST 调用点 → 图边"的**唯一**实现。
//
// 改造前这三段逻辑各有两份逐行等价的副本：
//   - `CodeParser::_extract_entities_from_file` 与 `IncrementalManager::_extract_entities_from_file`
//   - `CodeParser::_analyze_file_calls`（legacy/layered 两档）与 `IncrementalManager::_analyze_file_calls`
// 副本的存在就是 P0 记录在案的"三套并行 first-wins 实现，改一处漏一处"缺陷的复发温床。
// P3 之后：两条链路都只**委托**下面的自由函数，不再各自持有一份实现。
//
// Phase P3 收口 A：`IncrementalManager` 那一侧的**委托者本身已被删除** ——
// 它的 `refresh_file` / `_extract_entities_from_file` / `_analyze_file_calls` 整条产边链路
// 在 `src/`、`tests/`、`examples/`、`benches/` 里零调用者（证据：`rg -n
// "IncrementalManager|get_incremental" src tests examples benches` 只有本文件与
// `storage/mod.rs` 的构造点；真实的增量刷新一直是 `CodeParser::refresh_file`，
// 由 `codegraph/repository.rs::RepositoryManager::refresh_file` 调用）。
// ⇒ 现在这三个共用件的**唯一**消费者就是本文件的刷新链路（下面 `_extract_entities_from_file`
// 与 `_analyze_file_calls`），`pub(crate)` 可见性保持不变，将来要接第二条链路时它已经在那里。
//
// ⚠ 唯一没有并进来的历史副本是全量链路的 `_analyze_file_calls_for_petgraph`（它带
// `FunctionNameIndex` 的四级 callee 回退，且幽灵会**同时落成节点** ⇒ 形成 legacy 特有的
// "同名幽灵跨行复用"）。它与刷新档的口径本就不同，强行合并会改变
// legacy 产边（红线 1），故保留为"全量档 legacy"的独立实现。
// 这条差异的**权威登记位置**（不再有"P3 报告"这个东西）：
//   - 实现本体：下面的 `_analyze_file_calls_for_petgraph`（全量档 legacy 专用）；
//   - 收敛守卫：`tests/test_p1_edges_unchanged.rs::FIRST_WINS_ABSENCE_MARKERS`
//     （反向钉住"第二份实现不许再长出来"）与同文件的 legacy 聚合口径 `32/13/11/2/4/9/2/13`。
// ============================================================================================

/// Phase P3：把单个文件解析成图实体（`FunctionInfo`/`ClassInfo`）与文件级解析上下文 `ctx`。
///
/// 由 `CodeParser::_extract_entities_from_file` 调用（Phase P3 收口 A 之后它是唯一调用者；
/// 原来这里还有 `IncrementalManager::_extract_entities_from_file` 那份逐行等价的副本）。
/// `language` / `namespace` 由调用方传入而不是在这里重算：历史上两条链路各自的
/// `_detect_language`/`_extract_namespace` 存在差异（增量档不认 `.js/.jsx`），
/// 统一它们会改变节点字段，超出本轮范围，故只把"实体提取"这一段收敛。
pub(crate) fn extract_file_entities(
    ts_parser: &TreeSitterParser,
    file_path: &Path,
    resolution_root: Option<&Path>,
    language: &str,
    namespace: &str,
) -> Result<(Vec<ClassInfo>, Vec<FunctionInfo>, Arc<FileScopeCtx>), String> {
    // `parse_file_with_metadata` 内部就是 `parse_file` 的超集（`parse_file` 是它 `.symbols`
    // 的转发），符号列表逐字节相同，只是额外带出 Java 的 `package a.b.c;`（P1 交付物 2(b)）。
    let ast = ts_parser
        .parse_file_with_metadata(&file_path.to_path_buf())
        .map_err(|e| format!("Failed to parse file {}: {:?}", file_path.display(), e))?;
    let declared_package = ast.declared_package;
    let symbols = ast.symbols;

    // 同一批符号顺带产出只读 ctx（P1 交付物 3：只建不消费，legacy 分支不读它）。
    let ctx = build_file_scope_ctx(
        file_path,
        &symbols,
        resolution_root,
        language,
        declared_package,
    );

    let mut classes = Vec::new();
    let mut functions = Vec::new();

    for symbol in symbols {
        let symbol_guard = symbol.read();
        let symbol_ref = symbol_guard.as_ref();

        match symbol_ref.symbol_type() {
            crate::codegraph::treesitter::structs::SymbolType::FunctionDeclaration => {
                let function = FunctionInfo::legacy(
                    Uuid::new_v4(),
                    symbol_ref.name().to_string(),
                    file_path.to_path_buf(),
                    symbol_ref.full_range().start_point.row + 1,
                    symbol_ref.full_range().end_point.row + 1,
                    namespace.to_string(),
                    language.to_string(),
                    Some(symbol_ref.name().to_string()),
                );
                functions.push(function);
            }
            crate::codegraph::treesitter::structs::SymbolType::StructDeclaration => {
                let class = ClassInfo {
                    id: Uuid::new_v4(),
                    name: symbol_ref.name().to_string(),
                    file_path: file_path.to_path_buf(),
                    line_start: symbol_ref.full_range().start_point.row + 1,
                    line_end: symbol_ref.full_range().end_point.row + 1,
                    namespace: namespace.to_string(),
                    language: language.to_string(),
                    class_type: ClassType::Struct,
                    parent_class: None,
                    implemented_interfaces: vec![],
                    member_functions: vec![],
                    member_variables: vec![],
                };
                classes.push(class);
            }
            _ => {}
        }
    }

    Ok((classes, functions, ctx))
}

/// Phase P3：legacy 档的单文件产边（刷新/增量两条链路共用的唯一实现）。
///
/// 语义与改造前的两份副本逐字段一致（红线 2）：
/// - caller：在"本文件函数切片"上按行区间 **first-wins**；
/// - callee：先在本文件按**裸名相等** first-wins，再退到全仓 `find_functions_by_name().first()`；
/// - 未解析：只写一条 `callee_id` 为一次性随机 Uuid 的幽灵边，**不**落节点
///   （这是刷新/增量档的历史口径；全量档会落节点，见上面的登记）。
/// - 写边一律经 `PetCodeGraph::upsert_call_site_edges`（P2b 收口确立的唯一边写入入口）。
pub(crate) fn emit_legacy_call_edges_for_file(
    ts_parser: &TreeSitterParser,
    file_path: &Path,
    funcs: &[FunctionInfo],
    call_graph: &mut PetCodeGraph,
) -> Result<(), String> {
    let symbols = ts_parser
        .parse_file(&file_path.to_path_buf())
        .map_err(|e| format!("Failed to parse file for call analysis: {:?}", e))?;

    for symbol in symbols {
        let symbol_guard = symbol.read();
        let symbol_ref = symbol_guard.as_ref();

        if symbol_ref.symbol_type() != crate::codegraph::treesitter::structs::SymbolType::FunctionCall {
            continue;
        }
        let call_name = symbol_ref.name();
        let call_line = symbol_ref.full_range().start_point.row + 1;

        // caller：本文件切片上按行区间 first-wins（切片顺序 = 实体产出顺序，与改造前一致）。
        let Some(caller_id) = funcs.iter().find(|f| {
            f.file_path == file_path && call_line >= f.line_start && call_line <= f.line_end
        }).map(|f| f.id) else {
            continue;
        };

        // callee：先本文件同名 first-wins，再全仓 first-wins。
        let local = funcs.iter().find(|f| f.name == call_name).map(|f| f.id);
        let global_functions = call_graph.find_functions_by_name(call_name);
        let callee_id = local.or_else(|| global_functions.first().map(|f| f.id));

        let relation = match callee_id {
            Some(callee_id) => CallRelation::legacy(
                caller_id,
                callee_id,
                "".to_string(), // 会在 add_call_relation 中填充
                call_name.to_string(),
                file_path.to_path_buf(),
                file_path.to_path_buf(),
                call_line,
                true,
            ),
            None => CallRelation::legacy(
                caller_id,
                Uuid::new_v4(), // 一次性临时 ID（刷新/增量档不落幽灵节点）
                "".to_string(),
                call_name.to_string(),
                file_path.to_path_buf(),
                file_path.to_path_buf(),
                call_line,
                false,
            ),
        };

        if call_graph
            .upsert_call_site_edges(&caller_id, call_line, call_name, vec![relation])
            == 0
        {
            warn!("Failed to add call relation: {}", call_name);
        }
    }

    Ok(())
}

/// Phase P3：layered 档的单文件产边（刷新/增量两条链路共用的唯一实现）。
///
/// 两条链路改造前各有一份几乎相同的实现（差别只在 ctx 与 funcs 的来源、以及
/// `ResolutionConfig` 的来源 —— 这些都留在调用方，作为参数传进来）。判定代码本身
/// （调用点提取 → caller 归属 → 分层解析 → 发射）从这里出发只有一份。
pub(crate) fn emit_layered_call_edges_for_file(
    ts_parser: &TreeSitterParser,
    file_path: &Path,
    ctx: &Arc<FileScopeCtx>,
    funcs: &[FunctionInfo],
    call_graph: &mut PetCodeGraph,
    resolution_root: Option<&Path>,
    config: &ResolutionConfig,
) -> Result<(), String> {
    // 只取 symbols：ctx 已由调用方供给（绝不为了喂索引而把同一个文件再解析一遍）。
    let symbols = ts_parser
        .parse_file(&file_path.to_path_buf())
        .map_err(|e| format!("Failed to parse file for call analysis: {:?}", e))?;

    let mut per_file: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
    per_file.insert(file_path.to_path_buf(), ctx.clone());
    // 索引一律来自当前图（不塌缩），候选池因此包含别的文件的节点。
    let index =
        build_resolution_index_from_graph(call_graph, &per_file, resolution_root);

    let source = fs::read_to_string(file_path).ok();
    // P2a 口径①：生产路径必须走 `_with_source` 变体
    // （AST 的 `namespace` 对调用符号恒空，限定前缀只能从源码文本还原）。
    let sites = call_sites_from_symbols_with_source(&symbols, ctx.as_ref(), source.as_deref());

    let max_candidates = config.max_candidates.max(1) as usize;
    for site in sites {
        let Some(caller) = resolution_caller_node(&site, Some(ctx.as_ref()), funcs) else {
            continue;
        };
        let resolution = resolve_call_with_config(&site, Some(ctx.as_ref()), &index, config);
        resolution_emit(&site, &caller, &resolution, call_graph, max_candidates);
    }

    Ok(())
}

/// Phase P3 交付物 1：把唯一的产边结果（`PetCodeGraph`）投影成 `CodeGraph` 只读视图。
///
/// 存在的唯一理由：`services::CodeAnalyzer` 的十余个查询方法、`tests/test_functional.rs`
/// 与 `main.rs` 的 `Commands::Init` 都以 `&CodeGraph` 为契约。本函数**不产生任何新边**
/// —— 它只是把 `PetCodeGraph` 的节点/边逐条复制出来（写法与改造前
/// `_load_existing_code_graph` 里已有的 Pet→Code 转换完全一致），因此
/// "AST 调用点边只有一个写入口"这条不变量不受影响。
pub(crate) fn project_petgraph_to_code_graph(pet: &PetCodeGraph) -> CodeGraph {
    let mut code_graph = CodeGraph::new();
    for function in pet.get_all_functions() {
        code_graph.add_function(function.clone());
    }
    for relation in pet.get_all_call_relations() {
        code_graph.add_call_relation(relation.clone());
    }
    code_graph.update_stats();
    code_graph
}

/// 代码解析器，负责解析源代码文件并提取函数调用关系
pub struct CodeParser {
    /// 文件路径 -> 函数列表映射
    file_functions: HashMap<PathBuf, Vec<FunctionInfo>>,
    /// 函数名 -> 函数信息映射（用于解析调用关系）
    function_registry: HashMap<String, FunctionInfo>,
    /// Tree-sitter解析器
    ts_parser: TreeSitterParser,
    /// 文件索引
    file_index: FileIndex,
    /// 代码片段索引
    snippet_index: SnippetIndex,
    /// 是否忽略所有持久化缓存、强制全量重建（由 CLI 的 `--force-reindex` 置位）。
    ///
    /// 只有这一个布尔开关，默认 `false`，因此既有行为完全不变；置位后
    /// `_load_existing_graph` / `_load_existing_code_graph` / `_load_file_hashes`
    /// 三个缓存读取点一律返回"缓存 miss"，于是 `build_code_graph` /
    /// `build_petgraph_code_graph` 必然重新解析每个文件，并在第 8 步重新写回哈希。
    force_reindex: bool,
    /// 文件级解析上下文缓存：作用域树 + import 绑定表 + module path（见 `codegraph::resolution`）。
    ///
    /// 与 `file_functions` / `function_registry` / `file_index` / `snippet_index` 严格平行：
    /// 每个文件解析成功时写入，`_remove_file_entities` 移除，两个 `build_*` 入口按新的仓库根重建；
    /// 内容未变（MD5 命中）而被跳过的文件由 `ensure_file_scope_ctx` 按需补建。
    ///
    /// **现在是 layered 解析的真实输入**：`_analyze_file_calls_layered` 经 `file_scope_ctx`、
    /// `_analyze_petgraph_call_relations_layered` 经 `ensure_file_scope_ctx` 取到它，再喂给
    /// `resolve_call` —— 作用域树决定 caller 归属与 L1/L2 的遮蔽判定，import 绑定表喂 L4，
    /// module path 喂 L5/L6。历史（Phase P1）里这里只建不消费；legacy 的同名 first-wins
    /// （`_resolve_callee_function`、`_resolve_qualified_function_name`、`_build_function_name_index`）作为 `--resolver=legacy` 逃生口有意保留，数值红线不变。
    file_scopes: HashMap<PathBuf, Arc<FileScopeCtx>>,
    /// 本轮建图的仓库根，用于 `resolution` 的 module path 推导。
    ///
    /// module path 已按"文件相对本根 + Java `package` 声明"真实推导（`derive_module_path`）；
    /// 为 `None`（例如直接调用 `parse_file` 而没走 `build_*`）时降级为空串，构建配置尚未读取。
    resolution_root: Option<PathBuf>,
    /// `--resolver` 的取值：**默认 `Layered`**（见 `CodeParser::new`），`Legacy` 是逐字节回滚逃生口。
    ///
    /// 已被全部产边路径读取：`_analyze_file_calls` / `_analyze_petgraph_call_relations` /
    /// `_resolve_callee_function` / `_resolve_qualified_function_name` 都按它分叉，layered 分支
    /// 走 `codegraph::resolution`，legacy 分支走原有的 first-wins（红线：数值口径不变）。
    /// 它同时是图缓存档位戳的一部分：`_load_existing_graph` 用 `validate_resolution_stamp`
    /// 校验 `CGSP` 缓存里的 `resolver_mode` / `max_candidates`，档位不符即判定缓存失效。
    resolver_mode: ResolverMode,
    /// `--max-candidates`（默认 `DEFAULT_MAX_CANDIDATES` = 8）：**已生效**，layered 逐个候选落边时按它截断**条数**，但边上的 `candidate_count` 仍写截断前的真实候选数（歧义不会被伪装成唯一命中）。
    max_candidates: usize,
    /// Phase P2a 交付物 4：`--levels=` 的原始字符串（`None` = 全层开启）。
    /// 解析口径由 [`ResolutionConfig::from_levels_spec`] 唯一决定；非法值在建图入口即报错，
    /// **绝不静默忽略**（否则诊断结论会建立在"以为开了其实没开"的配置上）。
    levels_spec: Option<String>,
    /// Phase P2b 收口（口径 A）：上一轮 `build_*` 里**真正重新解析**的文件数（只读诊断）。
    ///
    /// 为什么需要它：任务口径 A 要求把两件事解耦 ——
    ///  - **解析/重建决策**：由既有 MD5 哈希契约决定（哈希命中 ⇒ 不重新解析、不重建
    ///    `file_functions`、不重建 `file_scopes`）；
    ///  - **索引供给**：`build_resolution_index_from_graph` 仅凭**图里已有的函数节点**
    ///    就能产出候选，被跳过的文件候选不许缺失（ctx 相关字段允许降级为中性值）。
    /// 于是"本轮没有重新解析任何文件"是一个**必须可断言**的性质，而改造前它只能靠
    /// "图节点数为 0"这种代理指标间接观察（正是 `test_force_reindex_escape_hatch_bypasses_all_caches`
    /// 里那条被 P0 空图缺陷污染的断言）。这里把它变成直接可测的计数。
    ///
    /// ⚠ 只读诊断：`ensure_file_scope_ctx` 的**按需补建不计入**本计数（它只补 ctx，
    /// 不产生 `FunctionInfo`/`CallRelation`，因此不算"重新解析该文件的实体/边"）。
    /// 该口径由 `tests/test_p2b_wiring.rs::p2b_hash_skip_contract_*` 钉死。
    last_round_parsed: usize,
    /// 上一轮 `build_*` 里因"内容未变（MD5 命中）"或"混淆 JS"被跳过的文件数（只读诊断）。
    last_round_skipped: usize,
}

impl CodeParser {
    pub fn new() -> Self {
        Self {
            file_functions: HashMap::new(),
            function_registry: HashMap::new(),
            ts_parser: TreeSitterParser::new(),
            file_index: FileIndex::default(),
            snippet_index: SnippetIndex::default(),
            force_reindex: false,
            file_scopes: HashMap::new(),
            resolution_root: None,
            // Phase P3 交付物 3：默认档位从 legacy 切到 **layered**（分层精度解析）。
            // `legacy` 作为 `--resolver=legacy` 回滚逃生口保留，行为与 P1 逐字节一致；
            // 但"不置任何 flag"现在走 layered。依赖 legacy 产边基线的测试/守卫必须
            // **显式** `set_resolver_mode(ResolverMode::Legacy)`，不得再隐式依赖此默认值。
            resolver_mode: ResolverMode::Layered,
            max_candidates: DEFAULT_MAX_CANDIDATES as usize,
            levels_spec: None,
            last_round_parsed: 0,
            last_round_skipped: 0,
        }
    }

    /// 设置/清除"强制全量重建"开关（对应 CLI 的 `--force-reindex`）。
    ///
    /// 之所以做成方法而不是构造函数参数：`CodeParser` 被 `services::CodeAnalyzer`、
    /// `codegraph::repository` 等多处构造，加参数会波及所有调用点；一个默认关闭的
    /// setter 是改动面最小、又能让三个缓存读取点集中生效的方式。
    pub fn set_force_reindex(&mut self, force_reindex: bool) {
        self.force_reindex = force_reindex;
    }

    /// 当前是否处于强制全量重建模式。
    pub fn force_reindex_enabled(&self) -> bool {
        self.force_reindex
    }

    /// Phase P2b 收口（口径 A）：上一轮 `build_code_graph` / `build_petgraph_code_graph`
    /// 的 `(真正重新解析的文件数, 被跳过的文件数)`。
    ///
    /// 这是"哈希命中即跳过"契约**唯一可直接断言**的观测点：
    /// - `force_reindex` 置位 ⇒ `(_ > 0, 0)`（三个缓存读取点一律 miss）；
    /// - 图缓存存在 + 内容未变 ⇒ `(0, N)`（一个文件都不许多解析）。
    ///
    /// ⚠ 只读诊断，不参与任何解析决策；`ensure_file_scope_ctx` 的按需补建**不**计入
    /// "重新解析"（它只往 `file_scopes` 里写 ctx，不产生 `FunctionInfo` / `CallRelation`）。
    pub fn last_round_file_counts(&self) -> (usize, usize) {
        (self.last_round_parsed, self.last_round_skipped)
    }

    /// Phase P1 交付物 3：读取某个文件的解析上下文（作用域树 + import 表 + module path）。
    ///
    /// 这是 `file_scopes` 缓存**唯一**的公开读者：P1 里只有单测和 `tests/test_p1_edges_unchanged.rs`
    /// 调用它，四处 callee 解析与 caller 归属一行未改，因此调用边语义不变。
    ///
    /// 返回 `None` 的两种情况：
    /// 1. 该文件本轮没有被解析过（未进入 `parse_file` / `refresh_file`）；
    /// 2. 增量构建中被文件哈希跳过（内容未变 ⇒ 没有重新解析 ⇒ 没有重建 ctx）。
    ///    TODO(P2)：接入解析路径时必须为"跳过的文件"补建 ctx（要么持久化，
    ///    要么在 `_should_skip_file` 命中时按已有图节点重建），否则该文件的调用点拿不到作用域。
    pub fn file_scope_ctx(&self, path: &Path) -> Option<Arc<FileScopeCtx>> {
        self.file_scopes.get(path).cloned()
    }

    /// 已建立 ctx 的文件数量（诊断与守卫测试用；不参与任何解析决策）。
    pub fn file_scope_count(&self) -> usize {
        self.file_scopes.len()
    }

    /// Phase P2a 交付物 2(b)：**按需补建**某个文件的 `FileScopeCtx`。
    ///
    /// 解决的正是 `file_scope_ctx()` 文档里那条 TODO(P2)：增量构建中被文件哈希
    /// （MD5）跳过的文件不会重新解析，因此 `file_scopes` 里没有它的 ctx。任务口径 C 要求
    /// **正确性绝不依赖缓存是否命中** —— 缺 ctx 就必须补建，而不是"降级成只用不需要
    /// ctx 的层"，否则同一个文件在全量/增量两条路径下会产出不同的边，直接违反 P3 的
    /// 一致性硬约束。
    ///
    /// 口径与 P1 的两个写入点（`_extract_entities_from_file`、`parse_file`）**逐参数一致**：
    /// `TreeSitterParser::parse_file_with_metadata` + `_detect_language` +
    /// `build_file_scope_ctx(path, symbols, self.resolution_root, language, declared_package)`。
    ///
    /// 硬约束（红线 1）：这里**只**往 `file_scopes` 里写，绝不触碰 `file_functions` /
    /// `function_registry` / `file_index` / `snippet_index`，也绝不产生
    /// `FunctionInfo` / `CallRelation` / `Uuid::new_v4()`。
    /// 因此 P2a 里它只被新测试与 P2b 的解析路径消费，对既有建图产出零影响。
    ///
    /// 返回 `None` 仅当文件无法读取/无法解析（与 `parse_file` 的失败口径一致，打 `warn`
    /// 而不 panic）；调用方应当把该文件的调用点按"无 ctx"处理并下沉到不需要 ctx 的层。
    pub fn ensure_file_scope_ctx(&mut self, path: &Path) -> Option<Arc<FileScopeCtx>> {
        // 命中缓存直接返回：与 `file_scope_ctx()` 同一份数据，绝不重复解析。
        if let Some(ctx) = self.file_scopes.get(path) {
            return Some(ctx.clone());
        }

        let path_buf = path.to_path_buf();
        let ast = match self.ts_parser.parse_file_with_metadata(&path_buf) {
            Ok(ast) => ast,
            Err(err) => {
                warn!("ensure_file_scope_ctx: 解析失败 {}: {}", path.display(), err.message);
                return None;
            }
        };
        let language = self._detect_language(path);
        let ctx = build_file_scope_ctx(
            path,
            &ast.symbols,
            self.resolution_root.as_deref(),
            &language,
            ast.declared_package,
        );
        // 与 P1 写入点一样回装进缓存，后续同一文件的调用点复用。
        self.file_scopes.insert(path_buf, ctx.clone());
        Some(ctx)
    }

    /// 本轮使用的解析引擎模式（对应 CLI 的 `--resolver`，默认 `Layered`）。
    ///
    /// 该模式在建图热路径上生效：`_analyze_file_calls` / `_build_function_name_index` /
    /// `_resolve_callee_function` / `_resolve_qualified_function_name` 按它分叉；
    /// 全量档的 legacy 路径另由 `_analyze_file_calls_for_petgraph` 独立实现。
    pub fn set_resolver_mode(&mut self, mode: ResolverMode) {
        self.resolver_mode = mode;
    }

    /// 当前解析引擎模式（Phase P3 起默认 `Layered`）。
    pub fn resolver_mode(&self) -> ResolverMode {
        self.resolver_mode.clone()
    }

    /// 候选保留上限（对应 CLI 的 `--max-candidates`，默认 8）。P2a 只存不用。
    pub fn set_max_candidates(&mut self, max_candidates: usize) {
        self.max_candidates = max_candidates;
    }

    /// 当前候选上限。
    pub fn max_candidates(&self) -> usize {
        self.max_candidates
    }

    /// 设置 `--levels=` 的原始字符串（`None`/空串 = 全层开启）。P2a 只存不用。
    pub fn set_levels_spec(&mut self, spec: Option<String>) {
        self.levels_spec = spec.filter(|s| !s.trim().is_empty());
    }

    /// 当前 `--levels=` 原始字符串。
    pub fn levels_spec(&self) -> Option<&str> {
        self.levels_spec.as_deref()
    }

    /// 把 `--max-candidates` / `--levels=` 组装成引擎配置（**只读诊断，不参与任何产边**）。
    ///
    /// `levels_spec` 非法时返回 `None`：调用方（CLI）必须**立即报错退出**，
    /// 不能静默退回全层，否则"关掉 L5"这类诊断结论就不可信。
    pub fn resolution_config(&self) -> Option<ResolutionConfig> {
        let base = match self.levels_spec.as_deref() {
            Some(spec) => ResolutionConfig::from_levels_spec(spec)?,
            None => ResolutionConfig::all(),
        };
        // usize → u8：超过 255 直接饱和（与 P2b 落盘的 `candidate_count: u8` 口径一致）。
        let max = self.max_candidates.min(u8::MAX as usize) as u8;
        Some(base.with_max_candidates(max))
    }

    /// Check whether a JS file is obfuscated/compiled code.
    /// Non-JS files directly return Ok(false).
    pub fn is_obfuscated_js_file(&self, file_path: &Path) -> Result<bool, String> {
        // Only check JS/JSX files
        let is_js = file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("js") || e.eq_ignore_ascii_case("jsx"))
            .unwrap_or(false);

        if !is_js {
            return Ok(false);
        }

        let content = std::fs::read_to_string(file_path).map_err(|e| {
            format!("Failed to read file for obfuscation check '{}': {}", file_path.display(), e)
        })?;

        let report = detector::analyze_js_code(&content);
        Ok(report.code_type == detector::CodeType::CompiledCode)
    }

    /// 扫描目录下的所有支持的文件
    pub fn scan_directory(&mut self, dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        self._scan_directory_recursive(dir, &mut files);
        // Phase P3 收口 C：**文件处理顺序必须与文件系统无关**。
        // `fs::read_dir` 的返回顺序是目录物理布局决定的，同一份源码被复制到两个不同目录
        // （典型场景：CI 沙箱、临时副本）时顺序就可能不同 ⇒ 节点入图顺序不同 →
        // legacy 的同名 first-wins 与 layered 的"幽灵先查再建"落在不同文件上，
        // 于是"同一份源码两次独立建图"产出的图不再逐字段相等
        // （实测 `tests/test_p3_final.rs::p3b_layered_is_deterministic_over_all_four_repos`
        // 在 `simple_js_project` 上每次都有若干条边的 `resolution_level` 在 1/7 之间翻转）。
        // 排序只改**顺序**，不改集合：节点数/边数与文件数都与此前完全一致。
        files.sort();
        files
    }

    fn _scan_directory_recursive(&self, dir: &Path, files: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // 跳过常见的忽略目录
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('.') || name == "target" || name == "node_modules" || name == "__pycache__" || name == "out" {
                            continue;
                        }
                    }
                    self._scan_directory_recursive(&path, files);
                } else if self.is_supported_file(&path) {
                    files.push(path);
                }
            }
        }
    }

    /// 判断文件是否为支持的源代码文件
    fn is_supported_file(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            matches!(ext.to_lowercase().as_str(),
                "cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "hh" |
                "inl" | "inc" | "tpp" | "tpl" |
                "py" | "py3" | "pyx" |
                "java" |
                "rs" |
                "ts" |
                "tsx" |
                "js" | "jsx" |
                "go"
            )
        } else {
            false
        }
    }

    /// 增量更新单个文件
    pub fn refresh_file(
        &mut self,
        file_path: &PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        info!("Refreshing file: {}", file_path.display());

        // 检查文件是否存在
        if !file_path.exists() {
            // 文件被删除，清理相关索引
            self._remove_file_entities(file_path, entity_graph, call_graph);
            return Ok(());
        }

        // 解析文件，提取新的实体和函数
        let (classes, functions) = self._extract_entities_from_file(file_path)?;

        // Phase P1 交付物 3：`_extract_entities_from_file` 已经把本次的 ctx 写进 `file_scopes`，
        // 而下面的 `_remove_file_entities` 会按"整文件失效"的语义把它一并清掉 ——
        // 与它对 `file_functions` 做的事完全一样，所以这里同样要在清理之后把新鲜的 ctx 装回去
        // （顺序不能反，否则刷新过的文件反而查不到 ctx）。
        let fresh_ctx = self.file_scopes.remove(file_path);

        // Phase P3 收口 C（真缺陷修复）：删除之前先快照**指向本文件**的调用边。
        //
        // `_remove_file_entities` 走 `remove_function` / `remove_functions_by_file`，而 petgraph
        // 删节点会连带删除**所有入射边** —— 其中包含"未改动文件的 caller → 本文件 callee"的边。
        // 这些 caller 本轮不重新解析，之后没有任何路径会把它们补回来 ⇒ 增量图比"同一份源码
        // 从零重建"少边。实测（`tests/test_p3_final.rs::p3b_full_vs_incremental_are_equivalent_over_four_repos_both_modes`）：
        // `simple_rust_project`/layered 少 2 条 —— `src/lib.rs::complex_calculation` 打到
        // `src/math.rs::{add,multiply}` 的两条边在刷新后凭空消失。
        // 下面在本文件节点重建之后把快照的边接到**新节点**上；匹配不到（函数被改名/删除）
        // 就保持原语义：边随 callee 一起失效。
        let orphaned_incoming: Vec<(String, usize, CallRelation)> = call_graph
            .get_all_call_relations()
            .into_iter()
            .filter_map(|relation| {
                let callee = call_graph.get_function_by_id(&relation.callee_id)?;
                (callee.file_path == *file_path)
                    .then(|| (callee.name.clone(), callee.line_start, relation.clone()))
            })
            .collect();

        // 移除旧的实体和函数
        self._remove_file_entities(file_path, entity_graph, call_graph);

        if let Some(ctx) = fresh_ctx {
            self.file_scopes.insert(file_path.clone(), ctx);
        }

        // 关键：把本次解析出的函数写回 parser 自己的索引，与 parse_file 的数据源保持一致。
        // `_analyze_file_calls` / `_update_snippet_index` 都依赖 file_functions 才能拿到
        // 当前文件刚生成的函数（否则 caller 永远解析不到、增量刷新会丢调用边）。
        // functions 随后会被 move 进 call_graph，所以这里必须先 clone。
        self.file_functions.insert(file_path.clone(), functions.clone());

        // 添加到图中
        let class_ids: Vec<Uuid> = classes.iter().map(|c| c.id).collect();
        let function_ids: Vec<Uuid> = functions.iter().map(|f| f.id).collect();

        for class in classes {
            entity_graph.add_class(class);
        }

        for function in functions {
            call_graph.add_function(function);
        }

        // 分析调用关系
        self._analyze_file_calls(file_path, call_graph)?;

        // Phase P3 收口 C：把上面快照的跨文件入边接回**本轮重建**的节点（说明见快照处注释）。
        // 匹配顺序：先按 `(名字, 起始行)` 精确命中，再退化到按名字命中 ——
        // 前者覆盖"编辑没有挪动这个函数"（绝大多数情况），后者覆盖"因插入/删除行而整体位移"。
        // 只用图里可比的源码派生字段做键，不依赖每次建图都重新生成的 Uuid。
        {
            let callee_identity: Vec<(String, usize, Uuid)> = self
                .file_functions
                .get(file_path)
                .map(|funcs| {
                    funcs
                        .iter()
                        .map(|f| (f.name.clone(), f.line_start, f.id))
                        .collect()
                })
                .unwrap_or_default();
            let mut relinked = 0usize;
            for (name, line_start, mut relation) in orphaned_incoming {
                let new_callee_id = callee_identity
                    .iter()
                    .find(|(n, l, _)| *n == name && *l == line_start)
                    .or_else(|| callee_identity.iter().find(|(n, _, _)| *n == name))
                    .map(|(_, _, id)| *id);
                let Some(id) = new_callee_id else { continue };
                relation.callee_id = id;
                // 走既有唯一入口 `add_call_relation`：它自己按 `(caller, callee, line)` 去重，
                // 并负责 resolved/unresolved 统计，因此这里不碰统计、不直接 add_edge。
                if call_graph.add_call_relation(relation).is_ok() {
                    relinked += 1;
                }
            }
            if relinked > 0 {
                debug!(
                    "refresh_file: 重新接回 {} 条指向本文件的跨文件调用边 ({})",
                    relinked,
                    file_path.display()
                );
            }
        }

        // 更新索引
        self.file_index.rebuild_for_file(file_path, class_ids.clone(), function_ids.clone());

        // 更新代码片段索引
        self._update_snippet_index(file_path, &class_ids, &function_ids, entity_graph)?;

        info!("Successfully refreshed file: {}", file_path.display());
        Ok(())
    }

    /// Phase P2b 交付物 1 / Phase P3 收口 C：**layered 档节点身份的唯一写入点**。
    ///
    /// `module_path` 取自 `FileScopeCtx`，`qualified_name = module_path + 分隔符 + 函数名`。
    /// **legacy 档逐字节不变**：这两个字段一律保持 `None`（红线 1），所以整段被 mode 守卫。
    ///
    /// 全量（`parse_file`）与增量（`_extract_entities_from_file`）两条链路都必须走这里：
    /// 收口 C 之前只有全量走，于是 `refresh_file` 过的文件在图里退化成"无身份节点"
    /// （`qualified_name`/`module_path` 双双为 `None`），实测 4 个整仓 × 两档的
    /// 节点投影全部对不上"同一份源码从零重建"（`tests/test_p3_final.rs::p3b_full_vs_incremental_*`）。
    fn _apply_layered_node_identity(
        &self,
        file_path: &PathBuf,
        language: &str,
        functions: &mut Vec<FunctionInfo>,
    ) {
        if self.resolver_mode != ResolverMode::Layered {
            return;
        }
        let Some(ctx) = self.file_scopes.get(file_path) else {
            return;
        };
        let separator = module_separator(language);
        for function in functions.iter_mut() {
            if ctx.module_path.is_empty() {
                // module path 推不出来（root 缺失）⇒ 只给裸名限定，绝不拼假前缀。
                function.module_path = None;
                function.qualified_name = Some(function.name.clone());
            } else {
                function.module_path = Some(ctx.module_path.clone());
                function.qualified_name = Some(format!(
                    "{}{}{}",
                    ctx.module_path, separator, function.name
                ));
            }
        }
    }

    /// 从文件提取实体
    ///
    /// Phase P3 交付物 1：实现本体已上提到
    /// [`extract_file_entities`]（与 `IncrementalManager` 共用的唯一真源），这里只保留
    /// "把 ctx 装进本 parser 的 `file_scopes` 缓存"这一段属于 `CodeParser` 自己的记账。
    fn _extract_entities_from_file(&mut self, file_path: &PathBuf) -> Result<(Vec<ClassInfo>, Vec<FunctionInfo>), String> {
        let language = self._detect_language(file_path);
        let namespace = self._extract_namespace(file_path);
        let (classes, mut functions, ctx) = extract_file_entities(
            &self.ts_parser,
            file_path,
            self.resolution_root.as_deref(),
            &language,
            &namespace,
        )?;
        self.file_scopes.insert(file_path.clone(), ctx);
        // Phase P3 收口 C：增量链路也必须补节点身份，口径与 `parse_file` 完全一致。
        self._apply_layered_node_identity(file_path, &language, &mut functions);
        Ok((classes, functions))
    }

    /// 分析文件的函数调用
    ///
    /// Phase P2b 交付物 2（接线点①，增量/刷新链路）：按 `--resolver` 分叉。
    /// Phase P3 交付物 1：两档的**实现本体**都上提到 `parser.rs` 顶部的共用件
    /// （[`emit_layered_call_edges_for_file`] / [`emit_legacy_call_edges_for_file`]），
    /// 与 `IncrementalManager` 共用同一份代码 —— 不再存在第二份产边实现。
    fn _analyze_file_calls(
        &mut self,
        file_path: &PathBuf,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        if self.resolver_mode == ResolverMode::Layered {
            return self._analyze_file_calls_layered(file_path, call_graph);
        }

        // 一次性取本文件函数列表（refresh_file 已在调用本方法前写回 file_functions）。
        // 之后所有 caller/callee 查找都只在这一个切片上做，复杂度 O(calls × k)。
        let funcs: &[FunctionInfo] = self
            .file_functions
            .get(file_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        emit_legacy_call_edges_for_file(&self.ts_parser, file_path, funcs, call_graph)
    }

    /// Phase P2b 交付物 2（接线点①的 layered 分支）：刷新/增量链路里单文件产边。
    ///
    /// 与全量路径 [`CodeParser::_analyze_petgraph_call_relations_layered`] 用**同一套**
    /// 引擎入口与同一个发射器，差别只在范围：这里只处理一个文件，
    /// 索引必须**从当前图**重建（图里已经有别的文件的节点，候选池才是完整的）。
    ///
    /// 这正是"全量 vs 增量结果一致"（交付物 4 的 harness）能够成立的前提：
    /// 两条链路对同一个调用点跑的是同一段判定代码。
    ///
    /// Phase P3 交付物 1：判定代码本体已上提到 [`emit_layered_call_edges_for_file`]，
    /// 本方法只负责供给"只有 `CodeParser` 才有的东西"（ctx 缓存、`file_functions` 切片、
    /// `--levels=` 配置）。
    fn _analyze_file_calls_layered(
        &mut self,
        file_path: &PathBuf,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String> {
        // ctx：优先用 `_extract_entities_from_file` 刚写进 `file_scopes` 的那份；
        // 拿不到（例如缓存被清理）就按需补建，绝不"降级成不需要 ctx 的层"。
        let ctx = self
            .file_scope_ctx(file_path)
            .or_else(|| self.ensure_file_scope_ctx(file_path));

        let config = self.resolution_config().unwrap_or_else(ResolutionConfig::all);

        let funcs: Vec<FunctionInfo> = self
            .file_functions
            .get(file_path)
            .cloned()
            .unwrap_or_default();

        // `emit_layered_call_edges_for_file` 需要 ctx（它从 ctx 取文件路径/语言/import 表）。
        // ctx 建不出来意味着这个文件本轮无法解析 —— 与 legacy 里 `parse_file` 失败
        // 直接 `Err` 上抛的口径一致，这里同样上抛错误，**绝不静默产 0 条边**。
        let ctx = match ctx {
            Some(c) => c,
            None => {
                return Err(format!(
                    "Failed to build FileScopeCtx for {} (layered resolver)",
                    file_path.display()
                ))
            }
        };

        emit_layered_call_edges_for_file(
            &self.ts_parser,
            file_path,
            &ctx,
            &funcs,
            call_graph,
            self.resolution_root.as_deref(),
            &config,
        )
    }

    /// Phase P2b：分层引擎的"单个最优答案"入口（接线点②③的共用薄调用）。
    ///
    /// 与 layered 发射器的区别只在**用法**：这里服务于"只能返回一个 id"的既有签名
    /// （`_resolve_callee_function` / `_resolve_qualified_function_name`），
    /// layered 发射器才会把全部候选落成多条边。
    ///
    /// 两条硬约束（任务交付物 2）：
    /// - 索引一律走 `build_resolution_index_from_graph`，**不读**
    ///   `FunctionNameIndex` / `_build_function_name_index` 的同名塌缩结果；
    /// - 判定完全交给 `resolve_call_with_config`，这里**没有任何 `contains` 子串匹配**，
    ///   也**没有任何 `.first()`**。
    ///
    /// `ctx` 只从 `file_scopes` 缓存读（本方法是 `&self`，不能按需补建）；缺失时引擎
    /// 自然下沉到不依赖 ctx 的层（L1/L7/L8），**结果是"更保守"而不是"更错"**。
    fn _layered_callee_first(
        &self,
        call_name: &str,
        current_file: &Path,
        functions: &[FunctionInfo],
        call_graph: &PetCodeGraph,
    ) -> Option<Uuid> {
        let config = self.resolution_config().unwrap_or_else(ResolutionConfig::all);
        // 行号：调用点一定落在本文件某个函数体内，取切片首个节点的起始行作为
        // "最不坏"的归属行（真正影响判定的只有 `(file, line_start)` 对齐与 ctx 是否存在，
        // 与 legacy 的"完全不看行号"相比已经更多信息）。
        let line = functions.first().map(|f| f.line_start).unwrap_or(1);
        let language = functions
            .first()
            .map(|f| f.language.clone())
            .unwrap_or_else(|| self._detect_language(current_file));
        let ctx = self.file_scope_ctx(current_file);
        let per_file: HashMap<PathBuf, Arc<FileScopeCtx>> = ctx
            .clone()
            .into_iter()
            .map(|c| (current_file.to_path_buf(), c))
            .collect();
        let index =
            build_resolution_index_from_graph(call_graph, &per_file, self.resolution_root.as_deref());

        let site = CallSite::new(current_file, line, call_name, &language);
        let resolution = resolve_call_with_config(&site, ctx.as_deref(), &index, &config);
        resolution
            .primary()
            .filter(|candidate| candidate.callee_id != Uuid::nil())
            .map(|candidate| candidate.callee_id)
    }

    /// 处理未解析的调用
    fn _handle_unresolved_call(
        &self,
        caller_id: &Uuid,
        call_name: &str,
        file_path: &PathBuf,
        call_line: usize,
        call_graph: &mut PetCodeGraph,
    ) {
        // 创建未解析的调用关系
        let relation = CallRelation::legacy(
            *caller_id,
            Uuid::new_v4(), // 临时ID
            "".to_string(),
            call_name.to_string(),
            file_path.clone(),
            file_path.clone(),
            call_line,
            false,
        );

        // Phase P2b 收口（口径 B）：刷新链路上的幽灵边同样走唯一写入入口。
        if call_graph
            .upsert_call_site_edges(caller_id, call_line, call_name, vec![relation])
            == 0
        {
            warn!("Failed to add unresolved call relation: {}", call_name);
        }
    }

    /// 移除文件相关的所有实体
    fn _remove_file_entities(
        &mut self,
        file_path: &PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) {
        // 获取文件的所有实体ID
        let entity_ids = self.file_index.get_all_entity_ids(file_path);
        let index_function_ids = self.file_index.get_all_function_ids(file_path);
        let _class_ids = self.file_index.get_all_class_ids(file_path);

        // 从图中移除
        for entity_id in entity_ids {
            entity_graph.remove_entity(&entity_id);
        }

        // Phase P2b 收口（真缺陷 1 的根因修复）：**"本文件的函数节点"必须以图为准**。
        //
        // 改造前这里只认 `self.file_index`，而 `file_index` 只有 `refresh_file` 这条链路会写
        // （`file_index.rebuild_for_file`）；`parse_file` / `build_petgraph_code_graph`
        // （也就是真实 CLI 的第一轮建图）**从不填它**。于是对"上一轮由全量建图写进图的节点"
        // 做一次 `refresh_file`，清理循环遍历的是**空列表** ⇒ 旧节点一个都没删掉，
        // 紧接着 `add_function` 用新 Uuid 又插入一份 ⇒ 同一个文件出现两套函数节点，
        // 同一个调用点各产一条边（`add_call_relation` 按 `(caller, callee, line)` 去重，
        // caller 是两个不同 uuid，于是去重完全失效）。
        // 实测：`cross_file_same_name/gamma.rs:6 call_helper -> helper` 增量刷新 2 条、
        // 从零重建 1 条（`tests/test_p2b_wiring.rs::p2b_full_vs_incremental_refresh_is_consistent`）。
        //
        // 修法：三处来源取并集，逐个走索引安全的删除原语。
        //  (a) `file_index`（刷新链路自己写的）
        //  (b) `self.file_functions`（同一 parser 实例内做过全量建图的）
        //  (c) **图自身的按文件反向索引**（跨进程、从 `graph.bin` 读回来的增量场景 ——
        //      `PetGraphStorage` 会重建 `file_functions`，见 `remove_functions_by_file`）
        let mut stale_function_ids: Vec<Uuid> = index_function_ids;
        if let Some(parser_side) = self.file_functions.get(file_path) {
            stale_function_ids.extend(parser_side.iter().map(|f| f.id));
        }
        for function_id in stale_function_ids {
            // 必须走索引安全的删除原语：直接 remove_node 会让 NodeIndex 映射因 swap 搬移而失效
            call_graph.remove_function(&function_id);
        }
        // 兜底：图知道自己每个文件挂了哪些节点，按文件整批清理（已删掉的 id 返回 false，幂等）。
        call_graph.remove_functions_by_file(file_path);

        // 清理索引
        self.file_index.remove_file(file_path);
        self.snippet_index.clear_file_cache(file_path);
        // 必须同时清除本文件的函数索引：调用关系分析现在直接依赖 file_functions，
        // 残留的陈旧条目会产生"幽灵 caller"（指向已从图中移除的函数）。
        self.file_functions.remove(file_path);
        // Phase P1 交付物 3：`file_scopes` 与 `file_functions` 同生命周期失效
        // （残留会让 P2 读到已删除文件的旧作用域树/旧 import 绑定）。
        self.file_scopes.remove(file_path);
    }

    /// 更新代码片段索引
    fn _update_snippet_index(
        &mut self,
        file_path: &PathBuf,
        class_ids: &[Uuid],
        function_ids: &[Uuid],
        entity_graph: &EntityGraph,
    ) -> Result<(), String> {
        // 读取文件内容
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file for snippet indexing: {}", e))?;

        let lines: Vec<&str> = content.lines().collect();

        // 为类添加代码片段
        for &class_id in class_ids {
            if let Some(class) = entity_graph.get_class_by_id(&class_id) {
                let snippet_content = self._extract_code_snippet(&lines, class.line_start, class.line_end);
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: class.line_start,
                    line_end: class.line_end,
                    cached_content: Some(snippet_content),
                };
                self.snippet_index.add_snippet(class_id, snippet_info);
            }
        }

        // 为函数添加代码片段
        // 从本文件的函数索引一次性构造 id -> 行范围（O(k)），取代原来"每个 id 全仓扫描"
        // 的 O(k × N_total)。行为保持不变：只有 id 命中时才 add_snippet。
        let function_lines: HashMap<Uuid, (usize, usize)> = self
            .file_functions
            .get(file_path)
            .map(|funcs| {
                funcs
                    .iter()
                    .map(|function| (function.id, (function.line_start, function.line_end)))
                    .collect()
            })
            .unwrap_or_default();

        for &function_id in function_ids {
            if let Some((line_start, line_end)) = function_lines.get(&function_id) {
                let snippet_content = self._extract_code_snippet(&lines, *line_start, *line_end);
                let snippet_info = crate::codegraph::types::SnippetInfo {
                    file_path: file_path.clone(),
                    line_start: *line_start,
                    line_end: *line_end,
                    cached_content: Some(snippet_content),
                };
                self.snippet_index.add_snippet(function_id, snippet_info);
            }
        }

        Ok(())
    }

    /// 检测文件语言
    fn _detect_language(&self, file_path: &Path) -> String {
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            match ext.to_lowercase().as_str() {
                "rs" => "rust".to_string(),
                "py" | "py3" | "pyx" => "python".to_string(),
                "ts" | "tsx" => "typescript".to_string(),
                "js" | "jsx" => "javascript".to_string(),
                "java" => "java".to_string(),
                "cpp" | "cc" | "cxx" | "c++" | "c" | "h" | "hpp" | "hxx" | "hh" => "cpp".to_string(),
                "go" => "go".to_string(),
                _ => "unknown".to_string(),
            }
        } else {
            "unknown".to_string()
        }
    }

    /// 提取命名空间
    fn _extract_namespace(&self, file_path: &Path) -> String {
        // 从文件内容解析命名空间
        if let Ok(content) = fs::read_to_string(file_path) {
            return self._extract_namespace_from_content(&content, &file_path.to_path_buf());
        }
        "global".to_string()
    }

    /// 解析单个文件（完整实现，支持多语言）
    pub fn parse_file(&mut self, file_path: &PathBuf) -> Result<(), String> {
        info!("Parsing file: {}", file_path.display());
        
        // 检查文件是否存在
        if !file_path.exists() {
            return Err(format!("File does not exist: {}", file_path.display()));
        }

        // 使用TreeSitter解析器解析文件
        // Phase P1 交付物 2(b)：同 `_extract_entities_from_file`，改用带元数据的入口
        // （符号列表与改造前逐字节相同，只是多带出 Java 的 package 声明）。
        let ast = self.ts_parser.parse_file_with_metadata(file_path)
            .map_err(|e| format!("Failed to parse file {}: {:?}", file_path.display(), e))?;
        let declared_package = ast.declared_package;
        let symbols = ast.symbols;
        info!("TreeSitter parsing completed, found {} symbols", symbols.len());
        


        // 读取文件内容用于代码片段提取
        let file_content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;

        let language = self._detect_language(file_path);
        let namespace = self._extract_namespace_from_content(&file_content, file_path);

        // Phase P1 交付物 3（**只建不消费**）：与 `_extract_entities_from_file` 同一套产出，
        // 只读 `symbols`，不产生任何 FunctionInfo/CallRelation，也不参与 callee 解析。
        // 放在下面的 match 之前，是为了让这个新增分支与既有三类 symbol 的处理完全解耦。
        let ctx = build_file_scope_ctx(
            file_path.as_path(),
            &symbols,
            self.resolution_root.as_deref(),
            &language,
            declared_package,
        );
        self.file_scopes.insert(file_path.clone(), ctx);

        let mut functions = Vec::new();
        let mut classes = Vec::new();
        let mut function_calls = Vec::new();

        // 分析每个AST符号
        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();
            debug!("Found symbol: {:?} - {}", symbol_ref.symbol_type(), symbol_ref.name());

            match symbol_ref.symbol_type() {
                crate::codegraph::treesitter::structs::SymbolType::FunctionDeclaration => {
                    // 提取函数信息
                    let function = self._extract_function_info(symbol_ref, file_path, &namespace, &language);
                    functions.push(function);
                },
                crate::codegraph::treesitter::structs::SymbolType::StructDeclaration => {
                    // 提取类/结构体信息
                    let class = self._extract_class_info(symbol_ref, file_path, &language, &namespace);
                    classes.push(class);
                },
                crate::codegraph::treesitter::structs::SymbolType::FunctionCall => {
                    // 提取函数调用信息
                    let call_info = self._extract_function_call_info(symbol_ref, file_path);
                    function_calls.push(call_info);
                },
                _ => {}
            }
        }

        // Phase P2b 交付物 1：layered 路径给 `FunctionInfo` 补上节点身份
        // （`module_path` 来自 `FileScopeCtx`，`qualified_name` = `module_path + sep + name`）。
        // Phase P3 收口 C：本体下提到 [`CodeParser::_apply_layered_node_identity`]，
        // 与增量链路 `_extract_entities_from_file` **共用同一份实现**（否则刷新过的文件
        // 会丢掉节点身份，增量图与"同一份源码从零重建"不再等价）。
        self._apply_layered_node_identity(file_path, &language, &mut functions);

        // 注册函数到全局注册表
        for function in &functions {
            self.function_registry.insert(function.name.clone(), function.clone());
        }
        
        // 保存文件函数映射
        self.file_functions.insert(file_path.clone(), functions.clone());

        // 更新代码片段索引
        self._update_snippet_index_with_content(file_path, &functions, &classes, &file_content)?;

        info!("Successfully parsed file: {} ({} functions, {} classes, {} calls)", 
              file_path.display(), functions.len(), classes.len(), function_calls.len());
        
        Ok(())
    }

    /// 从AST符号提取函数信息
    fn _extract_function_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        file_path: &PathBuf,
        namespace: &str,
        language: &str,
    ) -> FunctionInfo {
        let name = symbol.name().to_string();
        let line_start = symbol.full_range().start_point.row + 1;
        let line_end = symbol.full_range().end_point.row + 1;
        
        // 尝试提取函数签名
        let signature = self._extract_function_signature(symbol);

        FunctionInfo::legacy(
            Uuid::new_v4(),
            name,
            file_path.clone(),
            line_start,
            line_end,
            namespace.to_string(),
            language.to_string(),
            signature,
        )
    }

    /// 从AST符号提取类信息
    fn _extract_class_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        file_path: &PathBuf,
        language: &str,
        namespace: &str,
    ) -> ClassInfo {
        let name = symbol.name().to_string();
        let range = symbol.full_range();
        let line_start = range.start_point.row + 1;
        let line_end = range.end_point.row + 1;

        // 根据语言确定类类型
        let class_type = match language {
            "rust" => ClassType::Struct,
            "cpp" | "java" | "typescript" | "javascript" => ClassType::Class,
            _ => ClassType::Class,
        };

        ClassInfo {
            id: Uuid::new_v4(),
            name,
            file_path: file_path.clone(),
            line_start,
            line_end,
            namespace: namespace.to_string(),
            language: language.to_string(),
            class_type,
            parent_class: None, // 需要进一步解析继承关系
            implemented_interfaces: vec![],
            member_functions: vec![],
            member_variables: vec![],
        }
    }

    /// 从AST符号提取函数调用信息
    fn _extract_function_call_info(
        &self,
        symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance,
        _file_path: &PathBuf,
    ) -> (String, usize) {
        let call_name = symbol.name().to_string();
        let range = symbol.full_range();
        let line_number = range.start_point.row + 1;
        
        (call_name, line_number)
    }

    /// 提取函数签名
    fn _extract_function_signature(&self, symbol: &dyn crate::codegraph::treesitter::ast_instance_structs::AstSymbolInstance) -> Option<String> {
        // 使用声明范围来获取函数签名
        let decl_range = symbol.declaration_range();
        let full_range = symbol.full_range();
        
        // 尝试从声明范围提取签名
        if decl_range.start_point.row != full_range.start_point.row || 
           decl_range.end_point.row != full_range.end_point.row {
            // 如果声明范围与完整范围不同，说明有更精确的签名信息
            let signature = format!("{}()", symbol.name());
            return Some(signature);
        }
        
        // 否则返回函数名作为签名
        Some(symbol.name().to_string())
    }

    fn _extract_namespace_from_content(&self, content: &str, file_path: &PathBuf) -> String {
        let language = self._detect_language(file_path);
        
        match language.as_str() {
            "rust" => {
                // 查找mod声明
                for line in content.lines() {
                    if line.trim().starts_with("mod ") {
                        if let Some(name) = line.trim().split_whitespace().nth(1) {
                            return name.to_string();
                        }
                    }
                }
                "crate".to_string()
            },
            "python" => {
                // 查找包名或模块名
                for line in content.lines() {
                    if line.trim().starts_with("__package__") {
                        if let Some(name) = line.split('=').nth(1) {
                            return name.trim().trim_matches('"').trim_matches('\'').to_string();
                        }
                    }
                }
                "global".to_string()
            },
            "java" => {
                // 查找package声明
                for line in content.lines() {
                    if line.trim().starts_with("package ") {
                        if let Some(package) = line.trim().split_whitespace().nth(1) {
                            return package.trim_end_matches(';').to_string();
                        }
                    }
                }
                "default".to_string()
            },
            "cpp" => {
                // 查找namespace声明
                for line in content.lines() {
                    if line.trim().starts_with("namespace ") {
                        if let Some(name) = line.trim().split_whitespace().nth(1) {
                            return name.to_string();
                        }
                    }
                }
                "global".to_string()
            },
            _ => "global".to_string(),
        }
    }

    /// 更新代码片段索引（包含真实代码内容）
    fn _update_snippet_index_with_content(
        &mut self,
        file_path: &PathBuf,
        functions: &[FunctionInfo],
        classes: &[ClassInfo],
        file_content: &str,
    ) -> Result<(), String> {
        let lines: Vec<&str> = file_content.lines().collect();

        // 为函数添加代码片段
        for function in functions {
            let snippet_content = self._extract_code_snippet(&lines, function.line_start, function.line_end);
            
            let snippet_info = crate::codegraph::types::SnippetInfo {
                file_path: file_path.clone(),
                line_start: function.line_start,
                line_end: function.line_end,
                cached_content: Some(snippet_content),
            };
            
            self.snippet_index.add_snippet(function.id, snippet_info);
        }

        // 为类添加代码片段
        for class in classes {
            let snippet_content = self._extract_code_snippet(&lines, class.line_start, class.line_end);
            
            let snippet_info = crate::codegraph::types::SnippetInfo {
                file_path: file_path.clone(),
                line_start: class.line_start,
                line_end: class.line_end,
                cached_content: Some(snippet_content),
            };
            
            self.snippet_index.add_snippet(class.id, snippet_info);
        }

        Ok(())
    }

    /// 提取代码片段内容
    fn _extract_code_snippet(&self, lines: &[&str], start_line: usize, end_line: usize) -> String {
        let start_idx = (start_line - 1).min(lines.len());
        let end_idx = end_line.min(lines.len());
        
        if start_idx >= end_idx {
            return String::new();
        }
        
        lines[start_idx..end_idx].join("\n")
    }

    /// 解析目录下的所有文件
    pub fn parse_directory(&mut self, dir: &Path) -> Result<(), String> {
        let files = self.scan_directory(dir);
        info!("Found {} files to parse", files.len());

        for file in files {
            if let Err(e) = self.parse_file(&file) {
                warn!("Failed to parse {}: {}", file.display(), e);
            }
        }

        Ok(())
    }

    /// 构建完整的代码图（**Phase P3 交付物 1：只读投影，不再自己产边**）
    ///
    /// 改造前这里有一条与 `build_petgraph_code_graph` **并行**的产边链路
    /// （`_analyze_call_relations` → `_analyze_file_call_relations` →
    /// `CodeGraph::add_call_relation`），它是 P2b 收口登记的"第二套边写入逻辑"，
    /// 有两个致命后果：
    ///  1. 它**完全不读** `resolver_mode` ⇒ CLI 的 `--resolver=layered` 在真实入口上是空转的
    ///     （`Commands::Init` → `services::CodeAnalyzer::analyze_directory` → 本方法）；
    ///  2. 它绕过 `PetCodeGraph::upsert_call_site_edges` 这个唯一写入口，
    ///     同一份源码在两条链路上会得到不同的边。
    ///
    /// P3 的处置：**保留本方法的签名与返回类型**（`CodeAnalyzer` 的十余个查询方法、
    /// `tests/test_functional.rs`、`main.rs` 都依赖 `&CodeGraph`），但实现改为
    /// "委托 `build_petgraph_code_graph`（唯一产边链路，legacy/layered 由 `--resolver` 决定）
    ///  → 把 PetCodeGraph 投影成 CodeGraph 只读视图"。
    /// 于是全仓只剩**一套** AST→边的实现，`CodeGraph` 退化为纯查询/导出容器。
    pub fn build_code_graph(&mut self, dir: &Path) -> Result<CodeGraph, String> {
        let pet_graph = self.build_petgraph_code_graph(dir)?;
        Ok(project_petgraph_to_code_graph(&pet_graph))
    }

    /// 构建基于petgraph的代码图（增量构建）
    pub fn build_petgraph_code_graph(&mut self, dir: &Path) -> Result<PetCodeGraph, String> {
        // Phase P1 交付物 3：记下本轮仓库根，供 `file_scopes` 的 module path 推导使用。
        // 这里**不** clear `file_scopes` —— 与下面 `file_functions` 的增量语义保持一致
        // （本函数不清空 file_functions，内容未变的文件会被 `_should_skip_file` 跳过而不再解析，
        // 清空就意味着这些文件在本轮彻底没有 ctx）。
        // TODO(P2)：ctx 需要持久化（与 graph.bin 同一套 schema_version 门禁），
        // 或在 root 变化时按 (root, file) 重新推导 module path，否则跨项目复用 parser 实例时
        // 旧条目的 module path 会按旧根计算。
        self.resolution_root = Some(dir.to_path_buf());

        // 1. 尝试从本地数据库加载现有的图
        let mut code_graph = self._load_existing_graph(dir)?;
        let has_existing_data = code_graph.is_some();
        
        if let Some(ref mut existing_graph) = code_graph {
            info!("Loaded existing graph with {} functions", existing_graph.get_stats().total_functions);
        } else {
            info!("No existing graph found, starting fresh analysis");
            code_graph = Some(PetCodeGraph::new());
        }
        
        let mut code_graph = code_graph.unwrap();
        
        // 2. 扫描目录下的所有文件
        let files = self.scan_directory(dir);
        info!("Found {} files to process", files.len());
        
        // 3. 加载文件哈希值（**仅当图缓存存在时**）
        //
        // Phase P2b 口径 D 修复：哈希缓存与图缓存必须**同生同死**。
        //
        // 根因：本函数第 8 步把 MD5 写进 `file_hashes.json`，而**图**由
        // `codegraph::repository::save_state()` 在别处落盘。于是存在一种真实状态：
        // "哈希已落盘、图没落盘"（例如首次 `init` 走的是 `build_petgraph_code_graph`
        // 但没走到 `save_state`，或图文件被单独清理/版本门禁判失效）。下一轮：
        //   第 1 步读不到图 → `has_existing_data = false` → 走"全量"分支，
        //   但第 3 步仍然读到上一轮的哈希 → 第 4 步 `_should_skip_file` 对**每个**文件
        //   都返回 true → `self.file_functions` 为空 → 第 5 步"全量分支"往空图里
        //   添加 0 个函数 ⇒ **空图**（实测 `nested_function_caller`：5 节点/3 边 → 0/0）。
        //
        // 修法与本文件 `build_code_graph()`（CodeGraph 链路）里已有的写法对齐：
        // 那里第 3 步就是 `if has_existing_data { … } else { HashMap::new() }`，
        // 只有 petgraph 链路漏了这道守卫。语义后果：图缺失 ⇒ 一律全量重解析，
        // 正确性不再依赖"缓存是否命中"（口径 C 的要求）。
        let mut file_hashes: HashMap<String, String> = if has_existing_data {
            self._load_file_hashes(dir)?
        } else {
            info!("图缓存缺失：无视文件哈希缓存，本轮全量重新解析（P2b 口径 D 修复）");
            HashMap::new()
        };
        
        // 4. 逐个处理文件，检查是否需要重新解析
        let mut processed_files = 0;
        let mut skipped_files = 0;
        
        for file_path in files {
            if self._should_skip_file(&file_path, &mut file_hashes)? {
                skipped_files += 1;
                continue;
            }
            
            if let Err(e) = self.parse_file(&file_path) {
                warn!("Failed to parse {}: {}", file_path.display(), e);
            } else {
                processed_files += 1;
            }
        }
        
        info!("File processing completed: {} processed, {} skipped", processed_files, skipped_files);
        // Phase P2b 收口（口径 A）：跳过契约的直接观测点（同 `build_code_graph`）。
        // 注意 `ensure_file_scope_ctx` 的按需补建**不会**增加这两个计数：它只写 `file_scopes`，
        // 不重新解析实体、不产边（该口径由 `tests/test_p2b_wiring.rs` 钉死）。
        self.last_round_parsed = processed_files;
        self.last_round_skipped = skipped_files;
        
        // 5. 如果这是增量构建，需要合并新解析的函数
        if has_existing_data {
            self._merge_new_functions(&mut code_graph);
        } else {
            // 全量构建：直接添加所有函数
            //
            // Phase P3 收口 C 补强：`self.file_functions` 是 `HashMap`，直接 `for (…)` 迭代
            // 的顺序 = 每进程随机种子。它决定函数节点进入 `code_graph` 的**先后**，进而
            // 决定 `function_names[name]` / `get_all_functions()` 的 Vec 顺序。虽然解析器与
            // 索引都对这些顺序做了排序，但"节点入图顺序随种子"仍是幽灵复用与稳定排序
            // 的隐性依赖源 ⇒ 这里显式按文件路径排序，让入图顺序与种子彻底无关。
            let mut ordered_files: Vec<&PathBuf> = self.file_functions.keys().collect();
            ordered_files.sort();
            for file_path in ordered_files {
                let functions = &self.file_functions[file_path];
                for function in functions {
                    code_graph.add_function(function.clone());
                }
            }
        }
        
        // 6. 分析调用关系
        self._analyze_petgraph_call_relations(&mut code_graph);
        
        // 7. 更新统计信息
        code_graph.update_stats();
        
        // 8. 保存新的文件哈希值
        self._save_file_hashes(dir, &file_hashes)?;
        
        Ok(code_graph)
    }

    /// 尝试从本地数据库加载现有的图
    ///
    /// 两道门禁都在这里生效，且**只走同一条降级链**：
    /// 1. schema 版本（`PetGraphStorageManager::read_storage_from_*` 内的
    ///    `validate_schema_version`，不符 ⇒ `Err`）；
    /// 2. **解析档位戳**（`validate_resolution_stamp`）：缓存必须与本轮
    ///    `self.resolver_mode` / `self.max_candidates` 完全一致，否则判定失效。
    /// 任何一道不过 ⇒ 丢弃已读到的图 ⇒ `Err → warn! → Ok(None)` ⇒
    /// 由 `build_petgraph_code_graph` 用 `PetCodeGraph::new()` 全量重建，绝不 panic。
    fn _load_existing_graph(&self, dir: &Path) -> Result<Option<PetCodeGraph>, String> {
        // --force-reindex：一律报告"没有已有图"。这与"缓存版本不符 → Err → warn → Ok(None)"
        // 走的是同一条失效路径，最终由 `build_petgraph_code_graph` 用 PetCodeGraph::new() 重建。
        if self.force_reindex {
            info!("--force-reindex 已置位：跳过 PetCodeGraph 缓存，强制全量重建");
            return Ok(None);
        }

        use crate::storage::petgraph_storage::clamp_max_candidates;
        use crate::storage::PersistenceManager;
        
        // 使用项目路径作为项目ID
        let project_id = dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default");
        
        // 期望档位取自**本解析器自己的配置**（它才是这批边的生产者），
        // 且必须与写侧 `from_petgraph` 用同一个折算函数，否则大小写/截断口径会造出假失效。
        let expected_mode = self.resolver_mode.as_str();
        let expected_max_candidates = clamp_max_candidates(self.max_candidates);

        // 把"读缓存"与"档位门禁"合成成同一个 `Result<Option<PetGraphStorage>, String>`，
        // 这样两类失效共用下面**唯一**一条 `Err → warn → Ok(None)` 降级分支，
        // 不再新造第二条降级路径（任务要求：复用既有 warn 链，别新造一条）。
        let gated = PersistenceManager::new()
            .load_graph_storage(project_id)
            .map_err(|e| e.to_string())
            .and_then(|cached| match cached {
                Some(storage) => {
                    if let Err(reason) = storage.validate_resolution_stamp(expected_mode, expected_max_candidates) {
                        return Err(reason);
                    }
                    Ok(Some(storage))
                }
                None => Ok(None),
            });

        match gated {
            Ok(Some(storage)) => Ok(Some(storage.to_petgraph())),
            Ok(None) => Ok(None),
            Err(e) => {
                warn!("Failed to load existing graph: {}", e);
                Ok(None)
            }
        }
    }

    /// 加载文件哈希值
    fn _load_file_hashes(&self, dir: &Path) -> Result<HashMap<String, String>, String> {
        // --force-reindex：返回空哈希表 → `_should_skip_file` 对每个文件都判定"需要重新解析"，
        // 因此本次一定会完整解析目录，并在第 8 步 `_save_file_hashes` 写出全新的哈希。
        if self.force_reindex {
            info!("--force-reindex 已置位：跳过文件 MD5 哈希缓存，强制重新解析全部文件");
            return Ok(HashMap::new());
        }

        use crate::storage::PersistenceManager;
        use md5;
        
        let persistence = PersistenceManager::new();
        
        // 尝试多种方式的项目ID
        let project_ids = vec![
            // 1. 使用目录名（原始方式）
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default")
                .to_string(),
            // 2. 使用目录路径的MD5哈希（HTTP接口方式）
            format!("{:x}", md5::compute(dir.to_string_lossy().as_bytes())),
        ];
        
        for project_id in project_ids {
            match persistence.load_file_hashes(&project_id) {
                Ok(hashes) => {
                    if !hashes.is_empty() {
                        info!("Loaded {} file hashes for project ID: {}", hashes.len(), project_id);
                        return Ok(hashes);
                    }
                },
                Err(e) => {
                    debug!("Failed to load file hashes for project ID {}: {}", project_id, e);
                    continue;
                }
            }
        }
        
        info!("No file hashes found for any project ID");
        Ok(HashMap::new())
    }

    /// 保存文件哈希值
    fn _save_file_hashes(&self, dir: &Path, hashes: &HashMap<String, String>) -> Result<(), String> {
        use crate::storage::PersistenceManager;
        use md5;
        
        let persistence = PersistenceManager::new();
        
        // 使用目录路径的MD5哈希作为项目ID（与HTTP接口保持一致）
        let project_id = format!("{:x}", md5::compute(dir.to_string_lossy().as_bytes()));
        info!("Saving file hashes for project ID: {}", project_id);
        
        // 为每个文件保存哈希值
        for (file_path, hash) in hashes {
            if let Err(e) = persistence.save_file_hash(&project_id, file_path, hash) {
                warn!("Failed to save hash for {}: {}", file_path, e);
            }
        }
        
        Ok(())
    }

    /// 检查文件是否应该跳过（基于MD5哈希值）
    fn _should_skip_file(&self, file_path: &PathBuf, file_hashes: &mut HashMap<String, String>) -> Result<bool, String> {
        use std::fs;
        use md5;
        
        // 计算当前文件的MD5哈希值
        let content = fs::read(file_path)
            .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
        let current_hash = format!("{:x}", md5::compute(&content));
        
        // 获取相对文件路径（相对于当前工作目录）
        let file_path_str = file_path.to_string_lossy().to_string();
        
        // 检查是否存在相同的哈希值
        if let Some(saved_hash) = file_hashes.get(&file_path_str) {
            if saved_hash == &current_hash {
                debug!("File {} unchanged (MD5: {}), skipping", file_path.display(), current_hash);
                return Ok(true);
            }
        }
        
        // 更新哈希值
        file_hashes.insert(file_path_str, current_hash);
        Ok(false)
    }

    /// 合并新解析的函数到现有图中
    ///
    /// 复杂度：与 `_merge_new_functions_to_code_graph` 同一缺陷 ——
    /// 原实现对每个待合并函数调用一次 `graph.node_weights()` 线性扫描，
    /// 即 O(k_new × N_total)；现在一次遍历预建去重集合后 O(1) 查表，
    /// 整体 O(k_new + N_total)。去重键 `(file_path, line_start, line_end)` 未变。
    fn _merge_new_functions(&self, code_graph: &mut PetCodeGraph) {
        // 一次性遍历现有图构造去重集合（原实现是在每个候选函数的循环内做全图扫描）
        let mut seen: HashSet<FunctionKey> = code_graph
            .graph
            .node_weights()
            .map(|existing_func| {
                (existing_func.file_path.clone(), existing_func.line_start, existing_func.line_end)
            })
            .collect();

        for functions in self.file_functions.values() {
            for function in functions {
                let key = (function.file_path.clone(), function.line_start, function.line_end);

                // 检查函数是否已存在（基于文件路径和行号）
                if !seen.contains(&key) {
                    code_graph.add_function(function.clone());
                    // 必须回填：原实现里 add_function 会立刻改变图内容，
                    // 因此同批次内的重复三元组会被判为 exists 而只添加一次。
                    seen.insert(key);
                }
            }
        }
    }

    /// 预建全仓函数名索引（每轮调用关系分析 pass 只构建一次）
    ///
    /// ⚠ **Phase P3 交付物 1 的定位声明：这里只做 legacy 逃生口，layered 路径禁止使用。**
    /// - 唯一消费者是 `_analyze_petgraph_call_relations`（`--resolver=legacy` 的全量档），
    ///   以及它下游的 `_resolve_callee_function` / `_resolve_qualified_function_name`；
    /// - `layered` 链路的索引一律来自
    ///   `resolution::build_resolution_index_from_graph`（**不塌缩**、按 `(level, rank, uuid)`
    ///   稳定排序），绝不允许读这张表 —— 否则歧义调用点又会退化成"任意命中一个"。
    ///   这条禁令由 `tests/test_p1_edges_unchanged.rs::p2b_engine_entries_are_referenced_only_from_layered_branches`
    ///   的 `legacy_only_fns` 反向守卫钉住（本函数体内出现任何引擎入口即报错）。
    ///
    /// first-wins 语义与现状等价：原 `_find_function_by_name_global` 遍历的是
    /// `HashMap<PathBuf, Vec<FunctionInfo>>`，同名函数存在多个候选时返回哪一个取决于
    /// HashMap 的迭代顺序 —— 本身就是不确定的，所以这里不要求稳定顺序，只做
    /// "已存在的键不覆盖"（`or_insert`）以保持"任意命中一个"的既有行为。
    /// 也正因为这个不确定性，跨两次独立 legacy 建图的比较必须丢掉 callee 身份维度
    /// （见 `tests/fixtures/resolution/README.md:74`）。
    fn _build_function_name_index(&self) -> FunctionNameIndex<'_> {
        let total_functions: usize = self.file_functions.values().map(|funcs| funcs.len()).sum();
        let mut index: FunctionNameIndex<'_> = HashMap::with_capacity(total_functions);
        for functions in self.file_functions.values() {
            for function in functions {
                index.entry(function.name.as_str()).or_insert(function);
            }
        }
        index
    }

    /// 查找调用者函数（按行号）
    fn _find_caller_function_by_line(
        &self,
        file_path: &PathBuf,
        call_line: usize,
        functions: &[FunctionInfo]
    ) -> Option<usize> {
        // 查找包含调用行的函数
        for (idx, function) in functions.iter().enumerate() {
            if function.file_path == *file_path && 
               call_line >= function.line_start && 
               call_line <= function.line_end {
                return Some(idx);
            }
        }
        None 
    }

    /// 根据函数名查找函数（在预建名称索引中 O(1) 查找）
    ///
    /// 命中语义与原全仓扫描等价：同名多候选时返回索引中已有的那一个（first-wins，
    /// 见 `_build_function_name_index` 的注释）；原实现依赖 HashMap 迭代顺序，本来
    /// 就不保证稳定顺序，因此这里同样不引入排序。
    fn _find_function_by_name<'a>(&self, name_index: &FunctionNameIndex<'a>, name: &str) -> Option<&'a FunctionInfo> {
        name_index.get(name).copied()
    }

    /// 全局查找函数名（跨文件），从预建名称索引取值
    fn _find_function_by_name_global(&self, name_index: &FunctionNameIndex<'_>, name: &str) -> Option<FunctionInfo> {
        self._find_function_by_name(name_index, name).cloned()
    }

    /// 分析petgraph调用关系（完整实现）
    ///
    /// Phase P2b 交付物 2：按 `--resolver` 分叉。
    /// - `Legacy`（默认）：下面的原有四段降级逻辑**一字不改**（红线 1，逐字节产边）。
    /// - `Layered`：走 [`CodeParser::_analyze_petgraph_call_relations_layered`]，它用分层引擎
    ///   产边，并把"解析层级 / 置信度 / 歧义候选 / 候选总数"写成边的一等属性（口径 A/B）。
    fn _analyze_petgraph_call_relations(&mut self, code_graph: &mut PetCodeGraph) {
        // 接线点①（全量 petgraph 主路径）：layered 直接交给分层发射器，legacy 保持原样。
        if self.resolver_mode == ResolverMode::Layered {
            self._analyze_petgraph_call_relations_layered(code_graph);
            return;
        }

        info!("Starting petgraph call relation analysis for {} files", self.file_functions.len());
        
        // 预建一次名称索引，避免 `_resolve_callee_function` 在每个调用点上做全仓双重扫描
        let name_index = self._build_function_name_index();

        let mut total_calls = 0;
        let mut resolved_calls = 0;
        let mut unresolved_calls = 0;
        
        // 遍历每个文件的函数
        for (file_path, functions) in &self.file_functions {
            if functions.is_empty() {
                continue;
            }
            
            // 使用TreeSitter解析器分析文件中的函数调用
            match self.ts_parser.parse_file(file_path) {
                Ok(symbols) => {
                    let file_calls = self._analyze_file_calls_for_petgraph(
                        &symbols, 
                        functions, 
                        code_graph,
                        file_path,
                        &name_index
                    );
                    total_calls += file_calls.total;
                    resolved_calls += file_calls.resolved;
                    unresolved_calls += file_calls.unresolved;
                },
                Err(e) => {
                    warn!("Failed to parse file {} for call analysis: {:?}", file_path.display(), e);
                    // 即使解析失败，也尝试基于函数名的简单分析
                    self._fallback_call_analysis(functions, code_graph);
                }
            }
        }
        
        info!("Call analysis completed: {} total calls, {} resolved, {} unresolved", 
              total_calls, resolved_calls, unresolved_calls);
    }
    
    // ============================================================================================
    // Phase P2b 交付物 2：layered 发射器（把 P2a 的分层引擎接进真实建图链路）。
    //
    // 与 legacy 主路径的三点本质差异：
    //   1. **caller 归属**用 `ScopeTree::innermost_scope_containing`（口径 (iii)），
    //      替换 `_find_caller_function_by_line` 的"行区间 first-wins"，闭包/嵌套函数
    //      不再被误归外层；
    //   2. **callee 解析**用 `resolve_call_with_config`（L1..L8 分层 + 支配规则），
    //      索引来自 `build_resolution_index_from_graph`，**不经过**
    //      `_build_function_name_index` 的同名塌缩，也**不使用**任何 `contains` 子串匹配；
    //   3. **产边**按口径 A（每候选一条边）与口径 B（Unresolved 合成与 legacy 同形态的
    //      幽灵节点+幽灵边，保证"有边的调用点数"覆盖中性）。
    //
    // 确定性：文件按路径排序遍历、调用点由 `call_sites_from_symbols_with_source` 内部
    // 按 `(line, callee_name, guid)` 排序、候选顺序经 `rank_candidates`
    // （最终键 `(level, rank, uuid)`，且 `source_key` 在 uuid 之前），
    // 因此同一份源码两次建图的边多重集（含 4 个新字段）逐条相等。
    // ============================================================================================
    fn _analyze_petgraph_call_relations_layered(&mut self, code_graph: &mut PetCodeGraph) {
        // 配置：`--levels=` 非法时 CLI 已在建图前拦截；这里再兜一层，绝不用 unwrap。
        let config = self.resolution_config().unwrap_or_else(ResolutionConfig::all);
        let max_candidates = config.max_candidates.max(1) as usize;

        // 文件顺序必须确定（`file_functions` 是 HashMap，迭代顺序 = 每进程随机种子）。
        let mut files: Vec<PathBuf> = self.file_functions.keys().cloned().collect();
        files.sort();

        // 为每个文件补齐 ctx：增量链路里被 MD5 跳过的文件没有 ctx，
        // 而口径 C 要求"正确性绝不依赖缓存是否命中"⇒ 缺了就按需补建。
        let mut per_file: HashMap<PathBuf, Arc<FileScopeCtx>> = HashMap::new();
        for file in &files {
            if let Some(ctx) = self.ensure_file_scope_ctx(file) {
                per_file.insert(file.clone(), ctx);
            }
        }

        // 索引只从"图节点 + 可用 ctx"构建（不塌缩同名候选）。
        let index = build_resolution_index_from_graph(
            code_graph,
            &per_file,
            self.resolution_root.as_deref(),
        );

        let mut total_calls = 0usize;
        let mut resolved_calls = 0usize;
        let mut ambiguous_calls = 0usize;
        let mut unresolved_calls = 0usize;

        for file in &files {
            let Some(ctx) = per_file.get(file).cloned() else {
                // ctx 建不出来 = 该文件解析失败，与 legacy 的 `parse_file` 失败口径一致。
                continue;
            };
            let symbols = match self.ts_parser.parse_file(file) {
                Ok(symbols) => symbols,
                Err(e) => {
                    warn!(
                        "Failed to parse file {} for layered call analysis: {:?}",
                        file.display(),
                        e
                    );
                    continue;
                }
            };
            // P2a 口径①：生产路径必须走 `_with_source` 变体 —— AST 的 `namespace` 对调用
            // 符号恒为空串，限定名前缀（`mod_a::pick` / `mod_y.helper`）只能从源码文本还原。
            let source = fs::read_to_string(file).ok();
            let sites =
                call_sites_from_symbols_with_source(&symbols, ctx.as_ref(), source.as_deref());
            // 本文件的函数节点快照（克隆出来，避免与 `&mut self` / 图的可变借用打架）。
            let funcs: Vec<FunctionInfo> = self
                .file_functions
                .get(file)
                .cloned()
                .unwrap_or_default();

            for site in sites {
                total_calls += 1;
                // (iii) caller 归属：innermost 优先，再沿作用域链向外回退到"能对齐图节点"的层。
                let Some(caller) = self._layered_caller_node(&site, ctx.as_ref(), &funcs) else {
                    continue;
                };
                let resolution =
                    resolve_call_with_config(&site, Some(ctx.as_ref()), &index, &config);
                match resolution.status {
                    ResolutionStatus::Resolved => resolved_calls += 1,
                    ResolutionStatus::Ambiguous => ambiguous_calls += 1,
                    ResolutionStatus::Unresolved => unresolved_calls += 1,
                }
                self._emit_layered_resolution(
                    &site,
                    &caller,
                    &resolution,
                    code_graph,
                    max_candidates,
                );
            }
        }

        info!(
            "Layered call analysis completed: {} total calls, {} unique, {} ambiguous, {} unresolved",
            total_calls, resolved_calls, ambiguous_calls, unresolved_calls
        );
    }

    /// Phase P2b：caller 归属（口径 (iii)）。
    ///
    /// 实现在 `resolution::layered_caller_node`（与刷新链路、增量链路**共用同一段代码**，
    /// 这是"全量 vs 增量结果一致"的前提）。这里只保留 `self` 上下文相关的转发。
    fn _layered_caller_node(
        &self,
        site: &CallSite,
        ctx: &FileScopeCtx,
        funcs: &[FunctionInfo],
    ) -> Option<FunctionInfo> {
        resolution_caller_node(site, Some(ctx), funcs)
    }

    /// Phase P2b：把一个 `Resolution` 落成边（口径 A + 口径 B + 要求 (v)）。
    ///
    /// 实现在 `resolution::emit_layered_resolution`，与刷新链路、增量链路**共用同一段代码**
    /// （"全量 vs 增量一致"的前提）。这里只做转发并返回写出的边数，供上层统计。
    fn _emit_layered_resolution(
        &self,
        site: &CallSite,
        caller: &FunctionInfo,
        resolution: &Resolution,
        code_graph: &mut PetCodeGraph,
        max_candidates: usize,
    ) {
        resolution_emit(site, caller, resolution, code_graph, max_candidates);
    }

    /// 分析单个文件的函数调用（用于petgraph）
    fn _analyze_file_calls_for_petgraph(
        &self,
        symbols: &[crate::codegraph::treesitter::AstSymbolInstanceArc],
        functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
        file_path: &PathBuf,
        name_index: &FunctionNameIndex<'_>,
    ) -> CallAnalysisStats {
        let mut stats = CallAnalysisStats::default();
        
        // 分析每个AST符号
        for symbol in symbols {
            let symbol_guard = symbol.read();
            let symbol_ref = symbol_guard.as_ref();
            
            // 检查是否为函数调用
            if symbol_ref.symbol_type() == crate::codegraph::treesitter::structs::SymbolType::FunctionCall {
                stats.total += 1;
                let call_name = symbol_ref.name();
                let call_line = symbol_ref.full_range().start_point.row + 1;
                
                // 查找调用者函数（通过分析调用位置）
                if let Some(caller_idx) = self._find_caller_function_by_line(file_path, call_line, functions) {
                    let caller = &functions[caller_idx];
                    
                    // 尝试解析被调用函数
                    if let Some(callee_info) = self._resolve_callee_function(
                        call_name, 
                        file_path, 
                        functions, 
                        code_graph,
                        name_index
                    ) {
                        // 创建已解析的调用关系
                        let relation = CallRelation::legacy(
                            caller.id,
                            callee_info.id,
                            caller.name.clone(),
                            callee_info.name.clone(),
                            caller.file_path.clone(),
                            callee_info.file_path.clone(),
                            call_line,
                            true,
                        );
                        
                        // Phase P2b 收口（口径 B）：唯一的边写入入口（详见
                        // `PetCodeGraph::upsert_call_site_edges` 的文档）。legacy 全量路径
                        // 记录的 callee_name 是**解析结果名**，而刷新路径记录的是 AST 裸名，
                        // 因此 upsert 的删除名字族必须同时包含两者 —— 由 upsert 内部保证。
                        let written = code_graph.upsert_call_site_edges(
                            &caller.id,
                            call_line,
                            call_name,
                            vec![relation],
                        );
                        if written == 0 {
                            warn!("Failed to add resolved call relation: {}", call_name);
                        } else {
                            stats.resolved += 1;
                        }
                    } else {
                        // 创建未解析的调用关系
                        self._create_unresolved_call_relation(
                            caller, 
                            call_name, 
                            file_path, 
                            call_line, 
                            code_graph
                        );
                        stats.unresolved += 1;
                    }
                }
            }
        }
        
        stats
    }
    
    /// 解析被调用函数
    ///
    /// Phase P2b 交付物 2（接线点③，全量 petgraph 主路径的"四级降级"）：
    /// `layered` 分支**一条都不走**下面的四级降级（本文件同名 first → 全局索引
    /// → `find_functions_by_name().first()` → 限定名 `contains` 子串），
    /// 而是把整条判定交给分层引擎；`legacy` 分支一字未改（红线 1）。
    ///
    /// 附带修正：原实现的 `_current_file` 被下划线忽略（"本文件优先"其实压根没查文件），
    /// layered 分支**真正使用**它 —— 这是限定名与同名跨文件消歧的必要信息。
    fn _resolve_callee_function(
        &self,
        call_name: &str,
        current_file: &PathBuf,
        current_functions: &[FunctionInfo],
        code_graph: &PetCodeGraph,
        name_index: &FunctionNameIndex<'_>,
    ) -> Option<FunctionInfo> {
        // ---- Phase P2b：layered 薄调用（绝不出现 first() / contains()）----
        if self.resolver_mode == ResolverMode::Layered {
            let callee_id =
                self._layered_callee_first(call_name, current_file.as_path(), current_functions, code_graph)?;
            return code_graph.get_function_by_id(&callee_id).cloned();
        }

        // 1. 先在本文件查找
        for function in current_functions {
            if function.name == call_name {
                return Some(function.clone());
            }
        }
        
        // 2. 在全局函数名索引（本次 pass 预建）中查找
        if let Some(global_func) = self._find_function_by_name_global(name_index, call_name) {
            return Some(global_func);
        }
        
        // 3. 在代码图中查找
        let global_functions = code_graph.find_functions_by_name(call_name);
        if let Some(func) = global_functions.first() {
            return Some((*func).clone());
        }
        
        // 4. 尝试解析限定名（如 Class.method, module.function）
        if let Some(qualified_func) = self._resolve_qualified_function_name(call_name, code_graph) {
            return Some(qualified_func);
        }
        
        None
    }
    
    /// 解析限定函数名（如 Class.method, module.function）
    ///
    /// Phase P2b 交付物 2（接线点③的第二段）：`layered` 分支**禁止**下面这段
    /// `func.namespace.contains(prefix) || func.name == method_name` ——
    /// 后半个条件让前半个校验彻底失效（`func.name` 恒等于 `method_name`），
    /// 前半个又是子串匹配（`"ab"` 能命中 `"abc.rs::x"`）。限定名在 layered 里由
    /// L5 `QualifiedPath` 用**分段精确**比较处理（见 `resolver::module_chain_matches`）。
    fn _resolve_qualified_function_name(
        &self,
        qualified_name: &str,
        code_graph: &PetCodeGraph,
    ) -> Option<FunctionInfo> {
        // ---- Phase P2b：layered 走引擎（限定前缀交 L5，绝不子串匹配）----
        if self.resolver_mode == ResolverMode::Layered {
            let callee_id = self
                ._layered_callee_first(qualified_name, Path::new(""), &[], code_graph)?;
            return code_graph.get_function_by_id(&callee_id).cloned();
        }

        // 检查是否包含分隔符
        if let Some(dot_pos) = qualified_name.rfind('.') {
            let (prefix, method_name) = qualified_name.split_at(dot_pos);
            let method_name = &method_name[1..]; // 去掉点号
            
            // 查找匹配的方法
            let candidates = code_graph.find_functions_by_name(method_name);
            for func in candidates {
                // 检查函数是否在指定的类/模块中
                if func.namespace.contains(prefix) || func.name == method_name {
                    return Some(func.clone());
                }
            }
        }
        
        None
    }
    
    /// 创建未解析的调用关系
    fn _create_unresolved_call_relation(
        &self,
        caller: &FunctionInfo,
        call_name: &str,
        file_path: &PathBuf,
        call_line: usize,
        code_graph: &mut PetCodeGraph,
    ) {
        // 为未解析的调用创建一个临时函数节点
        let temp_callee_id = Uuid::new_v4();
        let temp_callee = FunctionInfo::legacy(
            temp_callee_id,
            call_name.to_string(),
            file_path.clone(),
            call_line,
            call_line,
            "unresolved".to_string(),
            caller.language.clone(),
            Some(format!("unresolved_call_{}", call_name)),
        );
        
        // 添加到代码图
        let _node_index = code_graph.add_function(temp_callee);
        
        // 创建未解析的调用关系
        let relation = CallRelation::legacy(
            caller.id,
            temp_callee_id,
            caller.name.clone(),
            call_name.to_string(),
            caller.file_path.clone(),
            file_path.clone(),
            call_line,
            false,
        );
        
        // Phase P2b 收口（口径 B）：未解析（幽灵）边也走同一个唯一写入入口 ——
        // 重写同一个调用点时，旧的幽灵边会被删除，绝不留成"两条同名同线的边"。
        if code_graph
            .upsert_call_site_edges(
                &caller.id,
                call_line,
                call_name,
                vec![relation],
            )
            == 0
        {
            warn!("Failed to add unresolved call relation: {}", call_name);
        }
    }
    
    /// 回退调用分析（当TreeSitter解析失败时使用）
    fn _fallback_call_analysis(
        &self,
        functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 基于函数名的简单启发式分析
        for function in functions {
            let function_name = function.name.to_lowercase();
            
            // 根据函数名推断可能的调用关系
            if function_name.contains("main") || function_name.contains("entry") {
                // main/entry 函数可能调用其他函数
                self._create_heuristic_calls(function, functions, code_graph);
            } else if function_name.contains("test") || function_name.contains("spec") {
                // 测试函数可能调用被测试的函数
                self._create_test_calls(function, functions, code_graph);
            }
        }
    }
    
    /// 创建启发式调用关系
    fn _create_heuristic_calls(
        &self,
        main_function: &FunctionInfo,
        all_functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 为main函数创建到其他函数的调用关系
        for other_func in all_functions {
            if other_func.id != main_function.id {
                let relation = CallRelation::legacy(
                    main_function.id,
                    other_func.id,
                    main_function.name.clone(),
                    other_func.name.clone(),
                    main_function.file_path.clone(),
                    other_func.file_path.clone(),
                    main_function.line_start,
                    false, // 启发式调用标记为未解析
                );
                
                if let Err(e) = code_graph.add_call_relation(relation) {
                    warn!("Failed to add heuristic call relation: {}", e);
                }
            }
        }
    }
    
    /// 创建测试调用关系
    fn _create_test_calls(
        &self,
        test_function: &FunctionInfo,
        all_functions: &[FunctionInfo],
        code_graph: &mut PetCodeGraph,
    ) {
        // 为测试函数创建到被测试函数的调用关系
        let test_name = test_function.name.to_lowercase();
        
        for other_func in all_functions {
            if other_func.id != test_function.id {
                let other_name = other_func.name.to_lowercase();
                
                // 检查是否是被测试的函数
                if test_name.contains(&other_name) || other_name.contains("test") {
                    let relation = CallRelation::legacy(
                        test_function.id,
                        other_func.id,
                        test_function.name.clone(),
                        other_func.name.clone(),
                        test_function.file_path.clone(),
                        other_func.file_path.clone(),
                        test_function.line_start,
                        false, // 启发式调用标记为未解析
                    );
                    
                    if let Err(e) = code_graph.add_call_relation(relation) {
                        warn!("Failed to add test call relation: {}", e);
                    }
                }
            }
        }
    }
}

/// 调用分析统计信息
#[derive(Default, Debug)]
struct CallAnalysisStats {
    total: usize,
    resolved: usize,
    unresolved: usize,
}

impl Default for CodeParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_parse_file_with_real_rust_code() {
        let mut parser = CodeParser::new();
        
        // Create a temporary directory and Rust file
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir.path().join("test.rs");
        
        // Write a simple Rust file with functions and structs
        let rust_code = r#"
pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new(initial: i32) -> Self {
        Calculator { value: initial }
    }

    pub fn add(&mut self, x: i32) -> i32 {
        self.value += x;
        self.value
    }

    pub fn get_value(&self) -> i32 {
        self.value
    }
}

pub fn main() {
    let mut calc = Calculator::new(10);
    let result = calc.add(5);
    
}
"#;
        
        fs::write(&test_file, rust_code).unwrap();
        
        // Parse the file
        let result = parser.parse_file(&test_file);
        assert!(result.is_ok(), "Failed to parse file: {:?}", result.err());
        
        // Check that functions were extracted
        let functions = parser.file_functions.get(&test_file).unwrap();
        assert!(!functions.is_empty(), "No functions were extracted");
        
        // Check that we have the expected functions
        let function_names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
        assert!(function_names.contains(&"new"), "Function 'new' not found");
        assert!(function_names.contains(&"add"), "Function 'add' not found");
        assert!(function_names.contains(&"get_value"), "Function 'get_value' not found");
        assert!(function_names.contains(&"main"), "Function 'main' not found");
        
        // Check that snippets were created
        for function in functions {
            let snippet_info = parser.snippet_index.get_snippet_info(&function.id);
            assert!(snippet_info.is_some(), "No snippet info for function {}", function.name);
            
            let snippet = snippet_info.unwrap();
            assert!(snippet.cached_content.is_some(), "No cached content for function {}", function.name);
            
            let content = snippet.cached_content.as_ref().unwrap();
            assert!(!content.is_empty(), "Empty snippet content for function {}", function.name);
        }
        

    }

    #[test]
    fn test_parse_file_with_python_code() {
        let mut parser = CodeParser::new();
        
        // Create a temporary directory and Python file
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir.path().join("test.py");
        
        // Write a simple Python file
        let python_code = r#"
def calculate_sum(a, b):
    """Calculate the sum of two numbers."""
    return a + b

def multiply_numbers(x, y):
    """Multiply two numbers."""
    result = x * y
    return result

class Calculator:
    def __init__(self, initial_value=0):
        self.value = initial_value
    
    def add(self, x):
        self.value += x
        return self.value
    
    def get_value(self):
        return self.value

if __name__ == "__main__":
    calc = Calculator(10)
    result = calc.add(5)
    print(f"Result: {result}")
"#;
        
        fs::write(&test_file, python_code).unwrap();
        
        // Parse the file
        let result = parser.parse_file(&test_file);
        assert!(result.is_ok(), "Failed to parse Python file: {:?}", result.err());
        
        // Check that functions were extracted
        let functions = parser.file_functions.get(&test_file).unwrap();
        assert!(!functions.is_empty(), "No functions were extracted from Python file");
        
        // Check that we have the expected functions
        let function_names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
        assert!(function_names.contains(&"calculate_sum"), "Function 'calculate_sum' not found");
        assert!(function_names.contains(&"multiply_numbers"), "Function 'multiply_numbers' not found");
        

    }

    #[test]
    fn test_analyze_petgraph_call_relations() {
        let mut parser = CodeParser::new();
        let mut code_graph = PetCodeGraph::new();
        
        // 创建一些测试函数
        let func1 = FunctionInfo {
            id: Uuid::new_v4(),
            name: "main".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 1,
            line_end: 10,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn main()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        
        let func2 = FunctionInfo {
            id: Uuid::new_v4(),
            name: "calculate".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 12,
            line_end: 20,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn calculate()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        
        // 添加到代码图
        code_graph.add_function(func1.clone());
        code_graph.add_function(func2.clone());
        
        // 添加到文件函数映射
        parser.file_functions.insert(
            PathBuf::from("test.rs"),
            vec![func1.clone(), func2.clone()]
        );
        
        // 运行调用关系分析
        parser._analyze_petgraph_call_relations(&mut code_graph);
        
        // 验证结果
        let stats = code_graph.get_stats();
        assert!(stats.total_functions >= 2);
        
        // 检查是否有调用关系（即使是启发式的）
        let _callers = code_graph.get_callers(&func1.id);
        let _callees = code_graph.get_callees(&func1.id);
        
        // 由于没有真实的AST解析，可能只有启发式调用关系
        // 或者没有调用关系（取决于回退分析的实现）
    }
    
    #[test]
    fn test_resolve_qualified_function_name() {
        // 本用例专测 **legacy** 的 `_resolve_qualified_function_name` 子串匹配语义；
        // 该函数会按 `resolver_mode` 分支，而 Phase P3 起默认已切 layered ⇒ 必须显式钉
        // Legacy，否则走引擎分支（空 file/空 ctx 下解析不到）会返回 None，与用例原意不符。
        let mut parser = CodeParser::new();
        parser.set_resolver_mode(ResolverMode::Legacy);
        let mut code_graph = PetCodeGraph::new();
        
        // 创建一个类方法
        let method = FunctionInfo {
            id: Uuid::new_v4(),
            name: "process".to_string(),
            file_path: PathBuf::from("test.rs"),
            line_start: 1,
            line_end: 10,
            namespace: "Calculator".to_string(),
            language: "rust".to_string(),
            signature: Some("fn process()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        
        code_graph.add_function(method.clone());
        
        // 测试解析限定名
        let result = parser._resolve_qualified_function_name("Calculator.process", &code_graph);
        assert!(result.is_some());
        
        let resolved_func = result.unwrap();
        assert_eq!(resolved_func.name, "process");
        assert_eq!(resolved_func.namespace, "Calculator");
    }

    #[test]
    fn test_incremental_build_with_md5_checking() {
        // 创建临时目录
        let temp_dir = tempfile::TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("test_project");
        fs::create_dir(&project_dir).unwrap();

        // 创建测试文件
        let test_file = project_dir.join("test.rs");
        let content = r#"
pub fn hello() {
    println!("Hello, world!");
}

pub fn greet(name: &str) {
    println!("Hello, {}!", name);
}
"#;
        fs::write(&test_file, content).unwrap();

        // 创建解析器
        let mut parser = CodeParser::new();

        // 第一次构建
        let graph1 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats1 = graph1.get_stats();
        assert_eq!(stats1.total_functions, 2);

        // 修改文件内容
        let new_content = r#"
pub fn hello() {
    println!("Hello, world!");
}

pub fn greet(name: &str) {
    println!("Hello, {}!", name);
}

pub fn new_function() {
    println!("This is a new function!");
}
"#;
        fs::write(&test_file, new_content).unwrap();

        // 第二次构建（应该检测到文件变化）
        let graph2 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats2 = graph2.get_stats();
        assert_eq!(stats2.total_functions, 3); // 应该有3个函数

        // 再次构建（文件未变化，应该跳过解析）
        let graph3 = parser.build_petgraph_code_graph(&project_dir).unwrap();
        let stats3 = graph3.get_stats();
        assert_eq!(stats3.total_functions, 3); // 应该仍然是3个函数

        // 清理
        temp_dir.close().unwrap();
    }

    /// 回归测试：增量刷新（refresh_file）必须保留调用边与函数级代码片段。
    ///
    /// 历史缺陷：`refresh_file` 全程不写 `self.file_functions`，而
    /// `_find_caller_function` / `_update_snippet_index` 都依赖它，于是
    /// "当前文件刚生成的函数 id"必然查不到 → caller 为 None → 调用边被静默丢弃，
    /// 函数片段也不会写入。此处同时锁住这两条链路。
    #[test]
    fn test_refresh_file_keeps_call_edges_and_function_snippets() {
        let mut parser = CodeParser::new();
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir.path().join("calls.rs");

        // 一个含真实调用关系的 Rust 文件：compute 调用 helper
        let rust_code = r#"
pub fn helper(x: i32) -> i32 {
    x + 1
}

pub fn compute(y: i32) -> i32 {
    helper(y)
}
"#;
        fs::write(&test_file, rust_code).unwrap();

        // 先做一次全量解析，让 file_functions/snippet_index 处于真实使用状态
        parser
            .parse_file(&test_file)
            .expect("parse_file should succeed");

        // 再走增量刷新链路：refresh_file -> _analyze_file_calls / _update_snippet_index
        let mut entity_graph = EntityGraph::new();
        let mut call_graph = PetCodeGraph::new();
        parser
            .refresh_file(&test_file, &mut entity_graph, &mut call_graph)
            .expect("refresh_file should succeed");

        let functions = parser
            .file_functions
            .get(&test_file)
            .expect("refresh_file must populate file_functions for the refreshed file");
        assert_eq!(
            functions.len(),
            2,
            "stale function entries must not survive refresh (expected exactly helper + compute)"
        );

        let find_id = |name: &str| -> Uuid {
            functions
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("function '{}' missing after refresh", name))
                .id
        };
        let helper_id = find_id("helper");
        let compute_id = find_id("compute");

        // file_functions 与 call_graph 必须是同一批函数（无幽灵条目）
        for function in functions {
            assert!(
                call_graph.get_function_by_id(&function.id).is_some(),
                "file_functions holds a function absent from call_graph: {}",
                function.name
            );
        }

        // 1) 刷新后必须产生调用边：compute -> helper
        let callees = call_graph.get_callees(&compute_id);
        assert!(
            !callees.is_empty(),
            "regression: refresh_file lost call edges (no callee found for 'compute')"
        );
        assert!(
            callees.iter().any(|(func, _)| func.name == "helper"),
            "callee of 'compute' should be 'helper'"
        );

        let callers = call_graph.get_callers(&helper_id);
        assert!(
            !callers.is_empty(),
            "regression: get_callers('helper') is empty after refresh"
        );
        assert!(
            callers.iter().any(|(func, _)| func.name == "compute"),
            "caller of 'helper' should be 'compute'"
        );

        let stats = call_graph.get_stats();
        assert!(
            stats.resolved_calls > 0,
            "regression: resolved_calls should be > 0 after refresh, got {}",
            stats.resolved_calls
        );

        // 2) 刷新后函数级片段可取到且缓存内容非空
        for function in functions {
            let snippet_info = parser
                .snippet_index
                .get_snippet_info(&function.id)
                .unwrap_or_else(|| panic!("no snippet info for function {}", function.name));
            let content = snippet_info
                .cached_content
                .as_ref()
                .unwrap_or_else(|| panic!("no cached content for function {}", function.name));
            assert!(
                !content.is_empty(),
                "empty snippet content for function {}",
                function.name
            );
            assert_eq!(snippet_info.file_path, test_file);
        }

        // helper 的片段应包含其函数体
        let helper_snippet = parser
            .snippet_index
            .get_snippet_info(&helper_id)
            .expect("helper snippet should exist");
        let helper_content = helper_snippet
            .cached_content
            .as_ref()
            .expect("helper snippet content should be cached");
        assert!(
            helper_content.contains("x + 1"),
            "helper snippet content mismatch: {:?}",
            helper_content
        );
    }

    /// 回归测试：增量合并必须按 `(file_path, line_start, line_end)` 三元组去重，
    /// 且**同批次内的重复项只允许添加一次**。
    ///
    /// 背景：`_merge_new_functions` / `_merge_new_functions_to_code_graph` 从
    /// "每个候选函数线性扫全图"改为"预建 HashSet + O(1) 查表"后，如果忘记在
    /// `add_function` 之后把 key 回填进集合，同批重复的三元组就会被重复插入
    /// （原实现因为每次都是实时扫描图内容，天然不会重复）。
    /// 本测试用"两条完全相同的三元组 + 一条不同的"锁住这一语义。
    #[test]
    fn test_merge_new_functions_dedupes_by_file_and_line() {
        let shared_file = PathBuf::from("merge_target.rs");

        // 现有图中已有的函数 f：(merge_target.rs, 10, 20)
        let existing = FunctionInfo {
            id: Uuid::new_v4(),
            name: "existing".to_string(),
            file_path: shared_file.clone(),
            line_start: 10,
            line_end: 20,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn existing()".to_string()),
            module_path: None,
            qualified_name: None,
        };

        // 本批次待合并：dup_a / dup_b 与 existing 三元组完全一致（id 不同，模拟同批重复），
        // unique 的三元组不同，应当被添加。
        let dup_a = FunctionInfo {
            id: Uuid::new_v4(),
            name: "dup_a".to_string(),
            file_path: shared_file.clone(),
            line_start: 10,
            line_end: 20,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn dup_a()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let dup_b = FunctionInfo {
            id: Uuid::new_v4(),
            name: "dup_b".to_string(),
            file_path: shared_file.clone(),
            line_start: 10,
            line_end: 20,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn dup_b()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let unique = FunctionInfo {
            id: Uuid::new_v4(),
            name: "unique".to_string(),
            file_path: shared_file.clone(),
            line_start: 30,
            line_end: 40,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn unique()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        // pair_c / pair_d 的三元组 (50, 60) 在现有图中**不存在**，且本批次内出现两次。
        // 这一组才是"同批重复只添加一次"语义的关键：若忘记在 add_function 后回填 seen，
        // 两个都会被插入（原实现因为每次实时扫描图内容，第二个必然被判为 exists）。
        let pair_c = FunctionInfo {
            id: Uuid::new_v4(),
            name: "pair_c".to_string(),
            file_path: shared_file.clone(),
            line_start: 50,
            line_end: 60,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn pair_c()".to_string()),
            module_path: None,
            qualified_name: None,
        };
        let pair_d = FunctionInfo {
            id: Uuid::new_v4(),
            name: "pair_d".to_string(),
            file_path: shared_file.clone(),
            line_start: 50,
            line_end: 60,
            namespace: "global".to_string(),
            language: "rust".to_string(),
            signature: Some("fn pair_d()".to_string()),
            module_path: None,
            qualified_name: None,
        };

        // ---------- PetCodeGraph 版本 ----------
        let mut parser = CodeParser::new();
        parser.file_functions.insert(
            shared_file.clone(),
            vec![
                dup_a.clone(),
                dup_b.clone(),
                unique.clone(),
                pair_c.clone(),
                pair_d.clone(),
            ],
        );

        let mut pet_graph = PetCodeGraph::new();
        pet_graph.add_function(existing.clone());
        assert_eq!(pet_graph.graph.node_count(), 1);
        assert_eq!(pet_graph.get_stats().total_functions, 1);

        parser._merge_new_functions(&mut pet_graph);

        // 合并后应只有 3 个函数：existing(10,20) + unique(30,40) + pair_{c,d} 中的一个(50,60)。
        // dup_a / dup_b 都命中现有图的 (10,20) 被跳过；pair_c / pair_d 只允许进一个。
        let count_pet = |start: usize, end: usize| -> usize {
            pet_graph
                .graph
                .node_weights()
                .filter(|f| f.file_path == shared_file && f.line_start == start && f.line_end == end)
                .count()
        };
        assert_eq!(
            pet_graph.graph.node_count(),
            3,
            "regression: same-batch duplicates were inserted (expected +2 functions only: unique and one of pair_c/pair_d)"
        );
        assert_eq!(pet_graph.get_stats().total_functions, 3);
        assert_eq!(
            count_pet(10, 20),
            1,
            "de-dup key must stay (file_path, line_start, line_end), not id/name"
        );
        assert_eq!(
            count_pet(50, 60),
            1,
            "regression: seen backfill missing -> same-batch duplicate (50,60) inserted twice"
        );
        let names: Vec<&str> = pet_graph.graph.node_weights().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"existing"), "pre-existing function must survive merge");
        assert!(names.contains(&"unique"), "new function must be merged");
        assert_eq!(
            usize::from(names.contains(&"pair_c")) + usize::from(names.contains(&"pair_d")),
            1,
            "exactly one of the same-batch duplicates (50,60) must be inserted"
        );

        // ---------- Phase P3 交付物 1：CodeGraph 版本已删除 ----------
        // 改造前这里还有一半用例，用 `_merge_new_functions_to_code_graph` 断言
        // `CodeGraph` 的同一组去重键 `(file_path, line_start, line_end)`。那个实现
        // 连同 `build_code_graph` 的整条自建产边链路一起被删掉了（`CodeGraph` 现在
        // 只是 `PetCodeGraph` 的只读投影，不再自己合并/产边），
        // **等价替代**就是上面的 PetCodeGraph 半段：同一个去重键、同一个"同批次回填 seen"
        // 回归、同一批断言（node_count / total_functions / (10,20)==1 / (50,60)==1 /
        // pair_c^pair_d==1），因此删除的只是"对已消失实现的重复覆盖"，没有丢任何不变量。
    }

    // ==========================================================================================
    // Phase P1 交付物 3：`file_scopes` 的接线测试（只建不消费）
    //
    // 这两条用例是 `file_scopes` / `file_scope_ctx` 的**真实消费者**，
    // 因此本阶段不需要任何 `#[allow(dead_code)]`。
    // ==========================================================================================

    /// `parse_file` 必须为本文件产出 `FileScopeCtx`（作用域树 + import 表），
    /// 并且 **ctx 的内容不得反过来影响 `FunctionInfo` 的产出**。
    #[test]
    fn test_parse_file_builds_file_scope_ctx_and_does_not_touch_functions() {
        let temp_dir = tempdir().unwrap();
        let file = temp_dir.path().join("ctx_probe.rs");
        fs::write(
            &file,
            "use a::b::{c, d as e};\npub fn host(x: i32) -> i32 { guest(x) }\nfn guest(y: i32) -> i32 { y }\n",
        )
        .unwrap();

        let mut parser = CodeParser::new();
        parser.parse_file(&file).expect("parse_file 必须成功");

        // 1) 建出来了，且只有这一个文件。
        assert_eq!(parser.file_scope_count(), 1, "parse_file 应恰好写入 1 条 ctx");
        let ctx = parser
            .file_scope_ctx(&file)
            .expect("`file_scope_ctx` 必须能读回刚写入的 ctx");

        // 2) import 原料真的在 ctx 里（走的是 P1 交付物 1 的 trait getter，不是 downcast）。
        assert_eq!(
            ctx.imports.len(),
            2,
            "`use a::b::{{c, d as e}};` 应拆成 2 条绑定，实测 = {:?}",
            ctx.imports.entries()
        );
        let aliased = ctx.imports.unique_binding("e").expect("别名 e 必须在 ImportMap 里");
        assert_eq!(aliased.module_path, vec!["a", "b", "d"]);
        assert_eq!(aliased.original_name.as_deref(), Some("d"));

        // 3) 未传仓库根（没走 build_*）⇒ module path 降级为空串，绝不瞎猜。
        assert_eq!(ctx.module_path, "", "仓库根未知时 module path 必须是空串");

        // 4) 只建不消费的**硬证明**：ctx 里的可调用作用域与既有 match 分支产出的
        //    FunctionInfo 必须一一对应 —— 两者同源于 FunctionDeclaration 符号，
        //    谁也没有替谁增删。
        let functions = parser
            .file_functions
            .get(&file)
            .cloned()
            .unwrap_or_default();
        let mut names_from_graph: Vec<String> = functions.iter().map(|f| f.name.clone()).collect();
        let mut names_from_ctx: Vec<String> = ctx
            .scopes
            .callable_scopes()
            .into_iter()
            .filter_map(|id| ctx.scopes.scope(id).map(|s| s.name.clone()))
            .collect();
        names_from_graph.sort();
        names_from_ctx.sort();
        assert_eq!(
            names_from_graph, names_from_ctx,
            "FunctionInfo 的集合必须不受 ctx 影响（两侧同源、数量与名字都该一致）"
        );
        assert_eq!(names_from_graph, vec!["guest".to_string(), "host".to_string()]);

        // 5) 交付物 2(b) 的通路在 `CodeParser` 里也是通的：Java 的 `package` 声明必须
        //    随 `parse_file_with_metadata` 一起进到 ctx（改造前 `java.rs` 是空分支、包名丢弃）。
        let java_file = temp_dir.path().join("Runner.java");
        fs::write(&java_file, "package com.ex;\npublic class Runner { public void go() {} }\n").unwrap();
        parser.parse_file(&java_file).expect("java parse_file 必须成功");
        let java_ctx = parser
            .file_scope_ctx(&java_file)
            .expect("Java 文件也应有 ctx");
        assert_eq!(
            java_ctx.declared_package.as_deref(),
            Some(&["com".to_string(), "ex".to_string()][..]),
            "package com.ex; 必须被带出（交付物 2(b)）"
        );
        assert_eq!(java_ctx.module_path, "com.ex", "有包名时以包名为准");
        assert_eq!(
            parser.file_functions.get(&java_file).map(|v| v.len()),
            Some(1),
            "Java 的 FunctionInfo 仍只有 `go` 一个 —— package 不产生任何新节点"
        );
    }

    /// 生命周期：`refresh_file` 重建 ctx，文件删除后 ctx 必须与 `file_functions` 一起失效。
    #[test]
    fn test_refresh_and_remove_keep_file_scope_ctx_in_sync() {
        let temp_dir = tempdir().unwrap();
        let file = temp_dir.path().join("ctx_life.rs");
        fs::write(&file, "use a::b;\nfn one() {}\n").unwrap();

        let mut parser = CodeParser::new();
        let mut entity_graph = EntityGraph::new();
        let mut call_graph = PetCodeGraph::new();

        parser
            .refresh_file(&file, &mut entity_graph, &mut call_graph)
            .expect("首次 refresh_file 必须成功");
        let first = parser.file_scope_ctx(&file).expect("refresh 后应有 ctx");
        assert_eq!(first.imports.len(), 1);
        assert_eq!(first.imports.entries()[0].module_path, vec!["a", "b"]);

        // 改写文件 ⇒ ctx 必须按新内容重建（不能残留旧的 import 绑定）。
        fs::write(&file, "use z::y;\nfn two() {}\n").unwrap();
        parser
            .refresh_file(&file, &mut entity_graph, &mut call_graph)
            .expect("二次 refresh_file 必须成功");
        let second = parser.file_scope_ctx(&file).expect("重建后仍应有 ctx");
        assert_eq!(second.imports.len(), 1, "ctx 必须整体替换，不得叠加");
        assert_eq!(second.imports.entries()[0].module_path, vec!["z", "y"]);
        assert!(
            second.imports.lookup("b").is_empty(),
            "旧文件的 import 不得残留在新 ctx 里"
        );

        // 删除文件 ⇒ `_remove_file_entities` 必须连带清除 ctx（与 file_functions 同语义）。
        fs::remove_file(&file).unwrap();
        parser
            .refresh_file(&file, &mut entity_graph, &mut call_graph)
            .expect("删除后的 refresh_file 走清理分支，不应报错");
        assert!(
            parser.file_scope_ctx(&file).is_none(),
            "文件已删除，ctx 必须同步失效（否则 P2 会读到已消失文件的作用域）"
        );
        assert_eq!(parser.file_scope_count(), 0);
    }

    // ==========================================================================================
    // Phase P2a 交付物 2(b)/3：`ensure_file_scope_ctx` 与 `--resolver/--max-candidates/--levels`
    //
    // 这三条用例是新增字段的**真实消费者**（因此不需要 `#[allow(dead_code)]`），
    // 同时把"P2a 只存不用、一个字节都不改产出"这条红线钉在单元测试里。
    // ==========================================================================================

    /// `ensure_file_scope_ctx`：命中缓存必须复用同一个 `Arc`（绝不重复解析）；
    /// 缓存缺失必须**按需补建**（口径 C：正确性不依赖缓存是否命中）；文件读不到才返回 `None`。
    #[test]
    fn test_ensure_file_scope_ctx_reuses_cache_and_backfills_on_demand() {
        let temp_dir = tempdir().unwrap();
        let file = temp_dir.path().join("ensure_ctx.rs");
        fs::write(&file, "pub fn a(p: i32) -> i32 { b(p) }\nfn b(q: i32) -> i32 { q }\n").unwrap();

        let mut parser = CodeParser::new();
        // 单测里直接设仓库根（`build_*` 入口本来就会设；这里不依赖建图，只验证补建口径）。
        parser.resolution_root = Some(temp_dir.path().to_path_buf());

        // 1) 冷缓存：没有解析过 ⇒ 现场补建。
        assert_eq!(parser.file_scope_count(), 0, "新 parser 的 ctx 缓存必须是空的");
        let first = parser
            .ensure_file_scope_ctx(&file)
            .expect("缺失时必须按需解析补建");
        assert_eq!(parser.file_scope_count(), 1, "补建的 ctx 必须回装进缓存");
        assert!(!first.scopes.is_empty(), "补建的 ctx 必须有作用域树");
        assert_eq!(first.module_path, "ensure_ctx", "补建必须用同一个仓库根推导 module path");

        // 2) 热缓存：第二次调用必须返回**同一个 Arc**（指针相等 ⇒ 一次都没重新解析）。
        let second = parser.ensure_file_scope_ctx(&file).expect("缓存命中");
        assert!(
            Arc::ptr_eq(&first, &second),
            "`ensure_file_scope_ctx` 命中缓存时不得重新解析（否则全量/增量成本与结果都可能不一致）"
        );

        // 3) 与 `file_scope_ctx` 是同一份数据（读者只有一个）。
        assert!(Arc::ptr_eq(&second, &parser.file_scope_ctx(&file).expect("读者必须拿得到")));

        // 4) 无法解析的路径 ⇒ `None`（与 `parse_file` 同口径：打 warn，绝不 panic）。
        let missing = temp_dir.path().join("does_not_exist.rs");
        assert!(
            parser.ensure_file_scope_ctx(&missing).is_none(),
            "文件不存在时返回 None，由调用方按\"无 ctx\"处理"
        );
        assert_eq!(
            parser.file_scope_count(),
            1,
            "补建失败不得往缓存里写任何条目"
        );
    }

    /// CLI 三枚新 flag 的组装：默认值、`--levels=` 解析、`--max-candidates` 的 u8 饱和、
    /// 非法 `--levels=` 必须 `None`（CLI 据此报错，**绝不静默退回全开**）。
    #[test]
    fn test_resolver_flags_defaults_and_config_composition() {
        let mut parser = CodeParser::new();

        // 默认：**layered** + 上限 8 + 全层（Phase P3 起默认档已从 legacy 切到 layered；
        // legacy 保留为 `--resolver=legacy` 回滚逃生口，行为仍与 P1 逐字节一致）。
        assert_eq!(parser.resolver_mode(), ResolverMode::Layered, "`--resolver` 默认必须是 layered");
        assert_eq!(parser.max_candidates(), DEFAULT_MAX_CANDIDATES as usize);
        assert!(parser.levels_spec().is_none(), "未传 `--levels` 时不得凭空造一个串");
        let config = parser.resolution_config().expect("默认配置必须可组装");
        assert_eq!(config.enabled_levels, ResolutionConfig::ALL_LEVELS);
        assert_eq!(config.max_candidates, DEFAULT_MAX_CANDIDATES);

        // 置位：三个字段都读得到（P2a 只存不用，P2b 才分叉）。
        parser.set_resolver_mode(ResolverMode::Layered);
        assert_eq!(parser.resolver_mode(), ResolverMode::Layered);

        // `--levels=all,-l6`：L6 的 bit 必须被清掉，其它保持全开。
        parser.set_levels_spec(Some("all,-l6".to_string()));
        let config = parser.resolution_config().expect("`all,-l6` 必须是合法配置");
        assert_eq!(
            config.enabled_levels
                & ResolutionConfig::bit(crate::codegraph::resolution::ResolutionLevel::SameModule),
            0,
            "关掉 L6 后 bit 必须为 0"
        );
        assert_ne!(
            config.enabled_levels
                & ResolutionConfig::bit(crate::codegraph::resolution::ResolutionLevel::QualifiedPath),
            0,
            "只关 L6 不得顺手关掉 L5"
        );

        // 空串等价于"没传"（全开），避免 `--levels=` 误伤成"一层都不开"。
        parser.set_levels_spec(Some("   ".to_string()));
        assert!(parser.levels_spec().is_none(), "空白串必须归一化成 None");
        assert_eq!(
            parser.resolution_config().expect("空白 levels 必须可组装").enabled_levels,
            ResolutionConfig::ALL_LEVELS
        );

        // `--max-candidates` 超过 u8 ⇒ 饱和到 255（与 P2b 落盘的 `candidate_count: u8` 同口径）。
        parser.set_max_candidates(1000);
        assert_eq!(parser.max_candidates(), 1000, "usize 原值必须可读回（诊断用）");
        assert_eq!(
            parser.resolution_config().expect("饱和配置").max_candidates,
            u8::MAX,
            "组装成引擎配置时必须饱和到 255"
        );
        parser.set_max_candidates(3);
        assert_eq!(parser.resolution_config().expect("配置").max_candidates, 3);

        // 非法 levels 串 ⇒ `None`（调用方必须报错退出）。
        parser.set_levels_spec(Some("l99".to_string()));
        assert!(parser.resolution_config().is_none(), "非法层号必须报错，不得静默忽略");
        parser.set_levels_spec(Some("not_a_level".to_string()));
        assert!(parser.resolution_config().is_none(), "无法识别的 token 必须报错");
    }

    /// **红线 2 的单元级证据**：把三个 flag 全打开，建图产出必须与默认（legacy）逐字段相同。
    ///
    /// 这条断言在 P2b 接线后**必须被替换成分叉基线**（见 `tests/test_p1_edges_unchanged.rs`
    /// 里同名的守卫与 `// P2b` 注释）。
    #[test]
    fn test_resolver_mode_layered_does_not_change_graph_output() {
        let temp_dir = tempdir().unwrap();
        let dir = temp_dir.path();
        fs::write(
            dir.join("alpha.rs"),
            "pub fn helper(x: i32) -> i32 { x + 1 }\npub fn use_it(x: i32) -> i32 { helper(x) }\n",
        )
        .unwrap();
        fs::write(dir.join("beta.rs"), "pub fn helper(y: i32) -> i32 { y * 2 }\n").unwrap();

        let signature = |layered: bool| {
            let mut parser = CodeParser::new();
            parser.set_force_reindex(true);
            if layered {
                parser.set_resolver_mode(ResolverMode::Layered);
                parser.set_max_candidates(2);
                parser.set_levels_spec(Some("all".to_string()));
            } else {
                // Phase P3：全局默认已切 layered，这里必须**显式**钉 Legacy，
                // 否则 signature(false) 拿到的就是 layered、mode_a 断言失效。
                parser.set_resolver_mode(ResolverMode::Legacy);
            }
            let graph = parser.build_petgraph_code_graph(dir).expect("建图必须成功");
            let mut nodes: Vec<(String, usize)> = graph
                .get_all_functions()
                .iter()
                .map(|f| (format!("{}:{}", f.name, f.line_start), 0))
                .collect();
            nodes.sort();
            let mut edges: Vec<(String, String, usize, bool)> = graph
                .graph
                .edge_references()
                .map(|e| {
                    let rel = e.weight();
                    (
                        rel.caller_name.clone(),
                        rel.callee_name.clone(),
                        rel.line_number,
                        rel.is_resolved,
                    )
                })
                .collect();
            edges.sort();
            // 引擎上下文也必须在两种模式下都建出来（"只建不消费"不等于"layered 时才建"）。
            (nodes, edges, parser.file_scope_count(), parser.resolver_mode())
        };

        let (nodes_a, edges_a, ctx_a, mode_a) = signature(false);
        let (nodes_b, edges_b, ctx_b, mode_b) = signature(true);
        assert_eq!(mode_a, ResolverMode::Legacy);
        assert_eq!(mode_b, ResolverMode::Layered);
        assert_eq!(nodes_a, nodes_b, "layered 不得改变任何函数节点");
        assert_eq!(edges_a, edges_b, "layered 不得改变任何调用边");
        assert_eq!(ctx_a, ctx_b, "ctx 的建立不得依赖 `--resolver`（两种模式都建）");
        assert!(!edges_a.is_empty(), "对照组必须有边，否则这条断言是空的");
    }
}
