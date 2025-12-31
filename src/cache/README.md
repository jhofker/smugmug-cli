# Cache Module

This module implements the hash-based deduplication cache system for SmugMug CLI.

## Overview

The cache system prevents re-uploading files that have already been uploaded to SmugMug by calculating SHA256 hashes of files and storing metadata about previously uploaded files in a local database.

## Components

### `hash_store.rs`

Core implementation of the cache system using sled embedded database.

#### `HashStore`

Thread-safe hash store that maps SHA256 hashes to uploaded file metadata.

**Methods:**

- `new(path: &str) -> Result<Self>` - Creates a new HashStore at the specified path
- `get(&self, hash: &str) -> Result<Option<UploadedFile>>` - Retrieves uploaded file metadata by hash
- `insert(&self, hash: &str, file: UploadedFile) -> Result<()>` - Stores metadata for an uploaded file
- `clear(&self) -> Result<()>` - Clears all entries from the cache
- `stats(&self) -> Result<CacheStats>` - Returns statistics about the cache

**Thread Safety:**

HashStore uses `Arc<sled::Db>` internally, making it safe to clone and share across threads. Multiple threads can read and write concurrently.

#### `UploadedFile`

Metadata stored for each uploaded file.

```rust
pub struct UploadedFile {
    pub smugmug_uri: String,      // SmugMug API URI for the image
    pub album_key: String,         // Album where the image is stored
    pub image_key: String,         // Unique image identifier
    pub uploaded_at: DateTime<Utc>, // Upload timestamp
    pub file_size: u64,            // File size in bytes
    pub original_path: String,     // Original file path on disk
}
```

#### `calculate_file_hash(path: Path) -> Result<String>`

Calculates SHA256 hash of a file using a buffered reader (8KB buffer) for memory efficiency.

**Example:**

```rust
use smugmug_cli::cache::calculate_file_hash;

let hash = calculate_file_hash("/path/to/image.jpg")?;
println!("SHA256: {}", hash);
```

### `mod.rs`

Module exports and utility functions.

#### `get_cache_path() -> Result<PathBuf>`

Returns the platform-specific cache directory path using the `directories` crate.

**Locations:**
- Linux: `~/.cache/smugmug-cli/hash_store`
- macOS: `~/Library/Caches/com.jhofker.smugmug-cli/hash_store`
- Windows: `%LOCALAPPDATA%\jhofker\smugmug-cli\cache\hash_store`

#### `clear_cache() -> Result<()>`

Clears the deduplication cache. Used by the `smugmug-cli cache clear` command.

## Usage Example

```rust
use smugmug_cli::cache::{HashStore, UploadedFile, calculate_file_hash, get_cache_path};
use chrono::Utc;

// Initialize cache
let cache_path = get_cache_path()?;
let store = HashStore::new(cache_path.to_str().unwrap())?;

// Calculate hash of file
let hash = calculate_file_hash("/path/to/photo.jpg")?;

// Check if already uploaded
if let Some(cached) = store.get(&hash)? {
    println!("File already uploaded: {}", cached.smugmug_uri);
    return Ok(());
}

// Upload file (not shown)
// ...

// Store metadata after successful upload
let uploaded = UploadedFile {
    smugmug_uri: "https://api.smugmug.com/api/v2/image/ABC123-0".to_string(),
    album_key: "XYZ789".to_string(),
    image_key: "IMG456".to_string(),
    uploaded_at: Utc::now(),
    file_size: 1024000,
    original_path: "/path/to/photo.jpg".to_string(),
};

store.insert(&hash, uploaded)?;
```

## Concurrent Upload Example

```rust
use std::sync::Arc;
use tokio::sync::Mutex;

let store = Arc::new(HashStore::new(cache_path)?);

// Clone store for use in multiple async tasks
let store_clone = store.clone();

tokio::spawn(async move {
    // Check cache
    let hash = calculate_file_hash(file_path)?;
    if let Some(cached) = store_clone.get(&hash)? {
        println!("Skipping duplicate file");
        return Ok(());
    }

    // Upload and cache...
    Ok(())
});
```

## Database Details

- **Engine:** sled embedded database
- **Format:** Key-value store
- **Keys:** SHA256 hashes (64 hex characters)
- **Values:** JSON-serialized UploadedFile structs
- **Durability:** Changes are flushed to disk after each insert/clear operation

## Performance Considerations

1. **Hashing:** Uses buffered I/O with 8KB chunks to efficiently hash large files
2. **Database:** sled provides fast concurrent access without external dependencies
3. **Memory:** HashStore itself is lightweight; sled manages its own caching
4. **Disk Space:** Metadata is minimal (~200-300 bytes per file)

## Error Handling

All methods return `anyhow::Result` with context-enriched error messages for easier debugging.

Common errors:
- File not found during hashing
- Database corruption
- Insufficient permissions
- Disk full

## Testing

Run tests with:

```bash
cargo test --test cache_test
```

Or run the demo:

```bash
cargo run --example cache_demo
```
