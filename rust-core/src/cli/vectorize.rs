use tracing::info;
use crate::config::Config;
use crate::services::embedding_service::EmbeddingService;

/// Run vectorize command
pub async fn run_vectorize(path: String, collection: String, db_path: String, config: Option<Config>) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting vectorize command");
    info!("Path: {}", path);
    info!("Collection: {}", collection);
    info!("LanceDB Path: {}", db_path);
    
    // Create vectorize service
    let service = EmbeddingService::new(&db_path, collection, config.as_ref(), None).await?;
    
    // Ensure collection exists
    service.ensure_collection().await?;
    
    // Vectorize directory
    service.vectorize_directory(&path, None).await?;
    
    info!("Vectorize command completed successfully");
    Ok(())
}
