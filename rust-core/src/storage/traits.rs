use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::codegraph::types::{FileMetadata, FileIndex, PetCodeGraph, SnippetIndex};

/// Graph persistence abstraction for saving/loading graphs and auxiliary metadata
pub trait GraphPersistence {
    /// 落盘图缓存。**必须**同时传入本轮的解析档位戳：
    /// - `resolver_mode`：`crate::cli::args::ResolverMode::as_str()` 的稳定名
    ///   （`"legacy"` / `"layered"`）；
    /// - `max_candidates`：`crate::storage::petgraph_storage::clamp_max_candidates()`
    ///   折算后的 `--max-candidates`。
    ///
    /// 为什么这两个参数进 trait 而不是留成实现细节：戳是缓存**身份**的一部分，
    /// 少一个参数就会出现"能写出无戳缓存"的路径，而该缓存读回来必然过不了档位门禁
    /// ⇒ 每次全量重建（正确但永远命中不了缓存）。让编译器强制每个写缓存的人交代档位，
    /// 是 P3 收口这一版收敛的本意。
    fn save_graph(&self, project_id: &str, graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) -> io::Result<()>;
    fn load_graph(&self, project_id: &str) -> io::Result<Option<PetCodeGraph>>;

    fn save_file_hash(&self, project_id: &str, file_path: &str, hash: &str) -> io::Result<()>;
    fn load_file_hashes(&self, project_id: &str) -> io::Result<HashMap<String, String>>;

    fn delete_project(&self, project_id: &str) -> io::Result<()>;
    fn list_projects(&self) -> io::Result<Vec<String>>;

    fn get_saved_files_info(&self, project_id: &str) -> io::Result<Vec<String>>;

    fn register_project(&self, project_id: &str, project_dir: &str) -> io::Result<()>;
    fn is_project_parsed(&self, project_id: &str) -> io::Result<bool>;
    fn find_project_by_dir(&self, project_dir: &str) -> io::Result<Option<String>>;
    fn list_parsed_projects(&self) -> io::Result<Vec<crate::storage::persistence::ProjectRecord>>;
}

/// Incremental updater abstraction for file-based graph updates
///
/// ⚠ Phase P3 收口：原来的 `refresh_file`（"自己解析文件、自己提实体、自己产边"）已从本
/// trait 与 `IncrementalManager` 一起删除 —— 它在 `src/` / `tests/` / `examples/` /
/// `benches/` 里**零调用者**（真实的增量刷新走 `CodeParser::refresh_file`，由
/// `codegraph/repository.rs::RepositoryManager::refresh_file` 调用），是 `CodeParser`
/// 产边链路的第二份副本，也正是 P0 记录在案的"三套并行 first-wins 实现"里的一套。
/// 本 trait 因此只覆盖"文件哈希 / 索引记账"，不再覆盖产边。
pub trait IncrementalUpdater {
    fn compute_file_md5(&self, file_path: &Path) -> Result<String, std::io::Error>;
    fn needs_update(&self, file_path: &Path) -> Result<bool, std::io::Error>;

    fn get_file_index(&self) -> &FileIndex;
    fn get_snippet_index(&self) -> &SnippetIndex;
    fn get_all_file_metadata(&self) -> &HashMap<PathBuf, FileMetadata>;

    fn save_state(&self, path: &Path) -> Result<(), String>;
    fn load_state(&mut self, path: &Path) -> Result<(), String>;
}

/// Serializer abstraction for PetCodeGraph import/export
pub trait GraphSerializer {
    /// 三个 `save_*` 都带解析档位戳，参数含义见 [`GraphPersistence::save_graph`]。
    fn save_to_file(code_graph: &PetCodeGraph, file_path: &Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String>;
    fn load_from_file(file_path: &Path) -> Result<PetCodeGraph, String>;

    fn save_to_json(code_graph: &PetCodeGraph, resolver_mode: &str, max_candidates: u16) -> Result<String, String>;
    fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String>;

    fn save_to_binary(code_graph: &PetCodeGraph, file_path: &Path, resolver_mode: &str, max_candidates: u16) -> Result<(), String>;
    fn load_from_binary(file_path: &Path) -> Result<PetCodeGraph, String>;

    fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
    fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
} 