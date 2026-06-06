use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::codegraph::types::{EntityGraph, FileMetadata, FileIndex, PetCodeGraph, SnippetIndex};

/// Graph persistence abstraction for saving/loading graphs and auxiliary metadata
pub trait GraphPersistence {
    fn save_graph(&self, project_id: &str, graph: &PetCodeGraph) -> io::Result<()>;
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
pub trait IncrementalUpdater {
    fn compute_file_md5(&self, file_path: &Path) -> Result<String, std::io::Error>;
    fn needs_update(&self, file_path: &Path) -> Result<bool, std::io::Error>;

    fn refresh_file(
        &mut self,
        file_path: &PathBuf,
        entity_graph: &mut EntityGraph,
        call_graph: &mut PetCodeGraph,
    ) -> Result<(), String>;

    fn get_file_index(&self) -> &FileIndex;
    fn get_snippet_index(&self) -> &SnippetIndex;
    fn get_all_file_metadata(&self) -> &HashMap<PathBuf, FileMetadata>;

    fn save_state(&self, path: &Path) -> Result<(), String>;
    fn load_state(&mut self, path: &Path) -> Result<(), String>;
}

/// Serializer abstraction for PetCodeGraph import/export
pub trait GraphSerializer {
    fn save_to_file(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
    fn load_from_file(file_path: &Path) -> Result<PetCodeGraph, String>;

    fn save_to_json(code_graph: &PetCodeGraph) -> Result<String, String>;
    fn load_from_json(json_str: &str) -> Result<PetCodeGraph, String>;

    fn save_to_binary(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
    fn load_from_binary(file_path: &Path) -> Result<PetCodeGraph, String>;

    fn export_to_graphml(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
    fn export_to_gexf(code_graph: &PetCodeGraph, file_path: &Path) -> Result<(), String>;
} 