pub mod persistence;
pub mod incremental;
pub mod petgraph_storage;
pub mod traits;
pub mod traits_bm25;
pub mod tantivy_index;
pub mod prelude;
pub mod lock;

pub use persistence::PersistenceManager;
pub use incremental::IncrementalManager;
pub use petgraph_storage::{PetGraphStorage, PetGraphStorageManager};
pub use traits::{GraphPersistence, IncrementalUpdater, GraphSerializer};
pub use tantivy_index::TantivyBm25Index;

use std::sync::Arc;
use parking_lot::RwLock;
use dirs;
use crate::codegraph::types::PetCodeGraph;
use crate::cli::args::StorageMode;
use crate::config::Config;
use crate::storage::traits_bm25::TextSearchProvider;

pub struct StorageManager {
    persistence: Arc<PersistenceManager>,
    incremental: Arc<IncrementalManager>,
    graph: Arc<RwLock<Option<PetCodeGraph>>>,
    storage_mode: StorageMode,
    pub config: Arc<RwLock<Option<Config>>>,
    /// 共享的 BM25 全文搜索索引（Tantivy）
    pub bm25_index: Arc<parking_lot::RwLock<Option<Arc<dyn TextSearchProvider>>>>,
}

impl StorageManager {
    pub fn new() -> Self {
        Self::with_storage_mode(StorageMode::default())
    }

    pub fn with_storage_mode(storage_mode: StorageMode) -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let base_dir = home.join(".codeseek");

        Self {
            persistence: Arc::new(PersistenceManager::with_storage_mode(storage_mode.clone(), base_dir)),
            incremental: Arc::new(IncrementalManager::new()),
            graph: Arc::new(RwLock::new(None)),
            storage_mode,
            config: Arc::new(RwLock::new(None)),
            bm25_index: Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    pub fn set_config(&self, config: Config) {
        *self.config.write() = Some(config);
    }

    pub fn get_config(&self) -> Option<Config> {
        self.config.read().clone()
    }

    pub fn set_storage_mode(&mut self, storage_mode: StorageMode) {
        self.storage_mode = storage_mode.clone();
        Arc::get_mut(&mut self.persistence)
            .unwrap()
            .set_storage_mode(storage_mode);
    }

    pub fn get_storage_mode(&self) -> &StorageMode {
        &self.storage_mode
    }

    pub fn get_persistence(&self) -> Arc<PersistenceManager> {
        self.persistence.clone()
    }

    pub fn get_incremental(&self) -> Arc<IncrementalManager> {
        self.incremental.clone()
    }

    pub fn set_bm25_index(&self, index: Arc<dyn TextSearchProvider>) {
        *self.bm25_index.write() = Some(index);
    }

    pub fn get_bm25_index(&self) -> Option<Arc<dyn TextSearchProvider>> {
        self.bm25_index.read().clone()
    }

    pub fn get_graph(&self) -> Arc<RwLock<Option<PetCodeGraph>>> {
        self.graph.clone()
    }

    pub fn set_graph(&self, graph: PetCodeGraph) {
        *self.graph.write() = Some(graph);
    }

    pub fn get_graph_clone(&self) -> Option<PetCodeGraph> {
        self.graph.read().clone()
    }
}
