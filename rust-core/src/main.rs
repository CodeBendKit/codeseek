use clap::Parser;
use codeseek::cli::args::{Cli, Commands};
use codeseek::codegraph::types::{FunctionInfo, PetCodeGraph};
use codeseek::config::Config;
use codeseek::mcp;
use codeseek::services::hybrid_search::{HybridSearchConfig, HybridSearchService};
use codeseek::services::CodeAnalyzer;
use codeseek::services::EmbeddingService;
use codeseek::storage::lock::FileLock;
use codeseek::storage::petgraph_storage::clamp_max_candidates;
use codeseek::storage::StorageManager;
use codeseek::storage::TantivyBm25Index;
use codeseek::ui::progress::ProgressBar;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

// Treesitter AST skeletonizer imports
use codeseek::codegraph::treesitter::ast_instance_structs::SymbolInformation;
use codeseek::codegraph::treesitter::make_formatter;
use codeseek::codegraph::treesitter::parsers::get_ast_parser_by_filename;
use codeseek::codegraph::treesitter::structs::SymbolType;

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

    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if cli.verbose {
            tracing_subscriber::EnvFilter::new("debug")
        } else {
            tracing_subscriber::EnvFilter::new("warn")
        }
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter_layer)
        .init();

    // Phase P3 收口 B（交付物 1）：`--levels` 的**缓存陈旧** fail-fast。
    // 必须在这里（任何缓存/图读取之前）就拦住：`--levels` 会改变产出的调用边，但它
    // **不在**图缓存的解析档位戳里（戳只有 `resolver_mode` / `max_candidates`），
    // 所以"改了 `--levels` 却复用旧缓存"会静默得到一张混档图。
    // 返回的是**用户可读的 Err**（非 panic、非 unwrap），文案自带正确用法示例。
    // 为什么不给 `--levels` 再升一次 schema（3→4）：那会让所有用户刚重建过的缓存整体作废，
    // 而代价只该由"真的在做逐层诊断"的那次运行付。守卫本体见
    // `cli::args::Cli::validate_levels_against_cache`（与本文件、`cli/runner.rs` 同一条）。
    if let Err(message) = cli.validate_levels_against_cache() {
        return Err(message.into());
    }

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
            // `--force-reindex`：跳过已有图缓存，本次一定是干净的从零构建。
            //
            // Phase P3 收口：这里**同样**要做解析档位门禁。`CodeParser::_load_existing_graph`
            // 已经拦了一道，但 init 这条链路是把 `existing_graph` 直接当作本轮结果的基础
            // （下面 `pet_graph = existing_graph.unwrap_or_else(...)`），文件 MD5 未变的旧档
            // 节点/边会被原样保留 ⇒ 仍然得到 legacy/layered 混档图。
            // 判定复用同一个 `validate_resolution_stamp`（**不另写一套规则**）：不通过 ⇒
            // 丢弃旧图 + 日志 ⇒ 等价于"缓存 miss"，由后面的全量解析重建。绝不 panic。
            let existing_graph = if cli.force_reindex {
                info!("--force-reindex 已置位：跳过已有图缓存，强制全量重建");
                None
            } else {
                let expected_mode = cli.resolver.as_str();
                let expected_max_candidates = clamp_max_candidates(cli.max_candidates);
                storage
                    .get_persistence()
                    .load_graph_storage(&project_hash)
                    .ok()
                    .flatten()
                    .and_then(|cached| {
                        match cached.validate_resolution_stamp(expected_mode, expected_max_candidates) {
                            Ok(()) => Some(cached.to_petgraph()),
                            Err(reason) => {
                                info!("已有图缓存与本轮解析档位不符，按缓存失效处理（全量重建）：{reason}");
                                None
                            }
                        }
                    })
            };

            // Phase 1: Parse files (0→70%)
            let pb = ProgressBar::start("Scanning & parsing source files");
            pb.set_pct(5);
            let mut analyzer = CodeAnalyzer::new();
            // 把 `--force-reindex` 传进解析器：三个缓存读取点（已有图 / 文件 MD5 哈希）
            // 一律返回"缓存 miss"，保证本次全量重新解析。
            analyzer.set_force_reindex(cli.force_reindex);
            // Phase P2a 交付物 3：`--resolver` / `--max-candidates` / `--levels=` 走同一条注入链。
            // ⚠ 本阶段这三个值**不参与任何产边决策**（默认 legacy；layered 尚未接入管线），
            //   因此它们对 `analyze_directory` 的结果零影响 —— 由
            //   `tests/test_p1_edges_unchanged.rs::p2a_guard_cli_flags_are_plumbed_but_inert` 钉死。
            //   非法的 `--levels` 一律在建图之前报错退出，绝不静默退回全开。
            if let Err(err) = analyzer.set_resolver_options(
                cli.resolver,
                cli.max_candidates,
                Some(cli.levels.clone()),
            ) {
                return Err(err.into());
            }
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
                let changed_files: std::collections::HashSet<_> = code_graph
                    .functions
                    .values()
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
            std::fs::write(
                index_dir.join("project.json"),
                serde_json::to_string_pretty(&meta)?,
            )?;

            let stats = pet_graph.get_stats().clone();
            // Phase P3 收口：落盘必须带上**本轮建图用的**解析档位戳（档位 + 候选上限），
            // 二者都取自真正产边的 `analyzer` 里的 `CodeParser`，不是再读一遍 CLI ——
            // 只有同源，下一轮 `_load_existing_graph` / 上面的直读点才可能判出"同档命中"。
            storage.get_persistence().save_graph(
                &project_hash,
                &pet_graph,
                analyzer.resolver_mode().as_str(),
                clamp_max_candidates(analyzer.max_candidates()),
            )?;
            storage.set_graph(pet_graph);

            // Phase 4: Embedding (90→100%) — only if API token configured
            let mut embedding_done = false;
            if let Some(ref cfg) = config {
                if !cfg.embedding.api_token.is_empty() {
                    pb.set_pct(90);
                    pb.set_phase("Building vector embeddings...");
                    pb.set_stats(stats.total_files, stats.total_functions);

                    let db_path = Config::lancedb_dir(&project_hash)
                        .to_string_lossy()
                        .to_string();
                    let collection = format!("codeseek_{}", &project_hash[..8]);

                    // 创建BM25索引用于稀疏检索通道
                    let bm25_dir = Config::bm25_dir(&project_hash);
                    // 先保留具体类型实例：转成 trait object 后就读不到 was_rebuilt() 了
                    let bm25_concrete = TantivyBm25Index::open_or_create(&bm25_dir).ok();
                    // BM25 索引若因 schema/tokenizer 变更被重建，稀疏通道此刻是空的
                    let bm25_rebuilt = bm25_concrete
                        .as_ref()
                        .map(|i| i.was_rebuilt())
                        .unwrap_or(false);
                    let bm25_index = bm25_concrete.map(|idx| {
                        Arc::new(idx) as Arc<dyn codeseek::storage::traits_bm25::TextSearchProvider>
                    });

                    embedding_done = true; // mark attempted; actual success tracked below
                    match EmbeddingService::new(&db_path, collection, Some(cfg), bm25_index).await {
                        Ok(es) => {
                            if let Err(e) = es.ensure_collection().await {
                                warn!("Embedding table setup failed: {}", e);
                                embedding_done = false;
                            } else {
                                // Load previous file hashes to detect deletions
                                // BM25 索引刚被重建时，增量索引不会回填稀疏通道（已索引文件不会重新生成
                                // CodeChunk），必须传 None 走全量索引，否则稀疏检索会静默变空。
                                let existing_hashes = if bm25_rebuilt {
                                    warn!(
                                        "BM25 索引已因 schema/tokenizer 变更重建，本轮改为全量索引以回填稀疏通道"
                                    );
                                    None
                                } else {
                                    EmbeddingService::load_hashes(&project_hash)
                                };
                                match es
                                    .vectorize_directory(
                                        &project_root.to_string_lossy(),
                                        existing_hashes.as_ref(),
                                    )
                                    .await
                                {
                                    Ok(new_hashes) => {
                                        pb.set_stats(new_hashes.len(), stats.total_functions);
                                        // Persist hashes for next incremental run
                                        if let Err(e) = EmbeddingService::save_hashes(
                                            &project_hash,
                                            &new_hashes,
                                        ) {
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
            pb.finish(&format!(
                "{} files, {} functions{}",
                stats.total_files, stats.total_functions, suffix
            ));
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
                    let db_path = Config::lancedb_dir(&project_hash)
                        .to_string_lossy()
                        .to_string();

                    if let Ok(es) =
                        EmbeddingService::new(&db_path, collection, Some(cfg), None).await
                    {
                        // BM25 索引存储在项目目录下的 tantivy_bm25/ 子目录
                        let bm25_dir = Config::bm25_dir(&project_hash);
                        let bm25_index =
                            TantivyBm25Index::open_or_create(&bm25_dir).ok().map(|idx| {
                                Arc::new(idx)
                                    as Arc<dyn codeseek::storage::traits_bm25::TextSearchProvider>
                            });

                        if let Some(bm25) = bm25_index {
                            let hybrid_cfg = &cfg.index.hybrid;
                            let reranker_cfg = &cfg.index.reranker;

                            let hybrid = if reranker_cfg.enabled
                                && !reranker_cfg.api_token.is_empty()
                            {
                                let reranker =
                                    codeseek::services::RerankerService::new(reranker_cfg.clone());
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
                                        cutoff: hybrid_cfg.cutoff.clone(),
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
                                        cutoff: hybrid_cfg.cutoff.clone(),
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
                        let output: Vec<_> = funcs
                            .iter()
                            .map(|f| {
                                serde_json::json!({
                                    "name": f.name,
                                    "file_path": f.file_path,
                                    "line_start": f.line_start,
                                    "line_end": f.line_end,
                                    "language": f.language,
                                })
                            })
                            .collect();
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
        Commands::Callgraph {
            symbol,
            depth,
            json,
        } => {
            let project_root = detect_project()?;
            let (_, lock_path) = project_paths(&project_root);
            let _lock = FileLock::shared(lock_path)?;
            let project_hash = Config::compute_project_hash(&project_root);
            let storage = Arc::new(StorageManager::new());
            match storage.get_persistence().load_graph(&project_hash) {
                Ok(Some(graph)) => {
                    let results = execute_callgraph(&graph, symbol, *depth, *json)?;
                    if !*json && results.trim().is_empty() {
                        println!("No function found for '{}'", symbol);
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
                                if let Ok(meta) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                                {
                                    let hash =
                                        path.file_name().unwrap_or_default().to_string_lossy();
                                    let graph_file = path.join("graph.bin");
                                    let functions = if graph_file.exists() {
                                        let s = graph_file.metadata().map(|m| m.len()).unwrap_or(0);
                                        format!("{}", s)
                                    } else {
                                        "—".to_string()
                                    };
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
                    println!(
                        "  {}  →  {}",
                        p["hash"]
                            .as_str()
                            .unwrap_or("?")
                            .chars()
                            .take(12)
                            .collect::<String>(),
                        p["project_root"].as_str().unwrap_or("?")
                    );
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
        Commands::Skeleton { file_paths, json } => match execute_skeleton(file_paths, *json) {
            Ok(output) => print!("{}", output),
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Snippet {
            function_name,
            file_path,
            json,
        } => match execute_snippet(function_name, file_path.as_deref(), *json) {
            Ok(output) => print!("{}", output),
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        },
        Commands::InstallHooks => {
            let project_root = detect_project()?;
            let git_dir = project_root.join(".git");
            if !git_dir.exists() {
                return Err("Not a git repository.".into());
            }

            let hooks_dir = git_dir.join("hooks");
            std::fs::create_dir_all(&hooks_dir)?;

            let hook_script = "#!/bin/sh\n# CodeSeek auto-index hook (async, non-blocking)\nnohup codeseek init </dev/null >/dev/null 2>&1 &\n";

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
        Commands::Knowledge { action } => {
            codeseek::knowledge::cli::handle_knowledge_command(action.clone()).await?;
        }
    }

    Ok(())
}

/// Phase P2b 交付物 3：给 `callers` / `callees` 的**文本**输出追加解析层级标注。
///
/// 选择"最小侵入"的方式：**只在 `resolution_level > 0` 时**追加一段后缀。
/// legacy 模式下四个新标量恒为 0 ⇒ 后缀为空 ⇒ 既有的文本输出逐字节不变
/// （红线 4：不改既有 JSON 字段名，也不破坏任何已有的文本断言）。
fn resolution_annotation(relation: &codeseek::codegraph::types::CallRelation) -> String {
    if relation.resolution_level == 0 {
        return String::new();
    }
    format!(
        "  [L{} c{} n{}{}]",
        relation.resolution_level,
        relation.confidence,
        relation.candidate_count,
        if relation.candidate_count >= 2 {
            " ambiguous"
        } else {
            ""
        }
    )
}

/// Phase P2b 交付物 3：给 JSON 输出**追加**解析元信息字段（既有键名与顺序一律不变）。
fn resolution_json_fields(
    relation: &codeseek::codegraph::types::CallRelation,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(
        "resolution_level".into(),
        serde_json::json!(relation.resolution_level),
    );
    map.insert("confidence".into(), serde_json::json!(relation.confidence));
    map.insert(
        "candidate_count".into(),
        serde_json::json!(relation.candidate_count),
    );
    map.insert(
        "ambiguous".into(),
        serde_json::json!(relation.candidate_count >= 2),
    );
    map
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
                let mut entry = serde_json::json!({
                    "caller": caller.name,
                    "caller_file": caller.file_path,
                    "caller_line": relation.line_number,
                    "target": func.name,
                });
                // Phase P2b：追加解析元信息（既有键名不变）。
                if let Some(obj) = entry.as_object_mut() {
                    obj.extend(resolution_json_fields(relation));
                }
                results.push(entry);
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
                        "  {} ({}:{}){}\n",
                        caller.name,
                        caller.file_path.display(),
                        relation.line_number,
                        resolution_annotation(relation)
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
                let mut entry = serde_json::json!({
                    "callee": callee.name,
                    "callee_file": callee.file_path,
                    "callee_line": relation.line_number,
                    "caller": func.name,
                });
                // Phase P2b：追加解析元信息（既有键名不变）。
                if let Some(obj) = entry.as_object_mut() {
                    obj.extend(resolution_json_fields(relation));
                }
                results.push(entry);
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
                        "  {} ({}:{}){}\n",
                        callee.name,
                        callee.file_path.display(),
                        relation.line_number,
                        resolution_annotation(relation)
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

// Callgraph query function delegated to services module
fn execute_callgraph(
    graph: &PetCodeGraph,
    symbol: &str,
    depth: u32,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    codeseek::services::execute_callgraph(graph, symbol, depth, json)
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
    dirs::home_dir()
        .unwrap_or_default()
        .join(".codex")
        .join("config.toml")
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
        (
            claude_global_mcp_path(),
            claude_settings_path(false),
            "global",
        )
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
        if config
            .get("mcpServers")
            .and_then(|s| s.get("codeseek"))
            .is_some()
        {
            config["mcpServers"]
                .as_object_mut()
                .map(|s| s.remove("codeseek"));
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

/// 判断符号类型是否应该作为顶层骨架条目显示
fn is_skeleton_worthy(symbol_type: &SymbolType) -> bool {
    matches!(
        symbol_type,
        SymbolType::StructDeclaration
            | SymbolType::FunctionDeclaration
            | SymbolType::TypeAlias
            | SymbolType::Module
    )
}

/// 对单个文件使用 AST 解析和 SkeletonFormatter 生成代码骨架
fn generate_skeleton_for_file(file_path_str: &str) -> Result<String, String> {
    let file_path = PathBuf::from(file_path_str);

    // 1. 验证文件存在
    if !file_path.exists() {
        return Err(format!("File not found: {}", file_path_str));
    }

    // 2. 读取源代码
    let code = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("Cannot read file {}: {}", file_path_str, e))?;

    // 3. 获取语言特定 AST 解析器
    let (mut parser, language_id) = get_ast_parser_by_filename(&file_path)
        .map_err(|e| format!("Skeleton not supported for {}: {}", file_path_str, e))?;

    // 4. 解析文件
    let symbols = parser.parse(&code, &file_path);
    if symbols.is_empty() {
        return Ok(format!("// No symbols found in {}", file_path_str));
    }

    // 5. 构建 SymbolInformation 列表和 guid_to_children 映射
    let mut symbols_info: Vec<SymbolInformation> = Vec::with_capacity(symbols.len());
    let mut guid_to_children: HashMap<Uuid, Vec<Uuid>> = HashMap::new();

    for s in &symbols {
        let guard = s.read();
        let info = guard.symbol_info_struct();
        let guid = guard.guid().clone();
        let children = guard.childs_guid().clone();
        guid_to_children.insert(guid, children);
        symbols_info.push(info);
    }

    // 6. 构建 guid_to_info 引用映射
    let guid_to_info: HashMap<Uuid, &SymbolInformation> =
        symbols_info.iter().map(|s| (s.guid.clone(), s)).collect();

    // 7. 识别顶层符号（其 GUID 不是任何其他符号的子节点）
    let all_children_guids: HashSet<Uuid> = guid_to_children
        .values()
        .flat_map(|children| children.iter())
        .cloned()
        .collect();

    let mut top_level_symbols: Vec<&SymbolInformation> = symbols_info
        .iter()
        .filter(|s| !all_children_guids.contains(&s.guid))
        .filter(|s| is_skeleton_worthy(&s.symbol_type))
        .collect();

    // 按行号排序，保持稳定的输出顺序
    top_level_symbols.sort_by_key(|s| s.full_range.start_point.row);

    if top_level_symbols.is_empty() {
        return Ok(format!(
            "// No structural symbols found in {}",
            file_path_str
        ));
    }

    // 8. 对每个顶层符号生成骨架
    let formatter = make_formatter(&language_id);
    let skeleton_parts: Vec<String> = top_level_symbols
        .iter()
        .map(|symbol| formatter.make_skeleton(symbol, &code, &guid_to_children, &guid_to_info))
        .collect();

    Ok(skeleton_parts.join("\n\n"))
}

// ── Skeleton command ──────────────────────────────────────────────────
fn execute_skeleton(
    file_paths: &[String],
    _json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut all_skeletons: Vec<serde_json::Value> = Vec::new();

    for file_path_str in file_paths {
        let input_path = PathBuf::from(file_path_str);
        let language = detect_language(&input_path);

        let skeleton_text = match generate_skeleton_for_file(file_path_str) {
            Ok(text) => text,
            Err(e) => format!("// Error: {}", e),
        };

        all_skeletons.push(serde_json::json!({
            "filepath": file_path_str,
            "language": language,
            "skeleton_text": skeleton_text,
        }));
    }

    let result = serde_json::json!({
        "success": true,
        "data": {
            "skeletons": all_skeletons
        }
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

// ── Snippet command ──────────────────────────────────────────────────
fn execute_snippet(
    function_name: &str,
    file_path: Option<&str>,
    _json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let project_root = detect_project()?;
    let (_index_dir, lock_path) = project_paths(&project_root);
    let _lock = FileLock::shared(lock_path)?;

    let project_hash = Config::compute_project_hash(&project_root);
    let storage = Arc::new(StorageManager::new());

    let graph = match storage.get_persistence().load_graph(&project_hash) {
        Ok(Some(g)) => g,
        _ => {
            let result = serde_json::json!({
                "success": false,
                "error": "No index found. Run 'codeseek init' first."
            });
            return Ok(serde_json::to_string_pretty(&result)?);
        }
    };

    // Find functions by name
    let mut candidates: Vec<&FunctionInfo> = graph.find_functions_by_name(function_name);

    // Filter by file path if provided
    if let Some(fp) = file_path {
        let fp_path = PathBuf::from(fp);
        candidates.retain(|f| {
            f.file_path == fp_path
                || f.file_path.ends_with(&fp_path)
                || fp_path.ends_with(&f.file_path)
        });
    }

    if candidates.is_empty() {
        let result = serde_json::json!({
            "success": false,
            "error": format!("Function '{}' not found{}", function_name,
                file_path.map(|fp| format!(" in file '{}'", fp)).unwrap_or_default())
        });
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    if candidates.len() > 1 && file_path.is_none() {
        let locations: Vec<String> = candidates
            .iter()
            .map(|f| {
                format!(
                    "  {}:{} (line {})",
                    f.file_path.display(),
                    f.name,
                    f.line_start
                )
            })
            .collect();
        let result = serde_json::json!({
            "success": false,
            "error": format!("Multiple functions named '{}' found. Specify --file-path to disambiguate:\n{}",
                function_name, locations.join("\n"))
        });
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    let func = candidates[0];

    // Resolve source file path on disk
    let source_path = resolve_source_path(&func.file_path, &project_root);

    // Read snippet from file
    let code_snippet = read_line_range(&source_path, func.line_start, func.line_end)
        .map_err(|e| format!("Failed to read function source: {}", e))?;

    let language = detect_language(&func.file_path);

    let result = serde_json::json!({
        "success": true,
        "data": {
            "filepath": func.file_path.to_string_lossy(),
            "function_name": func.name,
            "code_snippet": code_snippet,
            "line_start": func.line_start,
            "line_end": func.line_end,
            "language": language
        }
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

// ── Utility functions ────────────────────────────────────────────────

/// Resolve the actual file path on disk from a FunctionInfo's file_path.
fn resolve_source_path(file_path: &Path, root: &Path) -> PathBuf {
    if file_path.is_absolute() && file_path.exists() {
        file_path.to_path_buf()
    } else {
        let rooted = root.join(file_path);
        if rooted.exists() {
            rooted
        } else {
            file_path.to_path_buf()
        }
    }
}

/// Detect programming language from file extension.
fn detect_language(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "swift" => "swift",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        _ => "unknown",
    }
    .to_string()
}

/// Read a line range [start, end] (1-indexed, inclusive) from a file.
fn read_line_range(file_path: &Path, line_start: usize, line_end: usize) -> Result<String, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;
    let lines: Vec<&str> = content.lines().collect();
    let start_idx = line_start.saturating_sub(1); // Convert to 0-indexed
    let end_idx = line_end.min(lines.len());
    if start_idx >= lines.len() {
        return Err(format!(
            "Line range [{}, {}] out of bounds (file has {} lines)",
            line_start,
            line_end,
            lines.len()
        ));
    }
    Ok(lines[start_idx..end_idx].join("\n"))
}
