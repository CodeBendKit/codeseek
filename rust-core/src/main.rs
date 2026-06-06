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
use tracing::{info, warn, error};

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
                tracing_subscriber::EnvFilter::new("info")
            }
        });
    tracing_subscriber::fmt().with_env_filter(filter_layer).init();

    let config = Config::load().ok();

    match &cli.command {
        Commands::Init { interactive } => {
            let project_root = detect_project()?;
            let (index_dir, lock_path) = project_paths(&project_root);
            let _lock = FileLock::exclusive(lock_path)?;

            info!("Initializing index for project: {:?}", project_root);
            info!("Index directory: {:?}", index_dir);

            // Use CodeAnalyzer to parse and build graph
            let mut analyzer = CodeAnalyzer::new();
            let result = analyzer.analyze_directory(&project_root);
            match result {
                Ok(code_graph) => {
                    let stats = code_graph.get_stats();
                    let mut pet_graph = PetCodeGraph::new();
                    for func in code_graph.functions.values() {
                        pet_graph.add_function(func.clone());
                    }
                    for rel in &code_graph.call_relations {
                        let _ = pet_graph.add_call_relation(rel.clone());
                    }
                    pet_graph.update_stats();

                    let project_hash = Config::compute_project_hash(&project_root);
                    let storage = Arc::new(StorageManager::new());
                    storage.get_persistence().save_graph(&project_hash, &pet_graph)?;
                    storage.set_graph(pet_graph);

                    println!("Index built successfully:");
                    println!("  Project:   {:?}", project_root);
                    println!("  Functions: {}", stats.total_functions);
                    println!("  Files:     {}", stats.total_files);

                    // Trigger embedding build if configured
                    if let Some(ref cfg) = config {
                        if !cfg.embedding.api_token.is_empty() {
                            info!("Starting embedding build...");
                            let db_path = index_dir.to_string_lossy().to_string();
                            let collection = format!("codeseek_{}", &project_hash[..8]);
                            if let Ok(es) = EmbeddingService::new(&db_path, collection, None, None).await {
                                // Embedding build will be done in Phase 2 task 12
                                let _ = es;
                                info!("Embedding service initialized");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to analyze project: {:?}", e);
                    return Err(format!("Analysis failed: {}", e).into());
                }
            }
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
            let (index_dir, lock_path) = project_paths(&project_root);
            let _lock = FileLock::shared(lock_path)?;

            let results = if let Some(ref cfg) = config {
                if !cfg.embedding.api_token.is_empty() {
                    let project_hash = Config::compute_project_hash(&project_root);
                    let collection = format!("codeseek_{}", &project_hash[..8]);
                    let db_path = index_dir.to_string_lossy().to_string();

                    if let Ok(es) = EmbeddingService::new(&db_path, collection, Some(cfg), None).await {
                        let bm25_dir = index_dir.join("tantivy_bm25");
                        let bm25_index = TantivyBm25Index::open_or_create(&bm25_dir)
                            .ok()
                            .map(|idx| Arc::new(idx) as Arc<dyn codeseek::storage::traits_bm25::TextSearchProvider>);

                        if let Some(bm25) = bm25_index {
                            let hybrid_cfg = &cfg.index.hybrid;
                            let hybrid = HybridSearchService::new(
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
                            );
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

            match results {
                Some(results) if *json => {
                    println!("{}", serde_json::to_string_pretty(&results)?);
                }
                Some(results) => {
                    if results.is_empty() {
                        println!("No results found.");
                    } else {
                        for (i, r) in results.iter().enumerate() {
                            println!("{}. {} ({:.4})", i + 1, r.symbol_name, r.final_score);
                            println!("   {}:{}", r.file_path, r.line_start);
                        }
                    }
                }
                None => {
                    println!("Semantic search not available. Run 'codeseek init' first and configure embedding.");
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
