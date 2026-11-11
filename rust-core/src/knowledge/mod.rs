//! Knowledge management module for codeseek.
//!
//! Provides storage, search, and CLI/MCP interfaces for managing
//! knowledge entries with vector embeddings.

pub mod record;
pub mod store;
pub mod search;
pub mod tantivy;
pub mod cli;

pub use record::KnowledgeRecord;
pub use store::KnowledgeStore;
pub use search::KnowledgeSearchResult;
