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
| Windows | x64 |

### Homebrew (coming soon)

```bash
brew install CodeBendKit/tap/codeseek
```

### From source

```bash
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

```
codeseek search "auth middleware"
  → Detect project root (walk up to .git/)
  → Load index from ~/.codeseek/<project_hash>/
  → Graph-based name search (PetCodeGraph)
  → Output results
```

No daemon, no HTTP server. Every command is a standalone process.

### Storage

- **Config**: `~/.codeseek/config.json` (global, shared across all projects)
- **Index**: `~/.codeseek/<md5(project_root)>/`
  - `project.json` — Project metadata (root path, indexed timestamp)
  - `graph.bin` — Serialized call graph (PetCodeGraph)
  - `embeddings.lance/` — LanceDB vector data (optional, requires API token)
  - `tantivy_bm25/` — BM25 full-text index (optional)
  - `file_hashes.json` — MD5 incremental tracking

### Incremental Updates

`codeseek init` is idempotent:
- First run: Full AST parse → build graph → save
- Subsequent runs: MD5 comparison → only re-process changed files → merge with existing graph

```bash
# Install git hooks for automatic re-index on commit/merge
codeseek install-hooks
```

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
    "enable_reranker": false
  },
  "installed_hooks": {}
}
```

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
