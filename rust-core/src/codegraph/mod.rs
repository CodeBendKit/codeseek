pub mod graph;
pub mod parser;
/// Phase P1 交付物 3：名称解析的数据层（作用域树 / import 绑定表 / 不塌缩索引 / 分层候选）。
/// 本阶段**只建不消费**：没有任何建图路径读取它，四处同名 first-wins 一律未改。
pub mod resolution;
pub mod types;
pub mod treesitter;
pub mod repository;
pub mod chunker;

pub use graph::CodeGraph;
pub use types::{
    CallRelation, FunctionInfo, GraphNode, GraphRelation, PetCodeGraph,
    ClassInfo, ClassType, EntityNode, EntityEdge, EntityEdgeType, EntityGraph,
    FileMetadata, FileIndex, SnippetIndex, SnippetInfo
};
pub use treesitter::TreeSitterParser;
pub use repository::{RepositoryManager, RepositoryStats, SearchResult};