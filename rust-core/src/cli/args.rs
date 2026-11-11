use clap::{Parser, Subcommand, ValueEnum};

/// Storage format for persisted call graphs
#[derive(Debug, Clone, ValueEnum)]
pub enum StorageMode {
    /// JSON only
    Json,
    /// Binary (bincode) only
    Binary,
    /// Both JSON and binary
    Both,
}

impl Default for StorageMode {
    fn default() -> Self {
        StorageMode::Binary
    }
}

/// 调用边 callee 的解析引擎（对应 CLI 的 `--resolver`）。
///
/// - `Layered`（**默认**）：分层精度解析（L1 词法 → L2 外围类型 → L3 receiver 类型 →
///   L4 import 绑定 → L5 限定名 → L6 同模块 → L7 全仓唯一 → L8 启发式；level 0 为
///   `Unresolved` 占位）。歧义调用点每个候选一条边，并把层级 / 置信度 / 候选总数写进
///   `CallRelation`。分层引擎已接入全部建图路径（全量 / 刷新 / 增量），四条产边路径统一经
///   `PetCodeGraph::upsert_call_site_edges` 发射；`CallRelation` 已带解析元信息，
///   `GRAPH_SCHEMA_VERSION` 现为 3。
/// - `Legacy`（**仅作回滚逃生口，非默认**）：旧的同名 first-wins，行为与改造前逐字节一致，
///   数值红线由 `tests/test_p1_edges_unchanged.rs` 守护。legacy 侧仍存在的解析入口为
///   `codegraph/parser.rs` 的 `_resolve_callee_function` /
///   `_resolve_qualified_function_name` / `_build_function_name_index`，以及全量档的
///   `_analyze_file_calls_for_petgraph` / `emit_legacy_call_edges_for_file`。
///
/// 完整设计、图缓存契约与回滚口径见 `docs/call-graph-resolution.md`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResolverMode {
    /// 回滚逃生口（**非默认**）：名字相等即命中、多候选取第一个，行为与改造前逐字节一致。
    Legacy,
    /// 分层精度解析（**已生效且为默认**）：见本类型文档与 `docs/call-graph-resolution.md`。
    Layered,
}

impl Default for ResolverMode {
    /// Phase P3 起默认 **layered**（分层精度解析）。`legacy` 保留为回滚逃生口，
    /// 行为与 P1 逐字节一致，但**不再是默认档**。
    fn default() -> Self {
        ResolverMode::Layered
    }
}

impl ResolverMode {
    /// 落盘用的**稳定**档位名（`graph.json` / `graph.bin` 的解析档位戳就存这个字符串）。
    ///
    /// 为什么必须有这个方法而不是 `format!("{:?}", mode).to_lowercase()`：
    /// 戳字段一旦写进缓存文件就成了**持久化契约**，绝不能跟着 Rust 变体的 Debug 打印
    /// 或重命名而漂移。取值域固定为 `"legacy"` / `"layered"`，与 `--resolver` 的
    /// ValueEnum 名称一致；空串 `""` 在戳的语义里表示"未知/旧文件"（见
    /// `storage::petgraph_storage::PetGraphStorage::resolver_mode`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolverMode::Legacy => "legacy",
            ResolverMode::Layered => "layered",
        }
    }
}

#[derive(Parser, Debug)]
#[clap(name = "codeseek", author, version, about = "Code intelligence CLI tool", long_about = None)]
pub struct Cli {
    /// Verbose mode
    #[clap(short, long, action)]
    pub verbose: bool,

    /// 忽略所有已持久化的缓存（图缓存 + 文件 MD5 哈希），强制全量重新分析后重新落盘。
    ///
    /// 这是图缓存 schema 版本门禁的**逃生口**：当缓存被判定为失效、或怀疑缓存与源码
    /// 不一致时，用 `codeseek --force-reindex init` 保证一次干净的从零构建。
    #[clap(long, action)]
    pub force_reindex: bool,

    /// Phase P2a/P2b/P3：callee 解析引擎。**Phase P3 起默认 `layered`**。
    ///
    /// - `layered`（默认）= 分层精度解析（L1 词法 → L2 外围类型 → L3 receiver 类型 →
    ///   L4 import 绑定 → L5 限定名 → L6 同模块 → L7 全仓唯一 → L8 启发式），
    ///   歧义调用点**每个候选一条边**，并把层级/置信度/候选总数写进边；
    /// - `legacy` = 旧的同名匹配（名字相等就命中、多候选取第一个），**仅作回滚逃生口**：
    ///   怀疑分层引擎把某仓产边弄坏时，`--resolver=legacy` 一键退回与 P1 逐字节一致的行为。
    ///
    /// 默认值在 Phase P3 由 `legacy` 切到 `layered`（P2 阶段逐字节验证已完成，红线解除）。
    /// 分层解析的完整设计、缓存契约与回滚口径见 `docs/call-graph-resolution.md`。
    #[clap(
        long,
        value_enum,
        default_value = "layered",
        help = "调用边解析引擎：layered（默认）= 分层精度解析（每候选一条边，带层级/置信度）；legacy = 旧的同名匹配（命中第一个），仅作回滚逃生口"
    )]
    pub resolver: ResolverMode,

    /// Phase P2a/P2b：单个调用点最多落盘多少个候选。
    ///
    /// 只**截断落盘条数**：`candidate_count` 仍写截断前的真实候选数，
    /// `is_resolved` 也仍按真实数判定 ⇒ `--max-candidates=1` 不会把歧义伪装成唯一。
    #[clap(
        long,
        default_value = "8",
        help = "单个调用点最多落盘的候选条数（超出丢弃但仍计入 candidate_count，且不改变 is_resolved 判定）"
    )]
    pub max_candidates: usize,

    /// Phase P2a：分层诊断开关，只启用/禁用指定的层（**空 = 全开**）。
    ///
    /// 语法见 `codegraph::resolution::resolver::ResolutionConfig::from_levels_spec`：
    /// `all` / `p1` / `none` / `l1,l5` / `l2-l6` / `all,-l6` / `max=4`，逗号分隔、大小写不敏感。
    /// 非法取值会**直接报错退出**，绝不静默退回全开 —— 否则"关掉某层"的诊断结论不可信。
    ///
    /// ⚠ **缓存约束（Phase P3 收口 B）**：`--levels` 会改变产出的调用边，但它**不在**
    /// 图缓存的解析档位戳里（戳只有 `resolver_mode` / `max_candidates`），因此取**非默认值**
    /// （默认 = 全层开启）时**必须同时传 `--force-reindex`**，否则 CLI 在建图之前就报错退出。
    /// 正确用法：`codeseek --levels=l1,l7 --force-reindex init`。守卫本体就是本文件的
    /// [`Cli::validate_levels_against_cache`]（在 `src/main.rs` 与 `src/cli/runner.rs`
    /// 两处派发点各调用一次，都早于任何缓存读取），端到端行为由
    /// `tests/test_p3_final.rs::p3b_levels_flag_fail_fast_requires_force_reindex` 钉住。
    #[clap(
        long,
        default_value = "",
        help = "只启用指定解析层（空=全开）：all/p1/none/l1..l8/l2-l6/all,-l6；仅对 --resolver=layered 生效。⚠ 非默认取值必须同时传 --force-reindex（它不入图缓存档位戳，否则会复用旧档位建出的缓存图）"
    )]
    pub levels: String,

    #[clap(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// `--levels` 的取值是否**偏离默认**（默认 = 空串 = 全层开启）。
    ///
    /// 判定只看 `enabled_levels`，不看 `max_candidates`：`--levels=` 里的 `max=N` 会被
    /// `CodeParser::resolution_config()` 用 `--max-candidates` 覆盖掉，因此真正影响产边的
    /// 只有层位掩码。等价于全开的写法（`all`、`l1-l8`）都判为"默认"⇒ 不触发 fail-fast。
    ///
    /// ⚠ 比较时**忽略 bit0（`Unresolved` 占位层）**：那一层是"没解析出来"的占位，
    /// 引擎判定从不消费它，写 `l1-l8`（掩码 `0x01FE`）与写 `all`（`0x01FF`）产边完全相同，
    /// 因此不能被误判成"非默认"而逼用户多传一个 `--force-reindex`。
    ///
    /// 非法 spec 在这里判为"默认"（即不触发本条守卫）：它的报错由既有语法校验负责
    /// （`cli/runner.rs` 与本文件 [`Cli::levels_spec_is_invalid`]），两条错误不该混成一条。
    pub fn levels_deviates_from_default(&self) -> bool {
        use crate::codegraph::resolution::resolver::ResolutionConfig;
        let spec = self.levels.trim();
        if spec.is_empty() {
            return false;
        }
        // 参与比较的有效层位：L1..L8（bit0 = `Unresolved` 占位层，恒不参与）
        const EFFECTIVE_LEVELS: u16 = ResolutionConfig::ALL_LEVELS & !0b1u16;
        match ResolutionConfig::from_levels_spec(spec) {
            Some(cfg) => cfg.enabled_levels & EFFECTIVE_LEVELS != EFFECTIVE_LEVELS,
            None => false,
        }
    }

    /// `--levels` 是否**语法非法**（非空、且 `from_levels_spec` 解析不出来）。
    pub fn levels_spec_is_invalid(&self) -> bool {
        let spec = self.levels.trim();
        !spec.is_empty()
            && crate::codegraph::resolution::resolver::ResolutionConfig::from_levels_spec(spec)
                .is_none()
    }

    /// Phase P3 收口 B（交付物 1）：**`--levels` 的缓存陈旧风险用 fail-fast 关掉**。
    ///
    /// ## 为什么是 fail-fast，而不是再升一次 schema
    /// `--levels` 改变产边结果，但图缓存的解析档位戳只有 `resolver_mode` +
    /// `max_candidates`（`storage::petgraph_storage::PetGraphStorage`）。两条可选修法：
    ///  - **(A) 把 levels 掩码也写进戳** ⇒ 必须再升 `GRAPH_SCHEMA_VERSION` 3→4，
    ///    代价是**所有用户刚重建过的缓存整体作废**，而且日常不使用 `--levels` 的人
    ///    也跟着付一次全量重建的账（诊断开关不该有这种辐射面）；
    ///  - **(B) 非默认 `--levels` 必须配 `--force-reindex`**（本函数）⇒ 缓存格式零变更，
    ///    只有**真的在做逐层诊断**的那次运行被要求强制全量重建，代价落在唯一需要它的人身上。
    /// 本任务选定 **(B)**。
    ///
    /// ## 语义
    /// 只在**会读写图缓存的子命令**（`init`）上生效；其它子命令不建图，拒绝它们没有任何
    /// 收益，只会把一个纯诊断 flag 变成全局可用性障碍。判定条件严格照任务口径：
    /// `--levels` 取非默认值 **且** 未传 `--force-reindex` ⇒ 返回**可操作的用户错误**
    /// （`Err(String)`，由调用方 `return Err(...)` 走 CLI 的正常失败路径，**绝不 panic**）。
    pub fn validate_levels_against_cache(&self) -> Result<(), String> {
        // 只有 `init` 会读/写图缓存（其余子命令既不产边也不落缓存）
        let builds_graph = matches!(self.command, Commands::Init { .. });
        if !builds_graph {
            return Ok(());
        }
        if !self.levels_deviates_from_default() {
            return Ok(());
        }
        if self.force_reindex {
            return Ok(());
        }
        Err(format!(
            "`--levels` 是分层解析器的诊断开关，会改变产出的调用边，但它不在图缓存的解析档位戳里\
             （戳只有 `resolver_mode` / `max_candidates`）。\
             当前 `--levels=\"{levels}\"` 偏离默认（全层开启），而本轮没有传 `--force-reindex`：\
             继续复用已有图缓存会得到\"半新半旧\"的混档图，逐层诊断的结论不可信。\n\
             请二选一：\n\
             \x20 1) 同时传 `--force-reindex` 以获得一致结果（推荐）：\n\
             \x20    codeseek --levels={levels} --force-reindex init\n\
             \x20 2) 去掉 `--levels`（= 全层开启，可用缓存）：\n\
             \x20    codeseek init",
            levels = self.levels.trim(),
        ))
    }
}


#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Build or update the code index (full on first run, MD5-incremental thereafter)
    Init {
        /// Interactive configuration wizard
        #[clap(short = 'i', long, action)]
        interactive: bool,
    },
    /// Show index statistics (functions, files, last update)
    Status {
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Semantic code search (vector + BM25 + RRF fusion)
    Search {
        /// Search query text
        query: String,
        /// Maximum results to return
        #[clap(short, long, default_value = "10")]
        limit: usize,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Find functions that call the given symbol
    Callers {
        /// Function or symbol name
        symbol: String,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Find functions called by the given symbol
    Callees {
        /// Function or symbol name
        symbol: String,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Query function call graph with depth (bi-directional)
    Callgraph {
        /// Function name to query as center node
        symbol: String,
        /// Query depth — layers of callers and callees to include (1-3)
        #[arg(short = 'd', long = "depth", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=3))]
        depth: u32,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Delete the current project's index data
    Uninit {
        /// Skip confirmation prompt
        #[clap(long, action)]
        force: bool,
    },
    /// List all indexed projects
    List {
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Start MCP server (for Claude Code / Codex integration)
    Serve {
        /// Run in MCP stdio mode
        #[clap(long, action)]
        mcp: bool,
    },
    /// Register codeseek as MCP tools in Claude Code / Codex
    Install,
    /// Remove codeseek MCP integration from Claude Code / Codex
    Uninstall,
    /// Show the code skeleton (function signatures without implementation) for one or more files
    Skeleton {
        /// File paths to show skeletons for (absolute or relative to project root)
        #[arg(required = true)]
        file_paths: Vec<String>,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Show the code snippet for a specific function
    Snippet {
        /// Function name to look up
        function_name: String,
        /// File path to disambiguate if multiple functions share the same name
        #[arg(short, long = "file-path")]
        file_path: Option<String>,
        /// Output as JSON
        #[clap(long, action)]
        json: bool,
    },
    /// Install git hooks (post-commit, post-merge → codeseek init)
    InstallHooks,
    /// Manage the persistent knowledge base (add/search/list/delete)
    Knowledge {
        #[clap(subcommand)]
        action: KnowledgeCommand,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum KnowledgeCommand {
    /// Add a knowledge record to the knowledge base
    Add {
        /// Title (≤30 chars)
        #[arg(long)]
        title: String,
        /// Content (≤500 chars)
        #[arg(long)]
        content: String,
        /// Knowledge type: repo_retrieval | coding_modification
        #[arg(long)]
        r#type: String,
        /// Tags, comma-separated
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Related file paths, comma-separated
        #[arg(long, value_delimiter = ',')]
        related_files: Vec<String>,
        /// Source agent: repo_agent | coding_agent
        #[arg(long)]
        source_agent: String,
        /// Task ID
        #[arg(long)]
        task_id: String,
        /// Confidence 0.0-1.0
        #[arg(long, default_value_t = 0.8)]
        confidence: f32,
    },
    /// Search the knowledge base
    Search {
        /// Query text
        #[arg(long)]
        query: String,
        /// Max results
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Enable reranking (currently a no-op placeholder)
        #[arg(long, action)]
        rerank: bool,
        /// Domain filter: repo, coding (comma-separated; empty = all domains)
        #[arg(long, value_delimiter = ',')]
        domains: Vec<String>,
    },
    /// List knowledge records
    List {
        /// Max results
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Filter by type
        #[arg(long)]
        r#type: Option<String>,
        /// Filter by tag (match any)
        #[arg(long)]
        tag: Option<String>,
    },
    /// Delete a knowledge record by ID
    Delete {
        /// Record ID (kn_*)
        #[arg(long)]
        id: String,
    },
}
