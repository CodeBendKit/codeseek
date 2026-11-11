use tracing::info;
use crate::config::Config;

use super::args::{Cli, Commands};

pub struct CodeSeekRunner;

impl CodeSeekRunner {
    pub fn new() -> Self {
        Self
    }

    pub async fn run(cli: Cli, _config: Option<Config>) -> Result<(), Box<dyn std::error::Error>> {
        // `--force-reindex` 定义在 `Cli`（全局 flag）上，而下面的 `match cli.command`
        // 会移动 `cli.command`，所以必须在 match 之前先把这个 bool 取出来。
        let force_reindex = cli.force_reindex;
        // Phase P2a 交付物 3：同样先取出三个新全局 flag。
        // `--levels=` 必须在这里就校验：非法值立刻报错退出，**不得静默退回全开**，
        // 否则"关掉 L5"这类逐层诊断结论建立在错误的配置上。
        let resolver = cli.resolver;
        let max_candidates = cli.max_candidates;
        // Phase P3 收口 B（交付物 1）：`--levels` 取**非默认值**却没带 `--force-reindex`
        // ⇒ 直接失败。理由：`--levels` 改变产边结果，但它不在图缓存的解析档位戳里
        // （戳只有 `resolver_mode` / `max_candidates`），复用旧缓存会静默得到混档图。
        // 守卫本体在 `cli::args::Cli::validate_levels_against_cache`（与 `main.rs` 同一条），
        // 返回可读 Err 而非 panic；端到端行为由
        // `tests/test_p3_final.rs::p3b_levels_flag_fail_fast_requires_force_reindex` 钉住。
        // 顺序说明：本条只看 `enabled_levels` 是否偏离默认，**语法非法**的 spec 在这里被判
        // "不是非默认"（不触发本条），错误仍由下面的语法校验给出，两条错误不会互相盖掉。
        if let Err(message) = cli.validate_levels_against_cache() {
            return Err(message.into());
        }
        let levels = if cli.levels.trim().is_empty() {
            None
        } else {
            if cli.levels_spec_is_invalid() {
                return Err(format!(
                    "--levels 取值非法: {:?}（可用 all/p1/none/l1..l8/l2-l6/all,-l6/max=N，逗号分隔）",
                    cli.levels
                )
                .into());
            }
            Some(cli.levels)
        };

        match cli.command {
            Commands::Init { interactive: _ } => {
                // 真正的索引逻辑在 `src/main.rs` 的 `Commands::Init` 分支：
                // 该 flag 经 `CodeAnalyzer::set_force_reindex` → `CodeParser::set_force_reindex`
                // 让三个缓存读取点一律返回"缓存 miss"。
                info!(
                    "init command - to be implemented in Phase 2 (force_reindex: {})",
                    force_reindex
                );
                // Phase P2a：三个新 flag 已经过 clap 解析并在此转发；
                // ⚠ P2a 它们**不改变任何产边结果**（默认 legacy，layered 尚未接入建图管线）。
                info!(
                    "resolver mode = {:?}, max_candidates = {}, levels = {:?}",
                    resolver, max_candidates, levels
                );
                Ok(())
            }
            Commands::Status { json: _ } => {
                info!("status command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Search { query: _, limit: _, json: _ } => {
                info!("search command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Callers { symbol: _, json: _ } => {
                info!("callers command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Callees { symbol: _, json: _ } => {
                info!("callees command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Callgraph { symbol: _, depth: _, json: _ } => {
                info!("callgraph command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Uninit { force: _ } => {
                info!("uninit command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::List { json: _ } => {
                info!("list command - please use codeseek directly");
                Ok(())
            }
            Commands::Serve { mcp: _ } => {
                info!("serve command - please use codeseek directly");
                Ok(())
            }
            Commands::Install => {
                info!("install command - please use codeseek directly");
                Ok(())
            }
            Commands::Uninstall => {
                info!("uninstall command - please use codeseek directly");
                Ok(())
            }
            Commands::Skeleton { file_paths: _, json: _ } => {
                info!("skeleton command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::Snippet { function_name: _, file_path: _, json: _ } => {
                info!("snippet command - to be implemented in Phase 2");
                Ok(())
            }
            Commands::InstallHooks => {
                info!("install-hooks command - please use codeseek directly");
                Ok(())
            }
            Commands::Knowledge { action } => {
                crate::knowledge::cli::handle_knowledge_command(action).await?;
                Ok(())
            }
        }
    }
}
