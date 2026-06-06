use clap::Parser;
use codeseek::cli::{Cli, CodeSeekRunner};
use codeseek::cli::args::Commands;
use codeseek::http::CodeSeekServer;
use codeseek::storage::StorageManager;
use codeseek::config::Config;
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Initialize logging
    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            if cli.verbose {
                tracing_subscriber::EnvFilter::new("debug")
            } else {
                tracing_subscriber::EnvFilter::new("info")
            }
        });

    tracing_subscriber::fmt()
        .with_env_filter(filter_layer)
        .init();

    // Load configuration
    let config = match Config::load() {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("Failed to load configuration: {}", e);
            None
        }
    };

    match &cli.command {
        Commands::Server { address, storage_mode, repo_path } => {
            let default_addr = format!("127.0.0.1:{}", 12700);
            let server_addr = address.as_deref().unwrap_or(&default_addr);
            info!("Starting CodeSeek HTTP server on {}, repo: {}", server_addr, repo_path);

            // Determine storage mode
            let storage_mode = storage_mode.as_ref().unwrap_or(&cli.storage_mode).clone();
            info!("Using storage mode: {:?}", storage_mode);

            let storage = if let Some(cfg) = config {
                Arc::new(StorageManager::with_config(storage_mode, cfg))
            } else {
                Arc::new(StorageManager::with_storage_mode(storage_mode))
            };

            let mut server = CodeSeekServer::new(storage, repo_path.clone());
            server.start(server_addr).await?;
        }
        Commands::Vectorize { .. } => {
            CodeSeekRunner::run(cli, config).await?;
        }
    }

    Ok(())
}
