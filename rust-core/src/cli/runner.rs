use tracing::info;
use crate::config::Config;

use super::args::{Cli, Commands};

pub struct CodeSeekRunner;

impl CodeSeekRunner {
    pub fn new() -> Self {
        Self
    }

    pub async fn run(cli: Cli, _config: Option<Config>) -> Result<(), Box<dyn std::error::Error>> {
        match cli.command {
            Commands::Init { interactive: _ } => {
                info!("init command - to be implemented in Phase 2");
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
            Commands::InstallHooks => {
                info!("install-hooks command - please use codeseek directly");
                Ok(())
            }
        }
    }
}
