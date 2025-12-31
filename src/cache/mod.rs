use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

pub mod hash_store;

pub use hash_store::{HashStore, UploadedFile, CacheStats, calculate_file_hash};

/// Get the default cache directory path
pub fn get_cache_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "jhofker", "smugmug-cli")
        .context("Failed to determine project directories")?;

    let cache_dir = project_dirs.cache_dir().join("hash_store");

    // Ensure the cache directory exists
    std::fs::create_dir_all(&cache_dir)
        .context("Failed to create cache directory")?;

    Ok(cache_dir)
}

/// Clear the deduplication cache
pub fn clear_cache() -> Result<()> {
    let cache_path = get_cache_path()?;
    let store = HashStore::new(cache_path.to_str().unwrap())?;

    let stats = store.stats()?;
    println!("Clearing {} entries from cache...", stats.total_entries);

    store.clear()?;
    println!("Cache cleared successfully!");

    Ok(())
}
