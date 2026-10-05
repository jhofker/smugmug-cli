use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

pub mod hash_store;

pub use hash_store::HashStore;

/// The cache directory `upload` and `backup` use: the hash store and file
/// index (one sled database), and `last_run.json`.
pub fn get_cache_path() -> Result<PathBuf> {
    let cache_dir = match ProjectDirs::from("com", "smugmug-cli", "smugmug-cli") {
        Some(dirs) => dirs.cache_dir().to_path_buf(),
        None => PathBuf::from(".cache"),
    };

    // Ensure the cache directory exists
    std::fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

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
