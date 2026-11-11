use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::codegraph::types::{FileIndex, FileMetadata, SnippetIndex};

/// 增量更新的**记账**管理器（Phase P3 收口 B 恢复的**瘦身版**）。
///
/// ## 这个文件为什么还存在（以及为什么只剩这些）
/// Phase P3 收口 A 判定 `IncrementalManager::refresh_file` 整条**产边**链路是
/// `CodeParser` 产边链路的第二份副本、在 `src/` / `tests/` / `examples/` / `benches/`
/// 里**零调用者**（真实增量刷新一直是 `CodeParser::refresh_file`，由
/// `codegraph/repository.rs::RepositoryManager::refresh_file` 调用），于是把它连同
/// `TreeSitterParser` / `ResolverMode` / `FileScopeCtx` 缓存一起删掉了 —— 登记见
/// `src/storage/traits.rs` 里 `IncrementalUpdater` 上方的注释。
/// 但那次删除**把文件整体删掉、却漏了引用方**：`storage/mod.rs`（`pub mod` + `pub use`
/// + `StorageManager::incremental` 字段 + `get_incremental()`）、`storage/prelude.rs`、
/// 以及 `codegraph/repository.rs` 仍然需要 `IncrementalManager`，导致 HEAD 无法编译。
///
/// `repository.rs` 用到的**从来就只有记账面**（`needs_update` / `save_state` / `load_state`），
/// 所以这里按"只保留被真实引用的职责"恢复：
///   - 文件 MD5 记账：`compute_file_md5` / `needs_update` / `get_file_metadata` /
///     `get_all_file_metadata`；
///   - 索引只读访问：`get_file_index` / `get_snippet_index`；
///   - 状态落盘/加载：`save_state` / `load_state`（序列化字段名与收口 A 之前**逐字一致**，
///     因此既有 `incremental_state.json` 仍可被读回，不做任何迁移）；
///   - `IncrementalUpdater` trait 实现（该 trait 已在收口 A 去掉 `refresh_file`，
///     正好与这里的方法面一一对应）。
///
/// ⚠ **这里绝不产边、绝不解析源码**。任何"再加一个 `refresh_file`"的冲动都必须先读
/// `codegraph/parser.rs` 顶部的三个共用件（`extract_file_entities` /
/// `emit_legacy_call_edges_for_file` / `emit_layered_call_edges_for_file`）——产边只允许走
/// 那一条链路，且写边只允许经由 `codegraph::types::PetCodeGraph::upsert_call_site_edges`；
/// 本文件的"零产边"由 `tests/test_p1_edges_unchanged.rs` 的
/// `FIRST_WINS_ABSENCE_MARKERS` 反向断言钉死。
#[derive(Debug)]
pub struct IncrementalManager {
    /// 文件元数据（路径 -> MD5/大小/语言/更新时间），只由 `load_state` 填充
    file_metadata: HashMap<PathBuf, FileMetadata>,
    /// 文件索引（本结构体不写入，仅随 `save_state` / `load_state` 进出磁盘）
    file_index: FileIndex,
    /// 代码片段索引（同上）
    snippet_index: SnippetIndex,
}

impl IncrementalManager {
    pub fn new() -> Self {
        Self {
            file_metadata: HashMap::new(),
            file_index: FileIndex::default(),
            snippet_index: SnippetIndex::default(),
        }
    }

    /// 计算文件的 MD5 哈希值（与全量/增量解析链路里的 `md5::compute` 同一口径）
    pub fn compute_file_md5(&self, file_path: &Path) -> Result<String, io::Error> {
        let content = fs::read(file_path)?;
        let hash = md5::compute(&content);
        Ok(format!("{:x}", hash))
    }

    /// 检查文件是否需要更新：MD5 与记账值不同、或该文件从未登记过
    pub fn needs_update(&self, file_path: &Path) -> Result<bool, io::Error> {
        let current_md5 = self.compute_file_md5(file_path)?;

        if let Some(metadata) = self.file_metadata.get(file_path) {
            Ok(metadata.md5 != current_md5)
        } else {
            // 新文件，需要更新
            Ok(true)
        }
    }

    /// 获取单个文件的元数据
    pub fn get_file_metadata(&self, file_path: &Path) -> Option<&FileMetadata> {
        self.file_metadata.get(file_path)
    }

    /// 获取文件索引
    pub fn get_file_index(&self) -> &FileIndex {
        &self.file_index
    }

    /// 获取代码片段索引
    pub fn get_snippet_index(&self) -> &SnippetIndex {
        &self.snippet_index
    }

    /// 获取所有文件元数据
    pub fn get_all_file_metadata(&self) -> &HashMap<PathBuf, FileMetadata> {
        &self.file_metadata
    }

    /// 保存记账状态到文件（字段名与收口 A 之前完全一致 ⇒ 磁盘格式零迁移）
    pub fn save_state(&self, path: &Path) -> Result<(), String> {
        let state = serde_json::json!({
            "file_metadata": self.file_metadata,
            "file_index": self.file_index,
            "snippet_index": self.snippet_index,
        });

        fs::write(
            path,
            serde_json::to_string_pretty(&state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?,
        )
        .map_err(|e| format!("Failed to save state: {}", e))?;

        debug!("Incremental bookkeeping state saved to: {}", path.display());
        Ok(())
    }

    /// 从文件加载记账状态
    pub fn load_state(&mut self, path: &Path) -> Result<(), String> {
        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read state file: {}", e))?;

        let state: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse state: {}", e))?;

        if let Some(metadata) = state.get("file_metadata") {
            self.file_metadata = serde_json::from_value(metadata.clone())
                .map_err(|e| format!("Failed to deserialize file metadata: {}", e))?;
        }

        if let Some(index) = state.get("file_index") {
            self.file_index = serde_json::from_value(index.clone())
                .map_err(|e| format!("Failed to deserialize file index: {}", e))?;
        }

        if let Some(snippets) = state.get("snippet_index") {
            self.snippet_index = serde_json::from_value(snippets.clone())
                .map_err(|e| format!("Failed to deserialize snippet index: {}", e))?;
        }

        Ok(())
    }
}

impl Default for IncrementalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 记账面与 `IncrementalUpdater` 抽象一一对应（该 trait 已不含产边方法）。
impl crate::storage::traits::IncrementalUpdater for IncrementalManager {
    fn compute_file_md5(&self, file_path: &Path) -> Result<String, io::Error> {
        Self::compute_file_md5(self, file_path)
    }

    fn needs_update(&self, file_path: &Path) -> Result<bool, io::Error> {
        Self::needs_update(self, file_path)
    }

    fn get_file_index(&self) -> &FileIndex {
        Self::get_file_index(self)
    }

    fn get_snippet_index(&self) -> &SnippetIndex {
        Self::get_snippet_index(self)
    }

    fn get_all_file_metadata(&self) -> &HashMap<PathBuf, FileMetadata> {
        Self::get_all_file_metadata(self)
    }

    fn save_state(&self, path: &Path) -> Result<(), String> {
        Self::save_state(self, path)
    }

    fn load_state(&mut self, path: &Path) -> Result<(), String> {
        Self::load_state(self, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记账面必须真的能落盘再读回：`repository.rs` 的 `needs_update` 只可能因为
    /// `load_state` 恢复了 MD5 才返回 `false`（"文件未变 ⇒ 跳过"）。
    #[test]
    fn test_save_and_load_state_roundtrip_keeps_md5_bookkeeping() {
        let dir = tempfile::TempDir::new().expect("创建临时目录失败");
        let source = dir.path().join("sample.rs");
        fs::write(&source, b"fn a() {}").expect("写入样例源文件失败");

        let mut manager = IncrementalManager::new();
        let md5 = manager.compute_file_md5(&source).expect("计算 MD5 应成功");
        manager.file_metadata.insert(
            source.clone(),
            FileMetadata {
                path: source.clone(),
                md5: md5.clone(),
                last_updated: chrono::Utc::now(),
                file_size: 10,
                language: "rust".to_string(),
            },
        );

        // 未变 ⇒ 不需要更新；内容变了 ⇒ 需要更新
        assert!(!manager.needs_update(&source).expect("needs_update 应成功"));
        fs::write(&source, b"fn a() {} fn b() {}").expect("改写样例源文件失败");
        assert!(manager.needs_update(&source).expect("改写后 needs_update 应为 true"));

        let state_path = dir.path().join("incremental_state.json");
        manager.save_state(&state_path).expect("save_state 应成功");
        let raw = fs::read_to_string(&state_path).expect("读回状态文件失败");
        // 磁盘字段名是既有契约，不许改名
        for key in ["file_metadata", "file_index", "snippet_index"] {
            assert!(raw.contains(key), "状态文件缺少既有字段 {key}：{raw}");
        }

        let mut restored = IncrementalManager::new();
        restored.load_state(&state_path).expect("load_state 应成功");
        assert_eq!(restored.file_metadata.len(), 1, "MD5 记账必须原样读回");
        assert_eq!(
            restored.get_file_metadata(&source).map(|m| m.md5.clone()),
            Some(md5),
            "读回的 MD5 必须与落盘前一致"
        );
    }

    // ⚠ "本文件不许再长出第二份产边实现"这条**不在这里测**：它由
    // `tests/test_p1_edges_unchanged.rs::FIRST_WINS_ABSENCE_MARKERS`（源码扫描守卫）统一钉，
    // 在那儿写才能覆盖 `fn refresh_file` 等特征而不与本文件的字面量互相干扰。
}
