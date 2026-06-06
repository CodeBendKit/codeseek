# CodeSeek

**面向 Claude Code 的代码智能 CLI 工具。** 基于 AST 的调用图分析 + 语义搜索 — 从终端直接使用。

## 快速开始

```bash
# npm 安装（自动处理配置向导 + 二进制下载）
npm install -g codeseek

# 首次运行 — 交互式配置向导，填写嵌入模型信息
codeseek

# 构建索引
codeseek init

# 按符号名称搜索
codeseek search main --limit 10

# 查询调用关系
codeseek callers main
codeseek callees process_data

# 注册为 Claude Code / Codex MCP 工具
codeseek install

# 查看状态
codeseek status

# 安装 git hooks（提交时自动增量索引）
codeseek install-hooks
```

## 安装

### npm

```bash
npm install -g codeseek
```

npm 包包含一个轻量的 JS wrapper，负责：

| 步骤 | 说明 |
|------|------|
| **首次配置向导** | 交互式 CLI 引导填写 API token、模型名称、API 地址 |
| **二进制下载** | 自动从 GitHub Releases 拉取对应平台的 Rust 二进制 |
| **命令透传** | 所有命令（`init`、`search`、`callers` 等）转发给原生二进制 |

支持的平台：

| 平台 | 架构 |
|------|------|
| macOS | arm64（Apple Silicon）、x64（Intel） |
| Linux | x64 |
| Windows | x64 |

### Homebrew（即将推出）

```bash
brew install CodeBendKit/tap/codeseek
```

### 从源码编译

```bash
git clone https://github.com/CodeBendKit/codeseek.git
cd codeseek
./build.sh --release
```

`build.sh` 会同时编译 TypeScript wrapper（`dist/`）和 Rust 二进制，然后安装到 `~/.codeseek/bin/`。

## 命令列表

| 命令 | 说明 |
|------|------|
| `codeseek` | 首次配置向导（交互式配置嵌入模型） |
| `codeseek init` | 构建/更新代码索引（首次全量，后续 MD5 增量） |
| `codeseek status` | 索引统计：函数数、文件数、最后更新时间 |
| `codeseek search <关键词>` | 符号名称搜索（向量 → 图谱名称匹配自动回退） |
| `codeseek callers <符号>` | 查询调用该符号的函数 |
| `codeseek callees <符号>` | 查询该符号调用的函数 |
| `codeseek list` | 列出所有已索引的项目及路径 |
| `codeseek install` | 注册 codeseek 为 Claude Code / Codex 的 MCP 工具 |
| `codeseek uninstall` | 移除 MCP 集成 |
| `codeseek uninit` | 删除当前项目的索引数据 |
| `codeseek install-hooks` | 安装 git hooks（提交/合并时自动 `codeseek init`） |
| `codeseek serve --mcp` | 启动 MCP 服务器（stdio JSON-RPC，由 Claude Code 内部调用） |

所有查询命令支持 `--json` 输出机器可读格式。

## Claude Code / Codex 集成

```bash
codeseek install
```

自动写入 MCP 配置到：

| Agent | 配置文件 |
|-------|---------|
| **Claude Code** | `~/.claude.json`（全局，所有项目）或 `./.mcp.json`（项目局部） |
| **Codex CLI** | `~/.codex/config.toml` |

重启后 Claude Code 自动识别以下工具：

| 工具名 | 功能 |
|--------|------|
| `codeseek_search` | 按名称查找符号 |
| `codeseek_callers` | 追踪上游调用者 |
| `codeseek_callees` | 追踪下游被调用者 |
| `codeseek_status` | 检查索引健康状态 |

移除集成：

```bash
codeseek uninstall
```

## 工作原理

```
codeseek search "auth middleware"
  → 自动检测项目根目录（向上找 .git/）
  → 从 ~/.codeseek/<project_hash>/ 加载索引
  → 基于图谱的名称搜索（PetCodeGraph）
  → 输出结果
```

无守护进程、无 HTTP 服务器。每个命令都是独立的单次进程。

### 存储

- **配置**：`~/.codeseek/config.json`（全局，所有项目共享）
- **索引**：`~/.codeseek/<md5(project_root)>/`
  - `project.json` — 项目元数据（根路径、索引时间）
  - `graph.bin` — 序列化调用图（PetCodeGraph）
  - `embeddings.lance/` — LanceDB 向量数据（可选，需要 API token）
  - `tantivy_bm25/` — BM25 全文索引（可选）
  - `file_hashes.json` — MD5 增量追踪

### 增量更新

`codeseek init` 是幂等的：
- 首次运行：全量 AST 解析 → 构建图谱 → 保存
- 后续运行：MD5 对比 → 仅处理变更文件 → 与已有图谱合并

```bash
# 安装 git hooks，每次 commit/merge 自动增量索引
codeseek install-hooks
```

## 支持的语言

| 语言 | 函数 | 结构体/类 | 调用图 |
|------|:---:|:---:|:---:|
| Rust | ✅ | ✅ | ✅ |
| Python | ✅ | ✅ | ✅ |
| JavaScript | ✅ | ✅ | ✅ |
| TypeScript | ✅ | ✅ | ✅ |
| Go | ✅ | ✅ | ✅ |
| C/C++ | ✅ | ✅ | ✅ |
| Java | ✅ | ✅ | ✅ |

## 配置

`~/.codeseek/config.json`：

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

首次运行交互式向导自动填写，也可以手动创建。

## 开发

```bash
cd rust-core

# 编译
cargo build

# 编译 + 安装到 ~/.codeseek/bin/
cd .. && ./build.sh --release

# 运行测试
cargo test

# 编译 TypeScript wrapper
npm run build
```

## 许可证

MIT

构建于：Tree-sitter · Petgraph · LanceDB · Tantivy · Tokio · Clap
