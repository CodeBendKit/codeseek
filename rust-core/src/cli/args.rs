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

#[derive(Parser, Debug)]
#[clap(name = "codeseek", author, version, about = "Code intelligence CLI tool", long_about = None)]
pub struct Cli {
    /// Verbose mode
    #[clap(short, long, action)]
    pub verbose: bool,

    #[clap(subcommand)]
    pub command: Commands,
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
    /// Install git hooks (post-commit, post-merge → codeseek init)
    InstallHooks,
}
