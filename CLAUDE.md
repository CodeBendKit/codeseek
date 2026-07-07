# CLAUDE.md

## Build & Test

```bash
# Enter Rust project directory
cd rust-core

# Build
cargo build

# Run tests
cargo test

# Run specific test
cargo test test_build_graph_functionality

# Run with output
cargo test -- --nocapture

# Start server (single repo per process, required)
cargo run -- server --repo-path /path/to/repo

# Start server with custom address
cargo run -- server --repo-path /path/to/repo --address 0.0.0.0:8080

# CLI vectorize mode
cargo run -- vectorize --path /path/to/code --collection my-collection --db-uri data/lancedb
```

## Architecture

```
src/
├── main.rs              # CLI entry: server and vectorize subcommands
├── lib.rs               # Top-level module re-exports
├── config.rs            # Config loading from ~/.codeseek/config/config.toml
├── cli/                 # CLI args (clap), runner, analyze, vectorize commands
├── codegraph/           # AST parsing + graph data structures
│   ├── graph.rs         # Flat CodeGraph (HashMap-based)
│   ├── types.rs         # PetCodeGraph, EntityGraph, FileIndex, SnippetIndex
│   ├── parser.rs        # CodeParser: tree-sitter parsing with FileIndex/SnippetIndex
│   └── treesitter/      # Language parsers (Rust/Python/JS/TS/Java/C++/Go)
├── services/            # High-level analysis services
│   ├── analyzer.rs      # CodeAnalyzer: call chains, cycles, complexity, reports
│   ├── embedding_service.rs  # LanceDB vector embeddings + SQLite cache + semantic search
│   └── snippet_service.rs    # Code snippet extraction + caching
├── storage/             # Graph persistence and file watching
│   ├── persistence.rs   # Save/load graphs (JSON + binary), project registry
│   ├── petgraph_storage.rs  # PetCodeGraph serde (JSON, bincode, GraphML, GEXF)
│   ├── incremental.rs   # MD5-based incremental file change detection
│   └── mod.rs           # StorageManager: central hub (graph, watchers, tasks, config, current_repo)
├── http/                # Axum HTTP server
│   ├── server.rs        # CodeSeekServer: startup init + router
│   ├── handlers/        # Request handlers (query, search, investigate, embed)
│   └── models/          # Request/response types + ApiResponse<T>
└── config/              # Config file template
```

## Core Design: Single Repo Per Process

The process binds to exactly one repository at startup via `--repo-path`. `StorageManager.current_repo` tracks this. `try_bind_repo()` rejects attempts to switch repos. The HTTP API no longer accepts `repo_path`/`project_dir` parameters — all endpoints use the bound repo.

## Key Types

- **`StorageManager`** — Central state: in-memory graph, persistence, file watchers, embedding tasks, config, current repo binding
- **`PetCodeGraph`** — petgraph `DiGraph<FunctionInfo, CallRelation>` with query methods (`get_callers`, `get_callees`, `find_functions_by_name`, `find_functions_by_file`)
- **`CodeAnalyzer`** — Wraps `CodeParser`, runs full directory analysis, provides analysis methods
- **`CodeSeekServer`** — Axum server; `start()` auto-inits (load/analyze graph + embed + watch) before binding port
- **`EmbeddingService`** — LanceDB-backed; per-repo tables named `{last_dir}_{md5(repo_path)}`; batch embedding with SQLite cache; incremental via `projects.json` file hashes
- **`PersistenceManager`** — File-based graph persistence in `.codegraph_db/{project_id}/`

## HTTP API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| GET | `/status` | Current repo status (path, functions, files, embedding state) |
| POST | `/query_call_graph` | Query call graph by file/function name |
| POST | `/query_code_snippet` | Extract code snippet with line ranges |
| POST | `/query_code_skeleton` | Batch skeleton extraction from file paths |
| POST | `/query_hierarchical_graph` | Hierarchical call tree with depth limit |
| POST | `/investigate_repo` | Top-15 functions by out-degree, directory tree, file skeletons |
| POST | `/semantic_search` | Vector-based semantic code search (uses bound repo) |
| POST | `/query_indexing_status` | Embedding indexing status (uses bound repo) |
| GET | `/` and `/draw_call_graph` | ECharts call graph visualization |

All handlers use `State<Arc<StorageManager>>` for shared state. Response type is `Json<ApiResponse<T>>` where `ApiResponse` has `success: bool` and `data: T`.

## Configuration

Config file at `~/.codeseek/config/config.toml`. Key sections:

- `[http]` — `server_port`, `codebase_port` (default 12800)
- `[codebase]` — `enable_embedding`, `embedding_db_uri`, `graph_db_uri`
- `[codebase.embedding]` — `model`, `api_token`, `api_base_url`, `dimensions`

## File Watching

`setup_watcher()` uses `notify` crate with 20-second debounce. On file changes, re-runs `perform_analysis` and triggers embedding rebuild. Watchers tracked in `StorageManager.watchers: HashMap<String, RecommendedWatcher>` keyed by project_id (MD5 of repo path).

## Important Notes

- `perform_analysis` runs in `tokio::task::spawn_blocking` (CPU-heavy tree-sitter parsing)
- Embedding builds run in background `tokio::spawn` tasks; `vector_tasks: HashSet<String>` prevents duplicate builds
- Project IDs are `format!("{:x}", md5::compute(repo_path))`
- LanceDB collection names: `format!("{}_{:x}", last_dir_name, md5(repo_path))`
- Tree-sitter is linked as a system library (see `build.rs`)
- Storage modes: `Json`, `Binary`, `Both` — configured via `--storage-mode` CLI flag

<!-- CODESEEK_INJECTION -->
# Code exploration: use CodeSeek MCP tools first

Before any Grep/Glob/Bash for code search, try CodeSeek tools first.
They give you AST-verified definitions with signatures and line numbers.

Tool priority (use in this order):
1. codeseek_search("query")      — FIRST for finding code by name or behavior
2. codeseek_callers("fn")        — REQUIRED before modifying any function
3. codeseek_callees("fn")        — to understand internal dependencies
4. codeseek_callgraph("fn", 2)   — to explore full calling context with depth
5. Grep — ONLY for exact strings (error messages, UUIDs, log formats)
6. Glob — ONLY when you already know the exact filename pattern
<!-- /CODESEEK_INJECTION -->
