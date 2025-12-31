use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use sha2::{Sha256, Digest};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UploadedFile {
    pub smugmug_uri: String,
    pub album_key: String,
    pub image_key: String,
    pub uploaded_at: DateTime<Utc>,
    pub file_size: u64,
    pub original_path: String,
}

/// Thread-safe hash store using sled embedded database
pub struct HashStore {
    db: Arc<sled::Db>,
}

impl HashStore {
    /// Create a new HashStore at the specified path
    pub fn new(path: &str) -> Result<Self> {
        let db = sled::open(path)
            .context("Failed to open sled database")?;

        Ok(HashStore {
            db: Arc::new(db),
        })
    }

    /// Get an uploaded file by its hash
    pub fn get(&self, hash: &str) -> Result<Option<UploadedFile>> {
        let value = self.db.get(hash.as_bytes())
            .context("Failed to read from database")?;

        match value {
            Some(bytes) => {
                let file: UploadedFile = serde_json::from_slice(&bytes)
                    .context("Failed to deserialize UploadedFile")?;
                Ok(Some(file))
            }
            None => Ok(None),
        }
    }

    /// Insert a new uploaded file record
    pub fn insert(&self, hash: &str, file: UploadedFile) -> Result<()> {
        let serialized = serde_json::to_vec(&file)
            .context("Failed to serialize UploadedFile")?;

        self.db.insert(hash.as_bytes(), serialized)
            .context("Failed to insert into database")?;

        self.db.flush()
            .context("Failed to flush database")?;

        Ok(())
    }

    /// Clear all entries from the cache
    pub fn clear(&self) -> Result<()> {
        self.db.clear()
            .context("Failed to clear database")?;

        self.db.flush()
            .context("Failed to flush database")?;

        Ok(())
    }

    /// Get cache statistics
    pub fn stats(&self) -> Result<CacheStats> {
        let mut total_entries = 0;
        let mut total_size = 0u64;

        for item in self.db.iter() {
            let (_key, value) = item.context("Failed to read database entry")?;

            let file: UploadedFile = serde_json::from_slice(&value)
                .context("Failed to deserialize UploadedFile")?;

            total_entries += 1;
            total_size += file.file_size;
        }

        Ok(CacheStats {
            total_entries,
            total_size,
        })
    }
}

// Implement Clone for HashStore since it uses Arc internally
impl Clone for HashStore {
    fn clone(&self) -> Self {
        HashStore {
            db: Arc::clone(&self.db),
        }
    }
}

#[derive(Debug)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_size: u64,
}

/// Calculate SHA256 hash of a file
pub fn calculate_file_hash<P: AsRef<Path>>(path: P) -> Result<String> {
    let file = File::open(path.as_ref())
        .context("Failed to open file for hashing")?;

    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192]; // 8KB buffer

    loop {
        let bytes_read = reader.read(&mut buffer)
            .context("Failed to read file during hashing")?;

        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}
