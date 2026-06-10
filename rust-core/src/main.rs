use clap::Parser;
use codeseek::cli::args::{Cli, Commands};
use codeseek::config::Config;
use codeseek::storage::lock::FileLock;
use codeseek::storage::StorageManager;
use codeseek::codegraph::types::PetCodeGraph;
use codeseek::services::CodeAnalyzer;
use codeseek::services::EmbeddingService;
use codeseek::services::hybrid_search::{HybridSearchService, HybridSearchConfig};
use codeseek::storage::TantivyBm25Index;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};
use codeseek::ui::progress::ProgressBar;
use codeseek::mcp;

/// 从当前工作目录检测项目根（向上找 .git/）
fn detect_project() -> Result<PathBuf, String> {
    Config::detect_project_root()
        .ok_or_else(|| "No project found. Run codeseek from within a git repository.".to_string())
}

/// 获取项目索引目录和锁路径
fn project_paths(project_root: &PathBuf) -> (PathBuf, PathBuf) {
    let hash = Config::compute_project_hash(project_root);
    let index_dir = Config::project_index_dir(&hash);
    let lock_path = index_dir.join(".lock");
    (index_dir, lock_path)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            if cli.verbose {
                tracing_subscriber::EnvFilter::new("debug")
            } else {
                tracing_subscriber::EnvFilter::new("warn")
            }
        });
    tracing_subscriber::fmt().with_env_filter(filter_layer).init();

    let config = Config::load().ok();

    match &cli.command {
        Commands::Init { interactive: _ } => {
            let project_root = detect_project()?;
            let (index_dir, lock_path) = project_paths(&project_root);
            let _lock = FileLock::exclusive(lock_path)?;

            eprintln!("  Project:    {}", project_root.display());
            eprintln!("  Index dir:  {}", index_dir.display());
            eprintln!();

            let project_hash = Config::compute_project_hash(&project_root);
            let storage = Arc::new(StorageManager::new());

            // Try loading existing graph for incremental update
            let existing_graph = storage.get_persistence().load_graph(&project_hash).ok().flatten();

            // Phase 1: Parse files (0→70%)
            let pb = ProgressBar::start("Scanning & parsing source files");
            pb.set_pct(5);
            let mut analyzer = CodeAnalyzer::new();
            let result = analyzer.analyze_directory(&project_root);
            let code_graph = match result {
                Ok(g) => g,
                Err(e) => {
                    pb.finish("failed");
                    return Err(format!("Analysis failed: {}", e).into());
                }
            };
            let new_stats = code_graph.get_stats();
            pb.set_pct(70);
            pb.set_stats(new_stats.total_files, new_stats.total_functions);

            // Phase 2: Build graph (70→85%)
            pb.set_phase("Building call graph");
            let mut pet_graph = existing_graph.unwrap_or_else(|| PetCodeGraph::new());

            if new_stats.total_functions > 0 {
                let changed_files: std::collections::HashSet<_> = code_graph.functions.values()
                    .map(|f| f.file_path.clone())
                    .collect();
                for file in &changed_files {
                    pet_graph.remove_functions_by_file(file);
                }
                for func in code_graph.functions.values() {
                    pet_graph.add_function(func.clone());
                }
                for rel in &code_graph.call_relations {
                    let _ = pet_graph.add_call_relation(rel.clone());
                }
                pet_graph.update_stats();
            }
            pb.set_pct(85);

            // Phase 3: Save graph (85→90%)
            pb.set_phase("Saving call graph");
            pb.set_pct(88);

            let meta = serde_json::json!({
                "project_root": project_root.to_string_lossy(),
                "indexed_at": chrono::Utc::now().to_rfc3339(),
            });
            std::fs::create_dir_all(&index_dir)?;
            std::fs::write(index_dir.join("project.json"), serde_json::to_string_pretty(&meta)?)?;

            let stats = pet_graph.get_stats().clone();
            storage.get_persistence().save_graph(&project_hash, &pet_graph)?;
            storage.set_graph(pet_graph);

            // Phase 4: Embedding (90→100%) — only if API token configured
            let mut embedding_done = false;
            if let Some(ref cfg) = config {
                if !cfg.embedding.api_token.is_empty() {
                    pb.set_pct(90);
                    pb.set_phase("Building vector embeddings...");
                    pb.set_stats(stats.total_files, stats.total_functions);

                    let db_path = Config::lancedb_dir(&project_hash).to_string_lossy().to_string();
                    let collection = format!("codeseek_{}", &project_hash[..8]);

                    // 创建BM25索引用于稀疏检索通道
                    let bm25_dir = Config::bm25_dir(&project_hash);
                    let bm25_index = TantivyBm25Index::open_or_create(&bm25_dir)
                        .ok()
                        .map(|idx| Arc::new(idx) as Arc<dyn codeseek::storage::traits_bm25::TextSearchProvider>);

                    embedding_done = true; // mark attempted; actual success tracked below
                    match EmbeddingService::new(&db_path, collection, Some(cfg), bm25_index).await {
                        Ok(es) => {
                            if let Err(e) = es.ensure_collection().await {
                                warn!("Embedding table setup failed: {}", e);
                                embedding_done = false;
                            } else {
                                // Load previous file hashes to detect deletions
                                let existing_hashes = EmbeddingService::load_hashes(&project_hash);
                                match es.vectorize_directory(
                                    &project_root.to_string_lossy(),
                                    existing_hashes.as_ref(),
                                ).await {
                                    Ok(new_hashes) => {
                                        pb.set_stats(new_hashes.len(), stats.total_functions);
                                        // Persist hashes for next incremental run
                                        if let Err(e) = EmbeddingService::save_hashes(&project_hash, &new_hashes) {
                                            warn!("Failed to save embedding hashes: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Embedding not available (LanceDB issue): {}. Graph-based search will be used.", e);
                                        embedding_done = false;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Embedding service unavailable: {}. Graph-based search will be used.", e);
                            embedding_done = false;
                        }
                    }
                }
            }
            pb.set_pct(100);

            let suffix = if embedding_done { " + embeddings" } else { "" };
            pb.finish(&format!("{} files, {} functions{}", stats.total_files, stats.total_functions, suffix));
        }
        Commands::Status { json } => {
            let project_root = detect_project()?;
            let (index_dir, _) = project_paths(&project_root);
            let project_hash = Config::compute_project_hash(&project_root);

            let storage = Arc::new(StorageManager::new());
            let status = match storage.get_persistence().load_graph(&project_hash) {
                Ok(Some(graph)) => {
                    let stats = graph.get_stats();
                    serde_json::json!({
                        "project_root": project_root,
                        "project_hash": project_hash,
                        "index_dir": index_dir,
                        "total_functions": stats.total_functions,
                        "total_files": stats.total_files,
                        "indexed": true,
                    })
                }
                _ => {
                    serde_json::json!({
                        "project_root": project_root,
                        "project_hash": project_hash,
                        "indexed": false,
                    })
                }
            };

            if *json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Project:     {:?}", project_root);
                println!("Indexed:     {}", status["indexed"]);
                if status["indexed"].as_bool().unwrap_or(false) {
                    println!("Functions:   {}", status["total_functions"]);
                    println!("Files:       {}", status["total_files"]);
                }
            }
        }
        Commands::Search { query, limit, json } => {
            let project_root = detect_project()?;
            let (_index_dir, lock_path) = project_paths(&project_root);
            let _lock = FileLock::shared(lock_path)?;

            let results = if let Some(ref cfg) = config {
                if !cfg.embedding.api_token.is_empty() {
                    let project_hash = Config::compute_project_hash(&project_root);
                    let collection = format!("codeseek_{}", &project_hash[..8]);
                    // LanceDB 存储在项目目录下的 lancedb/ 子目录
                    let db_path = Config::lancedb_dir(&project_hash).to_string_lossy().to_string();

                    if let Ok(es) = EmbeddingService::new(&db_path, collection, Some(cfg), None).await {
                        // BM25 索引存储在项目目录下的 tantivy_bm25/ 子目录
                        let bm25_dir = Config::bm25_dir(&project_hash);
                        let bm25_index = TantivyBm25Index::open_or_create(&bm25_dir)
                            .ok()
                            .map(|idx| Arc::new(idx) as Arc<dyn codeseek::storage::traits_bm25::TextSearchProvider>);

                        if let Some(bm25) = bm25_index {
                            let hybrid_cfg = &cfg.index.hybrid;
                            let reranker_cfg = &cfg.index.reranker;

                            let hybrid = if reranker_cfg.enabled && !reranker_cfg.api_token.is_empty() {
                                let reranker = codeseek::services::RerankerService::new(reranker_cfg.clone());
                                HybridSearchService::with_reranker(
                                    Arc::new(es),
                                    bm25,
                                    HybridSearchConfig {
                                        enable_sparse: hybrid_cfg.enable_bm25,
                                        rrf_k: hybrid_cfg.rrf_k,
                                        dense_limit: hybrid_cfg.vector_top_k,
                                        sparse_limit: hybrid_cfg.bm25_top_k,
                                        timeout_ms: 0,
                                        short_code_threshold: hybrid_cfg.short_code_threshold,
                                        short_code_penalty: hybrid_cfg.short_code_penalty,
                                    },
                                    Some(reranker),
                                )
                            } else {
                                HybridSearchService::new(
                                    Arc::new(es),
                                    bm25,
                                    HybridSearchConfig {
                                        enable_sparse: hybrid_cfg.enable_bm25,
                                        rrf_k: hybrid_cfg.rrf_k,
                                        dense_limit: hybrid_cfg.vector_top_k,
                                        sparse_limit: hybrid_cfg.bm25_top_k,
                                        timeout_ms: 0,
                                        short_code_threshold: hybrid_cfg.short_code_threshold,
                                        short_code_penalty: hybrid_cfg.short_code_penalty,
                                    },
                                )
                            };
                            Some(hybrid.search(query, *limit).await.unwrap_or_default())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // Fallback to graph-based name search when embeddings unavailable or empty
            let use_graph_fallback = results.as_ref().map(|r| r.is_empty()).unwrap_or(true);

            if use_graph_fallback {
                let storage = Arc::new(StorageManager::new());
                let project_hash = Config::compute_project_hash(&project_root);
                if let Ok(Some(graph)) = storage.get_persistence().load_graph(&project_hash) {
                    let funcs = graph.find_functions_by_name(query);
                    if *json {
                        let output: Vec<_> = funcs.iter().map(|f| serde_json::json!({
                            "name": f.name,
                            "file_path": f.file_path,
                            "line_start": f.line_start,
                            "line_end": f.line_end,
                            "language": f.language,
                        })).collect();
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    } else if funcs.is_empty() {
                        println!("No results found.");
                    } else {
                        for (i, f) in funcs.iter().enumerate().take(*limit) {
                            println!("{}. {} [{}]", i + 1, f.name, f.language);
                            println!("   {}:{}", f.file_path.display(), f.line_start);
                        }
                    }
                } else {
                    println!("No index found. Run 'codeseek init' first.");
                }
            } else {
                let results = results.unwrap();
                if *json {
                    println!("{}", serde_json::to_string_pretty(&results)?);
                } else {
                    if results.is_empty() {
                        println!("No results found.");
                    } else {
                        for (i, r) in results.iter().enumerate() {
                            println!("{}. {} ({:.4})", i + 1, r.symbol_name, r.final_score);
                            println!("   {}:{}", r.file_path, r.line_start);
                        }
                    }
                }
            }
        }
        Commands::Callers { symbol, json } => {
            let project_root = detect_project()?;
            let (_, lock_path) = project_paths(&project_root);
            let _lock = FileLock::shared(lock_path)?;

            let project_hash = Config::compute_project_hash(&project_root);
            let storage = Arc::new(StorageManager::new());

            match storage.get_persistence().load_graph(&project_hash) {
                Ok(Some(graph)) => {
                    let results = execute_callers(&graph, symbol, *json)?;
                    if !*json && results.trim().is_empty() {
                        println!("No callers found for '{}'", symbol);
                    } else {
                        print!("{}", results);
                    }
                }
                _ => {
                    println!("No index found. Run 'codeseek init' first.");
                }
            }
        }
        Commands::Callees { symbol, json } => {
            let project_root = detect_project()?;
            let (_, lock_path) = project_paths(&project_root);
            let _lock = FileLock::shared(lock_path)?;

            let project_hash = Config::compute_project_hash(&project_root);
            let storage = Arc::new(StorageManager::new());

            match storage.get_persistence().load_graph(&project_hash) {
                Ok(Some(graph)) => {
                    let results = execute_callees(&graph, symbol, *json)?;
                    if !*json && results.trim().is_empty() {
                        println!("No callees found for '{}'", symbol);
                    } else {
                        print!("{}", results);
                    }
                }
                _ => {
                    println!("No index found. Run 'codeseek init' first.");
                }
            }
        }
        Commands::Uninit { force } => {
            let project_root = detect_project()?;
            let (index_dir, lock_path) = project_paths(&project_root);

            if !index_dir.exists() {
                println!("No index found for this project.");
                return Ok(());
            }

            if !force {
                print!("Delete index for {:?}? [y/N] ", project_root);
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Aborted.");
                    return Ok(());
                }
            }

            let _lock = FileLock::exclusive(lock_path)?;
            std::fs::remove_dir_all(&index_dir)?;
            info!("Deleted index at {:?}", index_dir);
            println!("Index deleted: {:?}", project_root);

            // Clean up installed_hooks
            if let Some(ref mut cfg) = Config::load().ok() {
                let key = project_root.to_string_lossy().to_string();
                cfg.installed_hooks.remove(&key);
                cfg.save().ok();
            }
        }
        Commands::List { json } => {
            let projects_dir = Config::projects_dir();
            if !projects_dir.exists() {
                println!("No indexed projects found.");
                return Ok(());
            }

            let mut projects = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&projects_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let meta_file = path.join("project.json");
                        if meta_file.exists() {
                            if let Ok(content) = std::fs::read_to_string(&meta_file) {
                                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                                    let hash = path.file_name().unwrap_or_default().to_string_lossy();
                                    let graph_file = path.join("graph.bin");
                                    let functions = if graph_file.exists() {
                                        let s = graph_file.metadata().map(|m| m.len()).unwrap_or(0);
                                        format!("{}", s)
                                    } else { "—".to_string() };
                                    projects.push(serde_json::json!({
                                        "project_root": meta["project_root"],
                                        "hash": hash,
                                        "indexed_at": meta.get("indexed_at").map(|v| v.as_str().unwrap_or("")),
                                        "size": functions,
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            if *json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else if projects.is_empty() {
                println!("No indexed projects found.");
            } else {
                for p in &projects {
                    println!("  {}  →  {}", p["hash"].as_str().unwrap_or("?").chars().take(12).collect::<String>(), p["project_root"].as_str().unwrap_or("?"));
                }
            }
        }
        Commands::Serve { mcp } => {
            if !mcp {
                eprintln!("Use 'codeseek serve --mcp' for MCP stdio mode.");
                return Ok(());
            }
            mcp::server::run_mcp_server().await?;
        }
        Commands::Install => {
            install_to_claude()?;
            install_to_codex()?;
        }
        Commands::Uninstall => {
            uninstall_from_claude()?;
            uninstall_from_codex()?;
        }
        Commands::InstallHooks => {
            let project_root = detect_project()?;
            let git_dir = project_root.join(".git");
            if !git_dir.exists() {
                return Err("Not a git repository.".into());
            }

            let hooks_dir = git_dir.join("hooks");
            std::fs::create_dir_all(&hooks_dir)?;

            let hook_script = "#!/bin/sh\n# CodeSeek auto-index hook\ncodeseek init\n";

            for hook_name in &["post-commit", "post-merge"] {
                let hook_path = hooks_dir.join(hook_name);
                std::fs::write(&hook_path, hook_script)?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
                }
                info!("Installed git hook: {:?}", hook_path);
            }

            // Record in config
            if let Ok(mut cfg) = Config::load() {
                cfg.installed_hooks.insert(
                    project_root.to_string_lossy().to_string(),
                    vec!["post-commit".into(), "post-merge".into()],
                );
                cfg.save().ok();
            }

            println!("Git hooks installed: post-commit, post-merge");
            println!("Each hook runs 'codeseek init' for incremental indexing.");
        }
    }

    Ok(())
}

fn execute_callers(
    graph: &PetCodeGraph,
    symbol: &str,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let functions = graph.find_functions_by_name(symbol);
    let mut output = String::new();

    if json {
        let mut results = Vec::new();
        for func in &functions {
            for (caller, relation) in graph.get_callers(&func.id) {
                results.push(serde_json::json!({
                    "caller": caller.name,
                    "caller_file": caller.file_path,
                    "caller_line": relation.line_number,
                    "target": func.name,
                }));
            }
        }
        output = serde_json::to_string_pretty(&results)?;
    } else {
        for func in &functions {
            let callers = graph.get_callers(&func.id);
            if callers.is_empty() {
                output.push_str(&format!("No callers for '{}'\n", func.name));
            } else {
                output.push_str(&format!("Callers of '{}':\n", func.name));
                for (caller, relation) in callers {
                    output.push_str(&format!(
                        "  {} ({}:{})\n",
                        caller.name,
                        caller.file_path.display(),
                        relation.line_number
                    ));
                }
            }
        }
    }

    if functions.is_empty() {
        if json {
            output = "[]".to_string();
        }
    }

    Ok(output)
}

fn execute_callees(
    graph: &PetCodeGraph,
    symbol: &str,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let functions = graph.find_functions_by_name(symbol);
    let mut output = String::new();

    if json {
        let mut results = Vec::new();
        for func in &functions {
            for (callee, relation) in graph.get_callees(&func.id) {
                results.push(serde_json::json!({
                    "callee": callee.name,
                    "callee_file": callee.file_path,
                    "callee_line": relation.line_number,
                    "caller": func.name,
                }));
            }
        }
        output = serde_json::to_string_pretty(&results)?;
    } else {
        for func in &functions {
            let callees = graph.get_callees(&func.id);
            if callees.is_empty() {
                output.push_str(&format!("No callees for '{}'\n", func.name));
            } else {
                output.push_str(&format!("Callees of '{}':\n", func.name));
                for (callee, relation) in callees {
                    output.push_str(&format!(
                        "  {} ({}:{})\n",
                        callee.name,
                        callee.file_path.display(),
                        relation.line_number
                    ));
                }
            }
        }
    }

    if functions.is_empty() {
        if json {
            output = "[]".to_string();
        }
    }

    Ok(output)
}

// ── MCP Install / Uninstall helpers ────────────────────────────────────

fn codeseek_bin() -> String {
    "codeseek".to_string()
}

fn mcp_server_entry() -> serde_json::Value {
    serde_json::json!({
        "command": codeseek_bin(),
        "args": ["serve", "--mcp"]
    })
}

fn claude_global_mcp_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude.json")
}

fn claude_local_mcp_path() -> PathBuf {
    PathBuf::from(".mcp.json")
}

fn claude_settings_path(local: bool) -> PathBuf {
    let base = if local {
        PathBuf::from(".claude")
    } else {
        dirs::home_dir().unwrap_or_default().join(".claude")
    };
    base.join("settings.json")
}

fn codex_config_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".codex").join("config.toml")
}

fn install_to_claude() -> Result<(), Box<dyn std::error::Error>> {
    let local = claude_local_mcp_path();
    let (mcp_path, settings_path, _scope) = if std::env::current_dir()
        .map(|d| d.join(".mcp.json").exists())
        .unwrap_or(false)
        || local.exists()
    {
        (local, claude_settings_path(true), "local")
    } else {
        (claude_global_mcp_path(), claude_settings_path(false), "global")
    };

    // 1. Write MCP server entry
    let mut mcp_config: serde_json::Value = if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !mcp_config.get("mcpServers").is_some() {
        mcp_config["mcpServers"] = serde_json::json!({});
    }
    mcp_config["mcpServers"]["codeseek"] = mcp_server_entry();

    if let Some(parent) = mcp_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp_config)?)?;
    println!("  ✓ MCP config → {}", mcp_path.display());

    // 2. Write permissions
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if !settings.get("permissions").is_some() {
        settings["permissions"] = serde_json::json!({});
    }
    let allow = settings["permissions"]["allow"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let perms = ["Bash(codeseek *)"];
    let mut new_allow = allow.clone();
    for p in &perms {
        let s = p.to_string();
        if !allow.iter().any(|v| v.as_str() == Some(&s)) {
            new_allow.push(serde_json::json!(s));
        }
    }
    settings["permissions"]["allow"] = serde_json::json!(new_allow);

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("  ✓ Permissions → {}", settings_path.display());
    println!();
    println!("  Restart Claude Code to apply. codeseek tools will appear automatically.");

    Ok(())
}

fn install_to_codex() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = codex_config_path();
    if !config_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(());
    }

    let toml_block = format!(
        "[mcp_servers.codeseek]\ncommand = \"{}\"\nargs = [\"serve\", \"--mcp\"]\n",
        codeseek_bin()
    );

    let existing = if config_path.exists() {
        std::fs::read_to_string(&config_path)?
    } else {
        String::new()
    };

    let header = "[mcp_servers.codeseek]";
    if existing.contains(header) {
        let start = existing.find(header).unwrap();
        let end = existing[start..]
            .find("\n[")
            .map(|i| start + i)
            .unwrap_or(existing.len());
        let mut updated = existing[..start].to_string();
        updated.push_str(&toml_block);
        if end < existing.len() {
            updated.push_str(&existing[end..]);
        }
        std::fs::write(&config_path, updated.trim_end())?;
    } else {
        std::fs::create_dir_all(config_path.parent().unwrap())?;
        let content = if existing.is_empty() {
            toml_block
        } else {
            format!("{}\n\n{}", existing.trim_end(), toml_block)
        };
        std::fs::write(&config_path, content)?;
    }

    println!("  ✓ Codex config → {}", config_path.display());
    Ok(())
}

fn uninstall_from_claude() -> Result<(), Box<dyn std::error::Error>> {
    let mcp_path = claude_global_mcp_path();
    if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path)?;
        let mut config: serde_json::Value = serde_json::from_str(&content)?;
        if config.get("mcpServers").and_then(|s| s.get("codeseek")).is_some() {
            config["mcpServers"].as_object_mut().map(|s| s.remove("codeseek"));
            std::fs::write(&mcp_path, serde_json::to_string_pretty(&config)?)?;
            println!("  ✓ Removed from {}", mcp_path.display());
        }
    }
    Ok(())
}

fn uninstall_from_codex() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = codex_config_path();
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        let header = "[mcp_servers.codeseek]";
        if content.contains(header) {
            let start = content.find(header).unwrap();
            let end = content[start..]
                .find("\n[")
                .map(|i| start + i)
                .unwrap_or(content.len());
            let mut updated = content[..start].to_string();
            if end < content.len() {
                updated.push_str(&content[end..]);
            }
            std::fs::write(&config_path, updated.trim())?;
            println!("  ✓ Removed from {}", config_path.display());
        }
    }
    Ok(())
}
