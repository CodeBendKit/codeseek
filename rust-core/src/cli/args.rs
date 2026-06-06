use clap::{Parser, Subcommand, ValueEnum};

/// 存储方式配置
#[derive(Debug, Clone, ValueEnum)]
pub enum StorageMode {
    /// 仅JSON格式存储
    Json,
    /// 仅二进制格式存储
    Binary,
    /// 同时保存JSON和二进制格式
    Both,
}

impl Default for StorageMode {
    fn default() -> Self {
        StorageMode::Binary
    }
}

#[derive(Parser, Debug)]
#[clap(name = "codeseek", author, version, about = "Code intelligence CLI tool", long_about = None)]
pub struct Cli {
    /// Verbose mode
    #[clap(short, long, action)]
    pub verbose: bool,

    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// 构建/更新代码索引（首次全量，后续MD5增量）
    Init {
        /// 交互式配置引导
        #[clap(short = 'i', long, action)]
        interactive: bool,
    },
    /// 查看索引状态
    Status {
        /// JSON 格式输出
        #[clap(long, action)]
        json: bool,
    },
    /// 语义搜索代码（向量+BM25+RRF融合）
    Search {
        /// 搜索查询文本
        query: String,
        /// 返回结果数量
        #[clap(short, long, default_value = "10")]
        limit: usize,
        /// JSON 格式输出
        #[clap(long, action)]
        json: bool,
    },
    /// 查询调用者
    Callers {
        /// 函数/符号名称
        symbol: String,
        /// JSON 格式输出
        #[clap(long, action)]
        json: bool,
    },
    /// 查询被调用者
    Callees {
        /// 函数/符号名称
        symbol: String,
        /// JSON 格式输出
        #[clap(long, action)]
        json: bool,
    },
    /// 删除当前项目的索引数据
    Uninit {
        /// 跳过确认
        #[clap(long, action)]
        force: bool,
    },
    /// 安装 git hooks（post-commit, post-merge → codeseek init）
    InstallHooks,
}
