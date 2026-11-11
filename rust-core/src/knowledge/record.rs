//! Knowledge record definition.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic counter for generating unique sequence numbers within a millisecond.
static SEQ_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A single knowledge entry stored in the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRecord {
    /// Unique identifier, format: `kn_{unix_ts_ms}_{seq}`
    pub id: String,
    /// Embedding vector (2560 dimensions). Filled before LanceDB write.
    #[serde(skip)]
    pub vector: Vec<f32>,
    /// Knowledge type: `repo_retrieval` or `coding_modification`
    #[serde(rename = "type")]
    pub type_: String,
    /// Title, ≤30 characters
    pub title: String,
    /// Content, ≤500 characters
    pub content: String,
    /// Tags
    pub tags: Vec<String>,
    /// Related file paths
    pub related_files: Vec<String>,
    /// Source agent: `repo_agent` or `coding_agent`
    pub source_agent: String,
    /// Task ID
    pub task_id: String,
    /// Confidence 0.0–1.0
    pub confidence: f32,
    /// Creation time (RFC3339)
    pub created_at: String,
    /// Last update time (RFC3339)
    pub updated_at: String,
    /// Access count
    pub access_count: u32,
    /// Last accessed time (RFC3339, nullable)
    pub last_accessed: Option<String>,
    /// Parent record IDs
    pub parent_ids: Vec<String>,
}

impl KnowledgeRecord {
    /// Create a new KnowledgeRecord with a generated ID.
    pub fn new(title: String, content: String, type_: String, source_agent: String, task_id: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        let ts_ms = chrono::Utc::now().timestamp_millis() as u64;
        let seq = SEQ_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("kn_{}_{}", ts_ms, seq);

        Self {
            id,
            vector: Vec::new(),
            type_,
            title,
            content,
            tags: Vec::new(),
            related_files: Vec::new(),
            source_agent,
            task_id,
            confidence: 1.0,
            created_at: now.clone(),
            updated_at: now,
            access_count: 0,
            last_accessed: None,
            parent_ids: Vec::new(),
        }
    }

    /// Truncate title to 30 chars and content to 500 chars.
    pub fn sanitize(mut self) -> Self {
        if self.title.len() > 30 {
            self.title = self.title.chars().take(30).collect();
        }
        if self.content.len() > 500 {
            self.content = self.content.chars().take(500).collect();
        }
        self
    }

    /// Build the text to embed (title + content).
    pub fn embed_text(&self) -> String {
        format!("{} {}", self.title, self.content)
    }
}
