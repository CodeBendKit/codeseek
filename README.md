# CodeSeek

**Code intelligence CLI tool for Claude Code.** AST-based call graph analysis + semantic vector search — right from your terminal.

## Quick Start

```bash
# Install via npm
npm install -g codeseek

# First run — configures embedding model interactively
codeseek

# Index your project
codeseek init

# Search with natural language
codeseek search "handle HTTP request" --limit 10

# Query call graph
codeseek callers main
codeseek callees process_data

# Check status
codeseek status

# Auto-index on git commits
codeseek install-hooks

# Clean up
codeseek uninit
```

## Install

### npm

```bash
npm install -g codeseek
```

The npm package includes a JS wrapper that handles:
- Interactive setup wizard (configures the embedding model)
- Automatic platform binary download
- Command pass-through to the Rust binary

### Homebrew (coming soon)

```bash
brew install wenwang/tap/codeseek
```

### From source

```bash
git clone https://github.com/wenwang/codeseek
cd codeseek
cd rust-core && cargo build --release
```

## Commands

| Command | Description |
|---------|-------------|
| `codeseek` | First-time setup wizard (configures embedding model) |
| `codeseek init` | Build/update code index (full on first run, incremental thereafter) |
| `codeseek status` | Index statistics: functions, files, last update |
| `codeseek search <query>` | Semantic search (vector + BM25 + RRF fusion) |
| `codeseek callers <symbol>` | Find functions that call this symbol |
| `codeseek callees <symbol>` | Find functions this symbol calls |
| `codeseek uninit` | Delete the project index |
| `codeseek install-hooks` | Install git hooks (post-commit/post-merge auto-index) |

All search/query commands support `--json` for machine-readable output.

## How It Works

```
codeseek search "auth middleware"
  → Load index from ~/.codeseek/<project_hash>/
  → Dense search (LanceDB vector ANN)
  → Sparse search (Tantivy BM25)
  → RRF fusion ranking
  → Output results
```

No daemon, no HTTP server. Every command is a standalone process that reads from disk.

### Storage

- **Config**: `~/.codeseek/config.json` (global, shared across all projects)
- **Index**: `~/.codeseek/<md5(project_root)>/`
  - `graph.bin` — Serialized call graph (PetCodeGraph)
  - `embeddings.lance/` — LanceDB vector data
  - `tantivy_bm25/` — BM25 full-text index
  - `file_hashes.json` — MD5 incremental tracking

### Incremental Updates

`codeseek init` is idempotent:
- First run: Full AST parse → embedding → index
- Subsequent runs: MD5 comparison → only re-process changed files

Install git hooks (`codeseek install-hooks`) for automatic updates on commit/merge.

## Supported Languages

| Language | Functions | Structs/Classes | Call Graph |
|----------|:---------:|:---------------:|:----------:|
| Rust     | ✅ | ✅ | ✅ |
| Python   | ✅ | ✅ | ✅ |
| JavaScript | ✅ | ✅ | ✅ |
| TypeScript | ✅ | ✅ | ✅ |
| Go       | ✅ | ✅ | ✅ |
| C/C++    | ✅ | ✅ | ✅ |
| Java     | ✅ | ✅ | ✅ |

## Configuration

`~/.codeseek/config.json`:

```json
{
  "embedding": {
    "provider": "openai-compatible",
    "model": "Qwen/Qwen3-Embedding-4B",
    "api_token": "sk-...",
    "api_base_url": "https://api.siliconflow.cn/v1",
    "dimensions": 2560
  },
  "index": {
    "min_code_block_length": 16,
    "enable_reranker": false
  },
  "installed_hooks": {}
}
```

## Development

```bash
cd rust-core

# Build
cargo build

# Run tests
cargo test

# Run specific test
cargo test test_build_graph_functionality
```

## License

MIT

Built with: Tree-sitter · Petgraph · LanceDB · Tantivy · Axum · Tokio · Clap
