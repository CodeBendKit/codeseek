//! CLI handlers for knowledge subcommands.

use anyhow::{anyhow, Context, Result};
use serde_json::json;

use crate::cli::args::KnowledgeCommand;
use crate::config::Config;
use crate::knowledge::record::KnowledgeRecord;
use crate::knowledge::store::KnowledgeStore;

/// Handle a `codeseek knowledge ...` subcommand.
pub async fn handle_knowledge_command(cmd: KnowledgeCommand) -> Result<()> {
    let project_root = Config::detect_project_root()
        .ok_or_else(|| anyhow!("No project found. Run codeseek from within a git repository."))?;
    let project_hash = Config::compute_project_hash(&project_root);
    let store = KnowledgeStore::new(&project_hash).context("failed to initialize knowledge store")?;

    match cmd {
        KnowledgeCommand::Add { title, content, r#type, tags, related_files, source_agent, task_id, confidence } => {
            // 校验 type 枚举值（非法则报错）
            if r#type != "repo_retrieval" && r#type != "coding_modification" {
                return Err(anyhow!("invalid type '{}' (expected repo_retrieval or coding_modification)", r#type));
            }
            // 校验 source_agent 枚举值（非法则报错）
            if source_agent != "repo_agent" && source_agent != "coding_agent" {
                return Err(anyhow!("invalid source_agent '{}' (expected repo_agent or coding_agent)", source_agent));
            }
            let mut record = KnowledgeRecord::new(title, content, r#type, source_agent, task_id);
            record.tags = tags;
            record.related_files = related_files;
            record.confidence = confidence;
            let saved = store.add(record).await.context("failed to add knowledge")?;
            println!("{}", serde_json::to_string_pretty(&saved)?);
        }
        KnowledgeCommand::Search { query, limit, rerank, domains } => {
            let results = store.search(&query, limit, rerank, &domains).await.context("knowledge search failed")?;
            // KnowledgeSearchResult 本身可 Serialize（record 是 #[serde(flatten)]），直接输出
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        KnowledgeCommand::List { limit, r#type, tag } => {
            let records = store.list(limit, r#type.as_deref(), tag.as_deref()).await.context("knowledge list failed")?;
            println!("{}", serde_json::to_string_pretty(&records)?);
        }
        KnowledgeCommand::Delete { id } => {
            store.delete(&id).await.context("knowledge delete failed")?;
            println!("{}", serde_json::to_string_pretty(&json!({"deleted": true, "id": id}))?);
        }
    }
    Ok(())
}
