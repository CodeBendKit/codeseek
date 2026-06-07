# CodeSeek CLI 改造开发方案

## 目标

将 CodeSeek 从一个 HTTP server 模式改为纯 CLI 工具模式，通过 npm /
Homebrew 分发，作为 Claude Code 的代码智能插件使用。

## 核心设计决策

| 决策项 | 方案 |
|--------|------|
| 分发方式 | JS wrapper (npm) + 平台二进制下载 + brew formula |
| 通信模型 | 纯 CLI，无 HTTP，无 MCP。每次命令独立进程，直接读写磁盘索引 |
| 常驻进程 | 无。所有状态靠 `~/.codeseek/` 下磁盘文件承载 |
| 向量嵌入 | 保留。LanceDB + Tantivy + SQLite 缓存 |
| 模型配置 | 全局一份 `~/.codeseek/config.json`，所有项目共享 |
| 索引存储 | `~/.codeseek/<md5(project_root)>/` |
| 项目定位 | cwd 自动检测，向上遍历找 `.git/` 确定项目根 |
| 增量更新 | `codeseek init` 幂等，MD5 跳过未变更文件 |
| 自动更新 | `codeseek install-hooks` → `.git/hooks/post-commit` 触发 `codeseek init` |
| 并发安全 | 写入时文件锁，查询时不加锁 |
| 模型提供商 | 先只支持 OpenAI 兼容 API，默认 SiliconFlow |

## 一、新仓库结构
```
codeseek/
├── package.json              # npm 包定义（项目根目录）
├── tsconfig.json             # TypeScript 配置
├── src/                      # JS/TS 源码
│   ├── bin/
│   │   └── codeseek.ts       # JS CLI 入口（配置向导 / 下载 / 透传）
│   └── install/
│       └── download.ts       # 平台检测 + 二进制下载逻辑
├── scripts/
│   ├── postinstall.js        # npm postinstall 钩子（首次下载二进制）
│   └── build-native.sh       # 本地开发：编译 Rust → 拷贝到 ~/.codeseek/bin/
├── rust-core/                # ← 原项目 Rust 源码移入此处
│   ├── Cargo.toml
│   ├── build.rs
│   ├── src/
│   │   ├── main.rs           # CLI 入口（精简后）
│   │   ├── lib.rs            # 模块导出
│   │   ├── config.rs         # 改造：JSON 格式 + ~/.codeseek/config.json
│   │   ├── cli/
│   │   │   ├── args.rs       # 新 CLI 子命令定义
│   │   │   ├── runner.rs     # 命令分发
│   │   │   └── hooks.rs      # install-hooks 实现
│   │   ├── codegraph/        # 不变：AST 解析 + 图谱结构
│   │   ├── services/         # 保留 analyzer / embedding_service / hybrid_search
│   │   │                      # 移除 commit_embedding / repo_knowledge
│   │   └── storage/          # 改造：去 watchers/vector_tasks/current_repo
│   │                          # 新增 lock.rs 文件锁
│   └── tests/
├── tests/
│   ├── parsers/              # 已移入的 parser 测试模块
│   └── fixtures/             # 已移入的测试 fixture 文件
└── README.md
```

---

## 二、JS Wrapper 设计

### 2.1 package.json

```json
{
  "name": "codeseek",
  "version": "0.1.0",
  "description": "Code intelligence CLI tool for Claude Code",
  "bin": {
    "codeseek": "./dist/bin/codeseek.js"
  },
  "scripts": {
    "build": "tsc",
    "postinstall": "node dist/install/download.js"
  },
  "files": [
    "dist/"
  ],
  "engines": {
    "node": ">=20.0.0"
  }
}
```

### 2.2 入口逻辑 (`src/bin/codeseek.ts`)

```
用户执行 codeseek <args>

1. 检测 ~/.codeseek/config.json 是否存在
   ├── 不存在 → 开启交互式配置向导（@clack/prompts）
   │   ├── 选择模型提供商（先只 OpenAI 兼容）
   │   ├── 填写 API Base URL（默认 https://api.siliconflow.cn/v1）
   │   ├── 填写 Model Name
   │   ├── 填写 API Token（隐藏输入）
   │   └── 写入 ~/.codeseek/config.json
   └── 存在 → 继续

2. 检测 ~/.codeseek/bin/codeseek 是否存在
   ├── 不存在 → 调用 download.ts 下载对应平台二进制
   │             存入 ~/.codeseek/bin/codeseek，chmod 755
   └── 存在 → 继续

3. 透传参数给 Rust 二进制
   spawn("~/.codeseek/bin/codeseek", process.argv.slice(2))
```

### 2.3 二进制下载 (`src/install/download.ts`)

从 GitHub Release 下载对应平台二进制：

| 平台 | Release 文件名 |
|------|---------------|
| darwin-arm64 | `codeseek-darwin-arm64` |
| darwin-x64 | `codeseek-darwin-x64` |
| linux-x64 | `codeseek-linux-x64` |
| linux-arm64 | `codeseek-linux-arm64` |

下载到 `~/.codeseek/bin/codeseek`，设置 `0o755` 权限。

---

## 三、全局配置设计

### 3.1 `~/.codeseek/config.json`

```json
{
  "embedding": {
    "provider": "openai-compatible",
    "model": "Qwen/Qwen3-Embedding-4B",
    "api_token": "sk-...",
    "api_base_url": "https://api.siliconflow.cn/v1",
    "dimensions": 2560
  },
  "installed_hooks": {
    "/Users/wenwang/projects/my-app": ["post-commit", "post-merge"]
  }
}
```

- JS wrapper 配置向导仅写入此文件
- Rust 进程启动时 `Config::load()` 读取此文件
- 全局一份，所有项目复用
- 格式从 TOML 改为 JSON（与 `.claude/settings.json` 风格一致）

### 3.2 Rust 侧 Config 改造

- `config.rs` 删除 TOML 依赖，改用 `serde_json`
- 删除 `CodeSeekConfig` 中 HTTP server 相关字段 (`server_port`)
- 新增 `installed_hooks: HashMap<String, Vec<String>>` 跟踪已安装 hook 的项目
- Config 加载路径固定为 `~/.codeseek/config.json`

---

## 四、项目索引存储

### 4.1 `~/.codeseek/<project_hash>/`

```
~/.codeseek/
├── config.json                          # 全局配置
├── bin/
│   └── codeseek                         # 平台二进制
├── <md5(project_root)>/
│   ├── graph.bin                        # PetCodeGraph (bincode)
│   ├── graph.json                       # PetCodeGraph (JSON, 可选)
│   ├── file_hashes.json                 # 文件 MD5 哈希（增量检测）
│   ├── embeddings.lance/                # LanceDB 向量数据
│   ├── tantivy_bm25/                    # Tantivy BM25 全文索引
│   └── .lock                            # 写入锁（flock/fcntl）
└── <md5(another_project)>/
    └── ...
```

### 4.2 项目定位逻辑

Rust 进程启动时：
1. `std::env::current_dir()` 获取当前工作目录
2. 向上遍历父目录，找到第一个 `.git/` 目录
3. `md5(project_root)` → 定位 `~/.codeseek/<hash>/`
4. 找不到 `.git/` → 报错退出

---

## 五、CLI 命令设计

### 5.1 命令列表

| 命令 | 说明 | 参数 |
|------|------|------|
| `codeseek init` | 全量/增量索引构建 | `-i` 交互式首次配置（无 config.json 时自动触发） |
| `codeseek status` | 索引统计 | `--json` 输出 JSON 格式 |
| `codeseek search <query>` | 混合检索 | `--limit N` `--json` |
| `codeseek callers <symbol>` | 查询调用者 | `--json` |
| `codeseek callees <symbol>` | 查询被调用者 | `--json` |
| `codeseek uninit` | 删除当前项目索引 | `--force` 跳过确认 |
| `codeseek install-hooks` | 安装 git hooks | — |

### 5.2 各命令详细说明

#### `codeseek init`

```
codeseek init          # 使用默认参数
codeseek init -i       # 交互式确认配置（首次无 config.json 自动触发）
```

流程：
1. 加载全局 config.json
2. 定位项目根目录（cwd → .git/）
3. 计算 project_hash = md5(project_root)
4. 创建 `~/.codeseek/<hash>/` 目录
5. 获取文件锁（排他）
6. 扫描所有源文件
7. 加载 `file_hashes.json`，MD5 跳过未变更文件
8. Tree-sitter 解析变更文件，构建 PetCodeGraph
9. 提取函数/结构体代码块，查询 SQLite 缓存
10. 调用嵌入 API 生成向量（缓存命中则跳过）
11. 批量写入 LanceDB
12. 构建 Tantivy BM25 索引
13. 序列化 PetCodeGraph → graph.bin
14. 保存 file_hashes.json
15. 释放文件锁
16. 输出统计信息

#### `codeseek status`

```
codeseek status        # 人类可读输出
codeseek status --json # JSON 输出（供 Claude Code 解析）
```

输出：
- 项目路径
- 总函数数 / 文件数
- 嵌入状态（已完成 / 进行中 / 未构建）
- 最后索引时间
- 数据库大小

#### `codeseek search <query>`

```
codeseek search "handle HTTP request" --limit 10
codeseek search "auth" --json
```

流程：
1. 加载全局配置
2. 定位项目 → project_hash
3. 打开 LanceDB + Tantivy + PetCodeGraph
4. 密集搜索（向量 ANN）
5. 稀疏搜索（BM25）
6. RRF 融合排序
7. 可选重排序（如果配置了 reranker）
8. 输出结果

#### `codeseek callers <symbol>` / `codeseek callees <symbol>`

```
codeseek callers main
codeseek callees "UserService::login" --json
```

流程：
1. 定位项目
2. 加载 PetCodeGraph
3. `find_functions_by_name()`
4. 遍历 `get_callers()` / `get_callees()`
5. 输出

#### `codeseek uninit`

```
codeseek uninit        # 确认后删除
codeseek uninit --force
```

流程：
1. 定位项目 → project_hash
2. 确认用户意图
3. 加锁
4. 删除 `~/.codeseek/<hash>/` 目录
5. 从 config.json 的 `installed_hooks` 中移除
6. 释放锁

#### `codeseek install-hooks`

```
codeseek install-hooks
```

流程：
1. 定位项目，找到 `.git/hooks/`
2. 写入 `post-commit` 文件：`#!/bin/sh\ncodeseek init`
3. 写入 `post-merge` 文件：`#!/bin/sh\ncodeseek init`
4. chmod 755
5. 更新 `~/.codeseek/config.json` 的 `installed_hooks`
6. 提示用户已安装

---

## 六、Rust 侧改造详细清单

### 6.1 删除

| 路径 | 说明 |
|------|------|
| `src/http/` | 整个 HTTP 模块 |
| `src/cli/args.rs` 中 `server` / `vectorize` 子命令 | — |
| `src/services/commit_embedding_service.rs` | — |
| `src/services/repo_knowledge_service.rs` | — |
| `storage/mod.rs` 中: `watchers: HashMap<...>` | — |
| `storage/mod.rs` 中: `vector_tasks: HashSet<...>` | — |
| `storage/mod.rs` 中: `current_repo` + `try_bind_repo()` | — |
| `storage/mod.rs` 中: `commit_embedding_service` 字段 | — |
| `storage/mod.rs` 中: `repo_knowledge_service` 字段 | — |
| `storage/mod.rs` 中: `watcher` 相关方法 | — |
| `storage/mod.rs` 中: `init_commit_embedding_service()` | — |
| `storage/mod.rs` 中: `init_repo_knowledge_service()` | — |
| `http/handlers/` 中 `setup_watcher()` / `trigger_embedding_build()` | — |
| `config.rs` 中: `server_port` / `codebase_port` / HTTP 配置 | — |
| `config.rs` 中: `.toml` 读取 → 改为 `.json` | — |
| `storage/petgraph_storage.rs` 中 GraphML/GEXF 导出 | 可选删除 |

### 6.2 新增

| 路径 | 说明 |
|------|------|
| `cli/hooks.rs` | git hook 安装/卸载逻辑 |
| `storage/lock.rs` | 文件锁实现（基于 `fs2` crate 或 `flock` syscall） |

### 6.3 改造

| 路径 | 改动 |
|------|------|
| `main.rs` | 删除 server/vectorize 逻辑，新增 CLI 子命令分发 |
| `config.rs` | TOML → JSON；删除 HTTP 字段；新增 `installed_hooks` |
| `cli/args.rs` | 新子命令：`Init` / `Status` / `Search` / `Callers` / `Callees` / `Uninit` / `InstallHooks` |
| `cli/runner.rs` | 新增各命令的分发函数 |
| `storage/mod.rs` | 新增 cwd 自动检测；传入 project_root 计算 hash；简化构造函数 |
| `storage/persistence.rs` | 存储路径改为 `~/.codeseek/<hash>/` |
| `services/embedding_service.rs` | 存储路径从相对改为绝对；传入 project_root 和 config |
| `services/hybrid_search.rs` | 从全局配置读取参数 |
| `Cargo.toml` | 新增 `fs2` 依赖 |

---

## 七、文件锁实现

使用 `fs2` crate（`flock` 封装）：

```rust
// storage/lock.rs
pub struct FileLock {
    file: std::fs::File,
}

impl FileLock {
    /// 排他锁（写入索引时使用）
    pub fn exclusive(lock_path: &Path) -> Result<Self, ...> {
        let file = OpenOptions::new().create(true).write(true).open(lock_path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }

    /// 共享锁（查询时可选使用）
    pub fn shared(lock_path: &Path) -> Result<Self, ...> {
        let file = OpenOptions::new().create(true).read(true).open(lock_path)?;
        fs2::FileExt::lock_shared(&file)?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        fs2::FileExt::unlock(&self.file).ok();
    }
}
```

`init` / `uninit` 用排他锁；`search` / `callers` / `callees` 用共享锁。

---

## 八、开发阶段

| 阶段 | 内容 | 验收标准 |
|------|------|----------|
| **Phase 1** | 仓库结构调整 | `rust-core/` 下 `cargo build` 通过 |
| **Phase 2** | Rust CLI 改造 | `cargo run -- init` 执行完整索引构建 |
| **Phase 3** | 存储路径迁移 | `~/.codeseek/<hash>/` 目录正确生成 |
| **Phase 4** | JS Wrapper | `npm install -g .` 后 `codeseek` 可执行 |
| **Phase 5** | CI/CD | GitHub Actions 多平台编译 + npm publish |
| **Phase 6** | 集成测试 | 端到端：`init` → `search` → 验证结果 |

---

## 九、参考项目

codegraph (`@colbymchenry/codegraph`) 的交互逻辑作为 CLI 设计参考：

- `codegraph init` — 幂等索引构建
- `codegraph sync` — 增量更新（codeseek 合入 `init`）
- `codegraph install` — 交互式安装向导（codeseek 用 `-i` 或自动触发）
- `codegraph query` — 即时符号搜索（codeseek 对应 `search`）
- git hook 自动安装方案来自 codegraph 的 `installer/targets/` 实现

---

**协议确认日期**: 2026-06-06
**开发开始**: Phase 1 即刻启动
