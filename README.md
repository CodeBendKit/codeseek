# CodeSeek

**Code intelligence CLI tool for Claude Code.** AST-based call graph analysis + semantic search — right from your terminal.

## Quick Start

```bash
# Install via npm (handles setup wizard + binary download automatically)
npm install -g codeseek

# First run — interactive setup wizard configures your embedding model
codeseek

# Index your project
codeseek init

# Search code by symbol name
codeseek search main --limit 10

# Query call graph
codeseek callers main
codeseek callees process_data

# Register with Claude Code / Codex as MCP tools
codeseek install

# Check status
codeseek status

# Auto-index on git commits
codeseek install-hooks
```

Natural Language Code Search example

```bash
╰─$ codeseek search 'how the code embedding work'
1. get_embedding (0.7973)
   /home/do/ssd/iohub/dev/codeseek/rust-core/src/services/embedding_service.rs:0
2. EmbeddingService (0.2855)
   /home/do/ssd/iohub/dev/codeseek/rust-core/src/services/embedding_service.rs:0
3. EmbeddingData (0.1449)
   /home/do/ssd/iohub/dev/codeseek/rust-core/src/services/embedding_service.rs:0
4. EmbeddingResponse (0.1304)
   /home/do/ssd/iohub/dev/codeseek/rust-core/src/services/embedding_service.rs:0
5. default_model (0.0450)
   /home/do/ssd/iohub/dev/codeseek/rust-core/src/config.rs:0

```

## Install

### npm

```bash
npm install -g codeseek
```

The npm package ships a lightweight JS wrapper that handles:

| Step | Description |
|------|-------------|
| **First-run wizard** | Interactive CLI prompts for embedding API token, model, and base URL |
| **Binary download** | Automatically pulls the correct Rust binary for your platform from GitHub Releases |
| **Pass-through** | All commands (`init`, `search`, `callers`, etc.) are forwarded to the native binary |

Supported platforms:

| Platform | Architecture |
|----------|-------------|
| macOS | arm64 (Apple Silicon), x64 (Intel) |
| Linux | x64 |

### Homebrew

```bash
brew tap CodeBendKit/codeseek git@github.com:CodeBendKit/codeseek.git
brew install codeseek
```

### From source

```bash
# install protoc
# macos: brew install protobuf
# ubuntu: sudo apt install protoc

git clone https://github.com/CodeBendKit/codeseek.git
cd codeseek
./build.sh --release
```

`build.sh` compiles both the TypeScript wrapper (`dist/`) and the Rust binary, then installs to `~/.codeseek/bin/`.

## Commands

| Command | Description |
|---------|-------------|
| `codeseek` | First-time setup wizard (configures embedding model interactively) |
| `codeseek init` | Build/update code index (full on first run, MD5-incremental thereafter) |
| `codeseek status` | Index statistics: functions, files, last update |
| `codeseek search <query>` | Symbol name search (falls back from vector → graph name match) |
| `codeseek callers <symbol>` | Find functions that call this symbol |
| `codeseek callees <symbol>` | Find functions this symbol calls |
| `codeseek list` | List all indexed projects with paths |
| `codeseek install` | Register codeseek as MCP tools in Claude Code / Codex |
| `codeseek uninstall` | Remove MCP integration |
| `codeseek uninit` | Delete the current project index |
| `codeseek install-hooks` | Install git hooks (post-commit/post-merge → `codeseek init`) |
| `codeseek serve --mcp` | Start MCP server (stdio JSON-RPC, used by Claude Code internally) |

All query commands support `--json` for machine-readable output.

## Claude Code / Codex Integration

```bash
codeseek install
```

Writes MCP server config to:

| Agent | Config file |
|-------|------------|
| **Claude Code** | `~/.claude.json` (global, all projects) or `./.mcp.json` (project-local) |
| **Codex CLI** | `~/.codex/config.toml` |

Claude Code auto-discovers these tools after restart:

| Tool | Capability |
|------|-----------|
| `codeseek_search` | Find symbols by name |
| `codeseek_callers` | Trace upstream callers |
| `codeseek_callees` | Trace downstream callees |
| `codeseek_status` | Check index health |

Remove integration:

```bash
codeseek uninstall
```

## How It Works

### Index Building (`codeseek init`)

```
Source files
  → Tree-sitter AST parse (7 languages)
  → Extract functions / classes / methods
  → Batch embed via API (20 texts per call, SQLite cache)
  → Store vectors in LanceDB
  → Build BM25 index in Tantivy
  → Serialize call graph (PetCodeGraph)
  → Save to ~/.codeseek/<project_hash>/
```

**Idempotent**: first run is full build, subsequent runs compare MD5 hashes — only changed files are re-processed. Use `codeseek install-hooks` for automatic re-index on git commit/merge.

### Hybrid Search Pipeline (`codeseek search`)

```
                        ┌─────────────────────┐
User query ────────────→│  Embedding Model    │──→ Query vector
                        └─────────────────────┘
                                  │
          ┌───────────────────────┼───────────────────────┐
          ▼                       ▼                       ▼
   ┌──────────────┐      ┌───────────────┐       ┌───────────────┐
   │ Dense Search │      │ Sparse Search │       │ Graph Search  │
   │ (LanceDB ANN)│      │ (Tantivy BM25)│       │ (PetCodeGraph)│
   └──────┬───────┘      └──────┬────────┘       └────────┬──────┘
          │                      │                        │
          └──────────────────────┼────────────────────────┘
                                 ▼
                        ┌─────────────────┐
                        │   RRF Fusion    │  ← Reciprocal Rank Fusion
                        │  (Top-20 candidates)│
                        └────────┬────────┘
                                 │
                                 ▼
                        ┌─────────────────┐
                        │    Reranker     │  ← Cross-Encoder fine re-ranking
                        │ (Qwen3-Reranker)│     scores each (query, code) pair
                        └────────┬────────┘
                                 │
                                 ▼
                        ┌─────────────────┐
                        │   Final Results  │  ← Top-5 (or Top-N)
                        └─────────────────┘
```

| Stage | Technology | Role | Speed |
|-------|-----------|------|:-----:|
| **Dense Search** | LanceDB + Embedding Model | Semantic vector similarity | Fast |
| **Sparse Search** | Tantivy BM25 | Keyword & token matching | Fast |
| **RRF Fusion** | Reciprocal Rank Fusion | Merge heterogeneous scores fairly | Instant |
| **Reranker** | Cross-Encoder (Qwen3-Reranker-4B) | Full-interaction precision scoring | ~1-2s |
| **Fallback** | PetCodeGraph | Graph-based name search (no API needed) | Instant |

If embedding/Reranker are unavailable, the pipeline falls back gracefully to graph-based name search.

### Storage

- **Config**: `~/.codeseek/config.json` (global, shared across all projects)
- **Index**: `~/.codeseek/<md5(project_root)>/`
  - `project.json` — Project metadata
  - `graph.bin` — Serialized call graph
  - `embeddings.lance/` — LanceDB vector data
  - `tantivy_bm25/` — BM25 full-text index
  - `file_hashes.json` — MD5 incremental tracking

No daemon, no HTTP server. Every command is a standalone process.

## Supported Languages

| Language | Functions | Structs/Classes | Call Graph |
|----------|:---------:|:---------------:|:----------:|
| Rust | ✅ | ✅ | ✅ |
| Python | ✅ | ✅ | ✅ |
| JavaScript | ✅ | ✅ | ✅ |
| TypeScript | ✅ | ✅ | ✅ |
| Go | ✅ | ✅ | ✅ |
| C/C++ | ✅ | ✅ | ✅ |
| Java | ✅ | ✅ | ✅ |

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
    "enable_reranker": true,
    "hybrid": {
      "enable_bm25": true,
      "bm25_top_k": 20,
      "vector_top_k": 20,
      "rrf_k": 60,
      "rrf_top_k": 20
    },
    "reranker": {
      "enabled": true,
      "model": "Qwen/Qwen3-Reranker-4B",
      "api_token": "sk-...",
      "api_base_url": "https://api.siliconflow.cn/v1/rerank",
      "top_n": 5,
      "candidate_multiplier": 5,
      "timeout_secs": 60
    }
  },
  "installed_hooks": {}
}
```

### Model Roles

| Model | Role | When |
|-------|------|------|
| `Qwen/Qwen3-Embedding-4B` | Converts code → vectors for dense search | Index building |
| `Qwen/Qwen3-Reranker-4B` | Scores (query, code) pairs for precision | Search time |

Set via the interactive wizard on first run, or create manually.

## Development

```bash
cd rust-core

# Build
cargo build

# Build + install to ~/.codeseek/bin/
cd .. && ./build.sh --release

# Run tests
cargo test

# Compile TypeScript wrapper
npm run build
```

## License

MIT

Built with: Tree-sitter · Petgraph · LanceDB · Tantivy · Tokio · Clap
