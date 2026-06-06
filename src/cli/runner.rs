use tracing::info;
use crate::config::Config;
use crate::http::server::CodeSeekServer;
use crate::storage::StorageManager;
use std::sync::Arc;

use super::args::{Cli, Commands};
use super::vectorize::run_vectorize;

pub struct CodeSeekRunner;

impl CodeSeekRunner {
    pub fn new() -> Self {
        Self
    }

    pub async fn run(cli: Cli, config: Option<Config>) -> Result<(), Box<dyn std::error::Error>> {
        match cli.command {
            Commands::Server { address, storage_mode, repo_path } => {
                info!("Starting server mode, repo: {}", repo_path);

                let mode = storage_mode.unwrap_or_else(|| cli.storage_mode.clone());
                let storage = match config {
                    Some(cfg) => Arc::new(StorageManager::with_config(mode, cfg)),
                    None => Arc::new(StorageManager::with_storage_mode(mode)),
                };

                let addr = address.unwrap_or_else(|| "127.0.0.1:3000".to_string());
                let mut server = CodeSeekServer::new(storage, repo_path);
                server.start(&addr).await?;
            }
            Commands::Vectorize { path, collection, db_uri } => {
                info!("Starting embedding mode");
                run_vectorize(path, collection, db_uri, config).await?;
            }
        }

        Ok(())
    }
}