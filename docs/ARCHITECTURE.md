# CodeSeek 架构与功能说明

## 概述

CodeSeek 是一个 Rust 实现的多语言代码分析引擎，提供源码 AST 解析、函数调用图谱构建、代码向量嵌入索引和语义搜索能力。系统以 HTTP 服务形式运行，启动时绑定单个仓库，支持 6 种主流编程语言。

## 核心设计原则

### 单进程单仓库

每个进程在启动时通过 `--repo-path` 参数绑定一个代码仓库：

```
cargo run -- server --repo-path /path/to/repo
```

进程生命周期内不可切换仓库。`StorageManager.current_repo` 跟踪绑定状态，`try_bind_repo()` 在启动时被调用一次，拒绝重复绑定。所有 HTTP API 端点无需在请求中携带仓库路径——直接使用已绑定的仓库。

**设计动机**：简化 API、避免并发冲突、降低实现复杂度。多仓库场景通过启动多个进程实例解决。

### 启动自初始化

`CodeSeekServer::start()` 方法在绑定端口前执行完整的初始化流程：

1. 验证 `--repo-path` 路径是否存在
2. 绑定进程到该仓库 (`try_bind_repo`)
3. 尝试从 `.codegraph_db/{project_id}/` 加载已缓存的代码图谱
4. 若缓存不存在，执行全量 AST 分析生成图谱
5. 触发后台嵌入索引构建（embeddings）
6. 启动文件监听器（inotify/FSEvents，20 秒防抖）

初始化完成后才启动 HTTP 监听。

## 模块架构

```
                       ┌──────────────────────────┐
                       │       main.rs (CLI)       │
                       │  server / vectorize 子命令 │
                       └─────┬──────────────┬──────┘
                             │              │
              ┌──────────────▼──┐   ┌──────▼──────────────┐
              │  http/server.rs  │   │    cli/runner.rs     │
              │  CodeSeekServer  │   │  vectorize 命令行模式 │
              └──────┬───────────┘   └─────────────────────┘
                     │
         ┌───────────┼───────────┐
         ▼           ▼           ▼
  ┌──────────┐ ┌──────────┐ ┌──────────┐
  │ handlers │ │  models  │ │ templates│
  │ 请求处理  │ │ 请求/响应 │ │  HTML    │
  └────┬─────┘ └──────────┘ └──────────┘
       │
       ▼
  ┌─────────────────────────────────────┐
  │          StorageManager             │
  │  中心状态：图谱 / 监听器 / 任务 / 配置 │
  └──┬────────┬──────────┬─────────────┘
     │        │          │
     ▼        ▼          ▼
┌─────────┐ ┌────────┐ ┌──────────────┐
│persistence│ │incremental│ │petgraph_storage│
│ 图谱持久化│ │MD5增量检测│ │ 序列化格式    │
└─────────┘ └────────┘ └──────────────┘
       │
       ▼
  ┌─────────────────────────────────────┐
  │            Services                  │
  │  CodeAnalyzer / SnippetService      │
  │  EmbeddingService                    │
  └──┬──────────────┬───────────────────┘
     │              │
     ▼              ▼
  ┌──────────────────────────────────────┐
  │           CodeGraph                   │
  │  CodeParser / PetCodeGraph           │
  │  EntityGraph / FileIndex             │
  │  TreeSitterParser (多语言)            │
  └──────────────────────────────────────┘
```

### 分层职责

| 层 | 模块 | 职责 |
|---|------|------|
| 入口 | `main.rs` | CLI 参数解析 (`clap`)，分发到 server 或 vectorize 子命令 |
| 配置 | `config.rs` | 从 `~/.codeseek/config/config.toml` 加载全局配置 |
| HTTP | `http/` | Axum 路由、请求处理、响应模型、ECharts HTML 模板 |
| 状态 | `storage/` | `StorageManager` 集中管理：图谱缓存、持久化、文件监听、嵌入任务 |
| 服务 | `services/` | 高层分析逻辑：代码分析器、代码片段服务、嵌入服务 |
| 核心 | `codegraph/` | 代码解析和图数据结构：AST 解析、图谱构建、类型定义 |
| CLI | `cli/` | CLI 参数定义、命令行运行器、离线的 analyze 和 vectorize |

## 核心类型详解

### PetCodeGraph — 函数调用有向图

```rust
pub struct PetCodeGraph {
    pub graph: DiGraph<FunctionInfo, CallRelation>,  // petgraph 有向图
    pub function_to_node: HashMap<Uuid, NodeIndex>,   // 函数ID → 节点索引
    pub node_to_function: HashMap<NodeIndex, Uuid>,   // 节点索引 → 函数ID
    pub function_names: HashMap<String, Vec<Uuid>>,   // 函数名 → ID列表(支持重载)
    pub file_functions: HashMap<PathBuf, Vec<Uuid>>,  // 文件 → 函数列表
    pub stats: CodeGraphStats,
}
```

**节点**：`FunctionInfo`，包含 `id`(Uuid)、`name`、`file_path`、`line_start/end`、`namespace`、`language`、`signature`

**边**：`CallRelation`，包含 `caller_id`、`callee_id`、`is_resolved`（是否成功解析）、行号

**核心查询方法**：

| 方法 | 说明 |
|------|------|
| `get_callers(id)` | 获取调用该函数的所有函数 |
| `get_callees(id)` | 获取该函数调用的所有函数 |
| `find_functions_by_name(name)` | 按函数名搜索（支持重载） |
| `find_functions_by_file(path)` | 按文件路径搜索 |
| `get_call_chain(id, max_depth)` | 递归获取调用链（BFS/DFS） |
| `has_cycles()` | 检测循环调用（petgraph::is_cyclic_directed） |
| `topological_sort()` | 拓扑排序 |
| `strongly_connected_components()` | 强连通分量（Kosaraju） |

### StorageManager — 状态管理中心

```rust
pub struct StorageManager {
    persistence: Arc<PersistenceManager>,              // 图谱持久化
    incremental: Arc<IncrementalManager>,              // MD5 增量检测
    graph: Arc<RwLock<Option<PetCodeGraph>>>,           // 内存图谱缓存
    storage_mode: StorageMode,                          // JSON / Binary / Both
    watchers: Arc<Mutex<HashMap<String, RecommendedWatcher>>>, // 文件监听器
    vector_tasks: Arc<Mutex<HashSet<String>>>,          // 嵌入任务防重复
    config: Arc<RwLock<Option<Config>>>,                 // 全局配置
    current_repo: Arc<RwLock<Option<String>>>,           // 绑定的仓库路径
}
```

所有组件通过 `StorageManager` 解耦：HTTP handlers、服务、解析器之间不直接依赖，通过 `StorageManager` 获取图谱、配置和状态。

### CodeAnalyzer — 高级分析器

封装 `CodeParser`，提供分析能力：

- `analyze_directory(dir)` — 全量分析目录，构建完整 CodeGraph
- `find_callers(func_name)` / `find_callees(func_name)` — 查找调用关系
- `find_call_chains(func_name, max_depth)` — 递归调用链
- `find_circular_dependencies()` — DFS 检测循环调用
- `find_most_complex_functions(limit)` — 按 (入度 + 出度) 排名的复杂函数
- `find_leaf_functions()` — 叶子函数（未被其他函数调用）
- `find_root_functions()` — 根函数（不调用其他函数）
- `generate_call_report()` — 生成文本分析报告

### EmbeddingService — 向量嵌入服务

核心属性：

- **嵌入提供者**：`OpenAICompatibleEmbeddingProvider`，兼容任意 OpenAI API 格式的嵌入服务
- **向量数据库**：LanceDB（嵌入式，零外部依赖）
- **嵌入缓存**：SQLite，key 为 `md5(model + code_block)`
- **增量更新**：通过 `projects.json` 存储文件 MD5 哈希，仅重新处理变更文件

嵌入流程：

```
源码文件 → tree-sitter 解析 → 提取函数/结构体 → 查询 SQLite 缓存
  ├─ 命中 → 使用缓存向量
  └─ 未命中 → 调用嵌入 API → 写入缓存 + LanceDB
```

表名规则：`{repo_dir_name}_{md5(repo_full_path)}`

## HTTP API 设计

### 路由表

| 方法 | 路径 | Handler | 说明 |
|------|------|---------|------|
| GET | `/health` | `health_check` | 返回服务运行状态 |
| GET | `/status` | `get_status` | 当前仓库：路径、函数数、文件数、嵌入状态 |
| POST | `/query_call_graph` | `query_call_graph` | 查询函数调用图谱，支持递归扩展 |
| POST | `/query_code_snippet` | `query_code_snippet` | 提取函数代码片段，可附带上下文行 |
| POST | `/query_code_skeleton` | `query_code_skeleton` | 批量提取文件的函数/类骨架签名 |
| POST | `/query_hierarchical_graph` | `query_hierarchical_graph` | 按函数展开的层级调用树 |
| POST | `/investigate_repo` | `investigate_repo` | 仓库全景：Top15 核心函数 + 目录树 + 骨架 |
| POST | `/semantic_search` | `semantic_search` | 自然语言语义搜索代码块 |
| POST | `/query_indexing_status` | `query_indexing_status` | 嵌入索引进度查询 |
| GET | `/` | `draw_call_graph_home` | ECharts 可视化主页 |
| GET | `/draw_call_graph` | `draw_call_graph` | 带查询参数的可视化页面 |

### 请求/响应格式

所有响应统一为：

```json
{
  "success": true,
  "data": { ... }
}
```

所有 handler 接收 `State<Arc<StorageManager>>`，通过 `storage.get_graph_clone()` 等获取数据。不依赖外部存储或缓存中间件。

### 关键端点详解

#### `POST /investigate_repo`

单次请求获取仓库全景视图，返回：
- **core_functions**：按出度排序的 Top 15 函数，每个函数附带去重后的 callers/callees 列表
- **directory_tree**：ASCII 风格目录树（自动忽略 `.git`、`node_modules`、`target` 等）
- **file_skeletons**：核心函数所在文件的骨架（函数/类签名）

#### `POST /query_hierarchical_graph`

从指定根函数出发，递归展开调用关系，构建层级树：

```
main
├── init_config
│   ├── load_config_file
│   └── validate_config
├── start_server
│   ├── bind_port
│   └── setup_middleware
└── run_event_loop
    ├── handle_request
    └── handle_signal
```

#### `POST /query_code_skeleton`

批量输入文件路径，对每个文件执行 tree-sitter 解析和骨架格式化：

```
// 输入: src/main.rs, src/lib.rs
// 输出: 每个文件的函数/结构体签名列表
pub fn main()
pub struct AppConfig { ... }
fn init_logging(level: LogLevel) -> Result<()>
```

## AST 解析层

### TreeSitterParser

封装 tree-sitter 的多语言解析：

```
TreeSitterParser::parse_file(path)
  → 检测语言 (按文件扩展名)
    → get_ast_parser_by_filename()
      → 返回对应语言的 parser + LanguageId
        → 解析 → AstSymbolInstanceArc 列表
```

每个 `AstSymbolInstance` 提供：

- `symbol_type()` — 符号类型（FunctionDeclaration、StructDeclaration、FunctionCall 等）
- `name()` — 符号名称
- `full_range()` — 完整代码范围
- `declaration_range()` — 声明范围
- `childs_guid()` — 子符号 ID 列表
- `symbol_info_struct()` — 扁平化的符号信息结构体
- `get_content_from_file_blocked()` — 从源文件提取代码内容

### 解析器分布

| 语言 | 模块 | tree-sitter 库 |
|------|------|---------------|
| Rust | `parsers/rust_parser.rs` | `tree-sitter-rust` |
| Python | `parsers/python_parser.rs` | `tree-sitter-python` |
| JavaScript | `parsers/javascript_parser.rs` | `tree-sitter-javascript` |
| TypeScript | `parsers/typescript_parser.rs` | `tree-sitter-typescript` |
| Go | `parsers/go_parser.rs` | `tree-sitter-go` |
| C++ | `parsers/cpp_parser.rs` | `tree-sitter-cpp` |
| Java | `parsers/java_parser.rs` | `tree-sitter-java` |

### Skeletonizer（骨架格式化器）

每种语言有一个 `SkeletonFormatter` 实现，将 AST 符号格式化为人类可读的代码骨架。`make_formatter(language_id)` 工厂函数返回对应的格式化器。

## 持久化层

### 存储路径

```
.codegraph_db/
├── projects.json            # 项目注册表 (project_id → ProjectRecord)
├── {project_id}/
│   ├── graph.json           # JSON 格式图谱
│   ├── graph.bin            # 二进制格式图谱（可选）
│   └── file_hashes.json     # {文件路径 → MD5哈希}
└── {project_id}/
    └── ...
```

### 存储模式

`StorageMode` 枚举控制图谱存储格式：

- `Json`：序列化为可读 JSON（默认）
- `Binary`：使用 bincode 序列化（更快、更小）
- `Both`：同时存储两种格式，加载时优先二进制

### 增量更新机制

`CodeParser.build_petgraph_code_graph(dir)` 的增量逻辑：

1. 扫描目录获取文件列表（跳过 `.git`、`target`、`node_modules` 等）
2. 加载 `file_hashes.json`（已有的文件哈希映射）
3. 对每个文件：
   - 计算当前 MD5
   - 与已存储哈希比较，相同则跳过解析
   - 不同则重新解析并更新哈希
4. 合并新解析的函数到已有图谱
5. 重新分析调用关系
6. 保存更新后的哈希

## 文件监听

`setup_watcher()` 使用 `notify` crate 监听文件系统事件：

```
文件变更 → notify watcher → 发送信号到 channel
  → 20 秒防抖 (debounce) → 超时后触发
    → 重新执行 perform_analysis (在 spawn_blocking 中)
    → 触发 embedding 重建
```

## 嵌入索引生命周期

### 启动时

`CodeSeekServer::start()` 流程中调用 `trigger_embedding_build()`：

1. 检查配置中 `enable_embedding` 是否为 true
2. 检查 `vector_tasks` 集合防止重复任务
3. 计算 LanceDB 表名：`{repo_dir_name}_{md5(repo_path)}`
4. 在 `tokio::spawn` 后台任务中执行向量化
5. 完成后更新 `projects.json` 的状态和时间戳

### 增量索引

`vectorize_directory()` 接收可选的 `existing_hashes` 参数：

- 读取 `projects.json` 中的 `file_hashes`
- 对每个文件计算 MD5，只处理变更文件
- 先在 `embedding_cache.sqlite` 中查缓存
- 调用嵌入 API 生成向量
- 每 100 条向量批量写入 LanceDB

### 缓存策略

```
SQLite embedding_cache 表:
  hash TEXT PRIMARY KEY          -- md5(model + code_block)
  vector BLOB                    -- bincode 序列化的 Vec<f32>
  created_at INTEGER             -- Unix 时间戳
```

缓存 key 包含模型名，切换嵌入模型后不会误用旧缓存。

## 配置系统

```toml
# ~/.codeseek/config/config.toml

[http]
server_port = 12800           # 默认 HTTP 端口
codebase_port = 12800         # codebase 服务端口

[codebase]
enable_embedding = true       # 是否启用语义搜索/嵌入
embedding_db_uri = "data/lancedb"  # LanceDB 存储路径
graph_db_uri = ".codegraph_db"     # 图谱持久化路径

[codebase.embedding]
model = "Qwen/Qwen3-Embedding-4B"  # 嵌入模型名
api_token = "sk-..."                # API 密钥
api_base_url = "https://api.siliconflow.cn/v1"  # API 端点
dimensions = 2560                   # 向量维度 (可选，覆盖模型默认值)

[global.llm]                    # 全局 LLM 配置
use_provider = "siliconflow"
[global.llm.providers.siliconflow]
model = "Qwen/Qwen3-235B-A22B"
temperature = 0.7
max_tokens = 8192
api_base_url = "https://api.siliconflow.cn/v1"
api_key = "sk-..."

[app]
enable_streaming = true

[agent]
conductor_max_steps = 30
coding_max_steps = 50
repo_max_steps = 20
lang = "zh"
```

## CLI 子命令

### `server`

```
cargo run -- server --repo-path /path/to/repo [--address 0.0.0.0:12800] [--storage-mode json|binary|both] [-v]
```

启动 HTTP 服务，自动初始化仓库（解析 + 嵌入 + 监听）。

### `vectorize`

```
cargo run -- vectorize --path /path/to/code --collection my-collection --db-uri data/lancedb
```

独立命令行模式：扫描目录、tree-sitter 解析、生成嵌入并写入 LanceDB。不启动 HTTP 服务。

## 技术栈

| 类别 | 技术 | 用途 |
|------|------|------|
| 语言 | Rust 2021 edition | 整体实现 |
| 异步运行时 | Tokio (full features) | HTTP 服务、文件监听、嵌入任务 |
| HTTP 框架 | Axum 0.7 | REST API |
| CLI | Clap 4.0 (derive) | 命令行参数解析 |
| 图结构 | Petgraph 0.6 | 有向图 (DiGraph)、循环检测、拓扑排序 |
| AST 解析 | Tree-sitter 0.25 | 多语言语法解析 |
| 向量数据库 | LanceDB 0.4.15 | 嵌入式向量存储 |
| 嵌入缓存 | Rusqlite 0.38 (bundled) | SQLite 嵌入式缓存 |
| HTTP 客户端 | Reqwest 0.13 | 调用嵌入 API |
| 序列化 | Serde + Bincode | JSON/二进制序列化 |
| 文件监听 | Notify 6.1 | 文件系统事件监听 |
| 中间件 | Tower-HTTP 0.5 | CORS 支持 |
| 日志 | Tracing 0.1 | 结构化日志 |
