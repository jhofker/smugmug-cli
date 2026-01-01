use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
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

        for item in self.db.iter() {
            let (_key, _value) = item.context("Failed to read database entry")?;
            total_entries += 1;
        }

        Ok(CacheStats {
            total_entries,
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
}
