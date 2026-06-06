use std::path::PathBuf;
use clap::Args;
use tracing::{info, warn};

use crate::codegraph::repository::RepositoryManager;

#[derive(Args)]
pub struct AnalyzeArgs {
    /// 要分析的仓库路径
    #[arg(short, long, default_value = ".")]
    path: PathBuf,

    /// 输出状态目录
    #[arg(short, long, default_value = "./.codegraph")]
    state_dir: PathBuf,

    /// 是否增量更新
    #[arg(short, long)]
    incremental: bool,

    /// 搜索查询
    #[arg(short, long)]
    search: Option<String>,

    /// 显示统计信息
    #[arg(short, long)]
    stats: bool,
}

pub fn run_analyze(args: &AnalyzeArgs) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting repository analysis for: {}", args.path.display());

    // 创建仓库管理器
    let mut repo_manager = RepositoryManager::new(args.path.clone());

    // 尝试加载现有状态
    if args.state_dir.exists() {
        if let Err(e) = repo_manager.load_state(&args.state_dir) {
            warn!("Failed to load existing state: {}", e);
            info!("Starting fresh analysis...");
        } else {
            info!("Loaded existing state from: {}", args.state_dir.display());
        }
    }

    if args.incremental {
        // 增量更新模式
        info!("Running in incremental mode");
        // 这里可以实现文件监控和增量更新逻辑
    } else {
        // 全量分析模式
        info!("Running full repository analysis");
        repo_manager.initialize()?;
    }

    // 显示统计信息
    if args.stats {
        let _stats = repo_manager.get_repository_stats();

    }

    // 执行搜索
    if let Some(query) = &args.search {
        info!("Searching for: {}", query);
        let results = repo_manager.search_entities(query);
        
        if results.is_empty() {
            // No results found
        } else {
            for result in results {
                println!("  {} [{}] - {}:{}:{} ({})", 
                    result.name, 
                    result.entity_type, 
                    result.file_path.display(), 
                    result.line_start, 
                    result.line_end,
                    result.language
                );
            }
        }
    }

    // 保存状态
    if let Err(e) = repo_manager.save_state(&args.state_dir) {
        warn!("Failed to save state: {}", e);
    } else {
        info!("Repository state saved to: {}", args.state_dir.display());
    }

    info!("Repository analysis completed successfully");
    Ok(())
} 