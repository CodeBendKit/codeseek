use std::path::PathBuf;
use std::fs;
use std::io;
use dirs;
use std::collections::HashMap;
use crate::codegraph::types::PetCodeGraph;
use crate::storage::petgraph_storage::PetGraphStorageManager;
use crate::cli::args::StorageMode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub struct PersistenceManager {
    base_dir: PathBuf,
    storage_mode: StorageMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub project_id: String,
    pub project_dir: String,
    pub parsed_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProjectsRegistry {
    // key: project_id
    projects: HashMap<String, ProjectRecord>,
}

impl PersistenceManager {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let base_dir = home.join(".codeseek");
        Self::with_storage_mode(StorageMode::default(), base_dir)
    }

    pub fn with_storage_mode(storage_mode: StorageMode, base_dir: PathBuf) -> Self {
        // Create base directory if it doesn't exist
        if !base_dir.exists() {
            fs::create_dir_all(&base_dir).ok();
        }
        
        Self { base_dir, storage_mode }
    }

    pub fn set_storage_mode(&mut self, storage_mode: StorageMode) {
        self.storage_mode = storage_mode;
    }

    pub fn get_storage_mode(&self) -> &StorageMode {
        &self.storage_mode
    }

    /// 获取项目数据目录: ~/.codeseek/projects/<project_id>/
    fn project_dir(&self, project_id: &str) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(".codeseek").join("projects").join(project_id)
    }

    pub fn save_graph(&self, project_id: &str, graph: &PetCodeGraph) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        fs::create_dir_all(&project_dir)?;
        
        match self.storage_mode {
            StorageMode::Json => {
                self.save_graph_json(project_id, graph)?;
            },
            StorageMode::Binary => {
                self.save_graph_binary(project_id, graph)?;
            },
            StorageMode::Both => {
                self.save_graph_json(project_id, graph)?;
                self.save_graph_binary(project_id, graph)?;
            },
        }
        
        Ok(())
    }

    fn save_graph_json(&self, project_id: &str, graph: &PetCodeGraph) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        let graph_file = project_dir.join("graph.json");
        
        PetGraphStorageManager::save_to_file(graph, &graph_file)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        
        Ok(())
    }

    fn save_graph_binary(&self, project_id: &str, graph: &PetCodeGraph) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        let graph_file = project_dir.join("graph.bin");
        
        PetGraphStorageManager::save_to_binary(graph, &graph_file)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        
        Ok(())
    }

    pub fn load_graph(&self, project_id: &str) -> io::Result<Option<PetCodeGraph>> {
        match self.storage_mode {
            StorageMode::Json => self.load_graph_json(project_id),
            StorageMode::Binary => self.load_graph_binary(project_id),
            StorageMode::Both => {
                // 优先尝试加载二进制格式（更快），如果失败则加载JSON格式
                match self.load_graph_binary(project_id) {
                    Ok(graph) => Ok(graph),
                    Err(_) => self.load_graph_json(project_id),
                }
            },
        }
    }

    fn load_graph_json(&self, project_id: &str) -> io::Result<Option<PetCodeGraph>> {
        let graph_file = self.project_dir(project_id).join("graph.json");
        
        if !graph_file.exists() {
            return Ok(None);
        }
        
        let graph = PetGraphStorageManager::load_from_file(&graph_file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        
        Ok(Some(graph))
    }

    fn load_graph_binary(&self, project_id: &str) -> io::Result<Option<PetCodeGraph>> {
        let graph_file = self.project_dir(project_id).join("graph.bin");
        
        if !graph_file.exists() {
            return Ok(None);
        }
        
        let graph = PetGraphStorageManager::load_from_binary(&graph_file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        
        Ok(Some(graph))
    }

    pub fn save_file_hash(&self, project_id: &str, file_path: &str, hash: &str) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        fs::create_dir_all(&project_dir)?;
        
        let hash_file = project_dir.join("file_hashes.json");
        let mut hashes: HashMap<String, String> = if hash_file.exists() {
            let content = fs::read_to_string(&hash_file)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            HashMap::new()
        };
        
        hashes.insert(file_path.to_string(), hash.to_string());
        let json = serde_json::to_string_pretty(&hashes)?;
        fs::write(hash_file, json)?;
        
        Ok(())
    }

    pub fn load_file_hashes(&self, project_id: &str) -> io::Result<HashMap<String, String>> {
        let hash_file = self.project_dir(project_id).join("file_hashes.json");
        
        if !hash_file.exists() {
            return Ok(HashMap::new());
        }
        
        let content = fs::read_to_string(hash_file)?;
        let hashes: HashMap<String, String> = serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        
        Ok(hashes)
    }

    pub fn delete_project(&self, project_id: &str) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        if project_dir.exists() {
            fs::remove_dir_all(project_dir)?;
        }
        // also remove from registry if present
        let mut registry = self.load_registry()?;
        registry.projects.remove(project_id);
        self.save_registry(&registry)?;
        Ok(())
    }

    pub fn list_projects(&self) -> io::Result<Vec<String>> {
        let projects_dir = crate::config::Config::projects_dir();
        let mut projects = Vec::new();
        
        if projects_dir.exists() {
            for entry in fs::read_dir(&projects_dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        projects.push(name.to_string());
                    }
                }
            }
        }
        
        Ok(projects)
    }

    /// 获取已保存的文件信息
    pub fn get_saved_files_info(&self, project_id: &str) -> io::Result<Vec<String>> {
        let project_dir = self.project_dir(project_id);
        let mut files = Vec::new();
        
        if !project_dir.exists() {
            return Ok(files);
        }
        
        for entry in fs::read_dir(&project_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    let metadata = entry.metadata()?;
                    let size = metadata.len();
                    files.push(format!("{} ({} bytes)", name, size));
                }
            }
        }
        
        Ok(files)
    }

    // ---- Projects registry (for parsed projects) ----

    fn registry_path(&self) -> PathBuf {
        self.base_dir.join("projects.json")
    }

    fn load_registry(&self) -> io::Result<ProjectsRegistry> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(ProjectsRegistry::default());
        }
        let content = fs::read_to_string(path)?;
        let reg: ProjectsRegistry = serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(reg)
    }

    fn save_registry(&self, registry: &ProjectsRegistry) -> io::Result<()> {
        let path = self.registry_path();
        let json = serde_json::to_string_pretty(registry)?;
        fs::write(path, json)
    }

    pub fn register_project(&self, project_id: &str, project_dir: &str) -> io::Result<()> {
        let mut registry = self.load_registry()?;
        let record = ProjectRecord {
            project_id: project_id.to_string(),
            project_dir: project_dir.to_string(),
            parsed_at: Utc::now(),
        };
        registry.projects.insert(project_id.to_string(), record);
        self.save_registry(&registry)
    }

    pub fn is_project_parsed(&self, project_id: &str) -> io::Result<bool> {
        let registry = self.load_registry()?;
        Ok(registry.projects.contains_key(project_id))
    }

    pub fn find_project_by_dir(&self, project_dir: &str) -> io::Result<Option<String>> {
        let registry = self.load_registry()?;
        for (pid, rec) in registry.projects.iter() {
            if rec.project_dir == project_dir {
                return Ok(Some(pid.clone()));
            }
        }
        Ok(None)
    }

    pub fn list_parsed_projects(&self) -> io::Result<Vec<ProjectRecord>> {
        let registry = self.load_registry()?;
        Ok(registry.projects.values().cloned().collect())
    }
} 

impl crate::storage::traits::GraphPersistence for PersistenceManager {
    fn save_graph(&self, project_id: &str, graph: &PetCodeGraph) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        fs::create_dir_all(&project_dir)?;

        match self.storage_mode {
            StorageMode::Json => {
                self.save_graph_json(project_id, graph)?;
            },
            StorageMode::Binary => {
                self.save_graph_binary(project_id, graph)?;
            },
            StorageMode::Both => {
                self.save_graph_json(project_id, graph)?;
                self.save_graph_binary(project_id, graph)?;
            },
        }
        Ok(())
    }

    fn load_graph(&self, project_id: &str) -> io::Result<Option<PetCodeGraph>> {
        match self.storage_mode {
            StorageMode::Json => self.load_graph_json(project_id),
            StorageMode::Binary => self.load_graph_binary(project_id),
            StorageMode::Both => {
                match self.load_graph_binary(project_id) {
                    Ok(Some(graph)) => Ok(Some(graph)),
                    _ => self.load_graph_json(project_id),
                }
            },
        }
    }

    fn save_file_hash(&self, project_id: &str, file_path: &str, hash: &str) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        fs::create_dir_all(&project_dir)?;

        let hashes_file = project_dir.join("file_hashes.json");
        let mut hashes: HashMap<String, String> = if hashes_file.exists() {
            let content = fs::read_to_string(&hashes_file)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            HashMap::new()
        };

        hashes.insert(file_path.to_string(), hash.to_string());

        let json = serde_json::to_string_pretty(&hashes)?;
        fs::write(&hashes_file, json)?;
        Ok(())
    }

    fn load_file_hashes(&self, project_id: &str) -> io::Result<HashMap<String, String>> {
        let hashes_file = self.project_dir(project_id).join("file_hashes.json");
        if !hashes_file.exists() {
            return Ok(HashMap::new());
        }
        let content = fs::read_to_string(&hashes_file)?;
        let hashes: HashMap<String, String> = serde_json::from_str(&content).unwrap_or_default();
        Ok(hashes)
    }

    fn delete_project(&self, project_id: &str) -> io::Result<()> {
        let project_dir = self.project_dir(project_id);
        if project_dir.exists() {
            fs::remove_dir_all(project_dir)?;
        }
        Ok(())
    }

    fn list_projects(&self) -> io::Result<Vec<String>> {
        Self::list_projects(self)
    }

    fn get_saved_files_info(&self, project_id: &str) -> io::Result<Vec<String>> {
        Self::get_saved_files_info(self, project_id)
    }

    fn register_project(&self, project_id: &str, project_dir: &str) -> io::Result<()> {
        Self::register_project(self, project_id, project_dir)
    }

    fn is_project_parsed(&self, project_id: &str) -> io::Result<bool> {
        Self::is_project_parsed(self, project_id)
    }

    fn find_project_by_dir(&self, project_dir: &str) -> io::Result<Option<String>> {
        Self::find_project_by_dir(self, project_dir)
    }

    fn list_parsed_projects(&self) -> io::Result<Vec<crate::storage::persistence::ProjectRecord>> {
        Self::list_parsed_projects(self)
    }
} 