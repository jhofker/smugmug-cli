use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
        let db = sled::open(path).context("Failed to open sled database")?;

        Ok(HashStore { db: Arc::new(db) })
    }

    /// Get an uploaded file by its hash
    pub fn get(&self, hash: &str) -> Result<Option<UploadedFile>> {
        let value = self
            .db
            .get(hash.as_bytes())
            .context("Failed to read from database")?;

        match value {
            Some(bytes) => {
                let file: UploadedFile =
                    serde_json::from_slice(&bytes).context("Failed to deserialize UploadedFile")?;
                Ok(Some(file))
            }
            None => Ok(None),
        }
    }

    /// Insert a new uploaded file record
    pub fn insert(&self, hash: &str, file: UploadedFile) -> Result<()> {
        let serialized = serde_json::to_vec(&file).context("Failed to serialize UploadedFile")?;

        self.db
            .insert(hash.as_bytes(), serialized)
            .context("Failed to insert into database")?;

        self.db.flush().context("Failed to flush database")?;

        Ok(())
    }

    /// Forget one file, e.g. when the image it points to no longer exists
    pub fn remove(&self, hash: &str) -> Result<()> {
        self.db
            .remove(hash.as_bytes())
            .context("Failed to remove from database")?;
        self.db.flush().context("Failed to flush database")?;
        Ok(())
    }

    /// Clear all entries from the cache
    pub fn clear(&self) -> Result<()> {
        self.db.clear().context("Failed to clear database")?;

        self.db.flush().context("Failed to flush database")?;

        Ok(())
    }

    /// Get cache statistics
    pub fn stats(&self) -> Result<CacheStats> {
        let mut total_entries = 0;
        let mut total_size = 0u64;
        let mut oldest: Option<DateTime<Utc>> = None;
        let mut newest: Option<DateTime<Utc>> = None;

        for item in self.db.iter() {
            let (_key, value) = item.context("Failed to read database entry")?;
            total_entries += 1;

            // Deserialize to get file size and timestamp
            if let Ok(file) = serde_json::from_slice::<UploadedFile>(&value) {
                total_size += file.file_size;
                let timestamp = file.uploaded_at;

                oldest = Some(oldest.map_or(timestamp, |old| old.min(timestamp)));
                newest = Some(newest.map_or(timestamp, |new| new.max(timestamp)));
            }
        }

        Ok(CacheStats {
            total_entries,
            total_size,
            oldest_entry: oldest,
            newest_entry: newest,
        })
    }

    /// Get the total number of entries in the cache
    #[allow(dead_code)]
    pub fn count(&self) -> Result<usize> {
        let mut count = 0;
        for item in self.db.iter() {
            item.context("Failed to read database entry")?;
            count += 1;
        }
        Ok(count)
    }

    /// Get the total size of cached data in bytes (sum of file_size fields)
    #[allow(dead_code)]
    pub fn size(&self) -> Result<u64> {
        let mut total_size = 0u64;
        for item in self.db.iter() {
            let (_key, value) = item.context("Failed to read database entry")?;
            if let Ok(file) = serde_json::from_slice::<UploadedFile>(&value) {
                total_size += file.file_size;
            }
        }
        Ok(total_size)
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

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_size: u64,
    pub oldest_entry: Option<DateTime<Utc>>,
    pub newest_entry: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn temp_dir() -> String {
        let dir = tempfile::tempdir().unwrap();
        dir.path().to_str().unwrap().to_string()
    }

    fn create_test_file(timestamp: DateTime<Utc>) -> UploadedFile {
        UploadedFile {
            smugmug_uri: "https://api.smugmug.com/api/v2/image/test".to_string(),
            album_key: "album123".to_string(),
            image_key: "img456".to_string(),
            uploaded_at: timestamp,
            file_size: 1024,
            original_path: "/test/path.jpg".to_string(),
        }
    }

    #[test]
    fn test_new_store() {
        let dir = temp_dir();
        let store = HashStore::new(&dir);
        assert!(store.is_ok());
    }

    #[test]
    fn test_insert_and_get() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let file = create_test_file(Utc::now());
        let hash = "abc123";

        // Insert
        store.insert(hash, file.clone()).unwrap();

        // Retrieve
        let retrieved = store.get(hash).unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.smugmug_uri, file.smugmug_uri);
        assert_eq!(retrieved.album_key, file.album_key);
        assert_eq!(retrieved.image_key, file.image_key);
    }

    #[test]
    fn test_get_nonexistent() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let result = store.get("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_clear() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        // Insert multiple entries
        for i in 0..5 {
            let hash = format!("hash{}", i);
            let file = create_test_file(Utc::now());
            store.insert(&hash, file).unwrap();
        }

        // Verify entries exist
        let count_before = store.count().unwrap();
        assert_eq!(count_before, 5);

        // Clear
        store.clear().unwrap();

        // Verify cleared
        let count_after = store.count().unwrap();
        assert_eq!(count_after, 0);
    }

    #[test]
    fn test_count() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        // Empty store
        assert_eq!(store.count().unwrap(), 0);

        // Add entries
        for i in 0..10 {
            let hash = format!("hash{}", i);
            let file = create_test_file(Utc::now());
            store.insert(&hash, file).unwrap();
        }

        assert_eq!(store.count().unwrap(), 10);
    }

    #[test]
    fn test_size() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        // Empty store
        assert_eq!(store.size().unwrap(), 0);

        // Add entries
        let file = create_test_file(Utc::now());
        store.insert("hash1", file).unwrap();

        // Size should be greater than 0
        let size = store.size().unwrap();
        assert!(size > 0);
    }

    #[test]
    fn test_stats_empty() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.total_size, 0);
        assert!(stats.oldest_entry.is_none());
        assert!(stats.newest_entry.is_none());
    }

    #[test]
    fn test_stats_single_entry() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let timestamp = Utc.with_ymd_and_hms(2024, 3, 15, 12, 0, 0).unwrap();
        let file = create_test_file(timestamp);
        store.insert("hash1", file).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_entries, 1);
        assert!(stats.total_size > 0);
        assert_eq!(stats.oldest_entry, Some(timestamp));
        assert_eq!(stats.newest_entry, Some(timestamp));
    }

    #[test]
    fn test_stats_multiple_entries() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        // Create entries with different timestamps
        let oldest = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let middle = Utc.with_ymd_and_hms(2024, 2, 15, 12, 0, 0).unwrap();
        let newest = Utc.with_ymd_and_hms(2024, 3, 15, 12, 0, 0).unwrap();

        store.insert("hash1", create_test_file(middle)).unwrap();
        store.insert("hash2", create_test_file(oldest)).unwrap();
        store.insert("hash3", create_test_file(newest)).unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_entries, 3);
        assert!(stats.total_size > 0);
        assert_eq!(stats.oldest_entry, Some(oldest));
        assert_eq!(stats.newest_entry, Some(newest));
    }

    #[test]
    fn test_stats_size_calculation() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        // Insert one entry
        let file1 = create_test_file(Utc::now());
        store.insert("hash1", file1).unwrap();
        let size1 = store.stats().unwrap().total_size;

        // Insert another entry
        let file2 = create_test_file(Utc::now());
        store.insert("hash2", file2).unwrap();
        let size2 = store.stats().unwrap().total_size;

        // Size should have increased
        assert!(size2 > size1);
    }

    #[test]
    fn test_clone() {
        let dir = temp_dir();
        let store1 = HashStore::new(&dir).unwrap();

        let file = create_test_file(Utc::now());
        store1.insert("hash1", file).unwrap();

        // Clone the store
        let store2 = store1.clone();

        // Both should be able to read the same data
        assert!(store1.get("hash1").unwrap().is_some());
        assert!(store2.get("hash1").unwrap().is_some());
        assert_eq!(store1.count().unwrap(), store2.count().unwrap());
    }

    #[test]
    fn test_thread_safety() {
        use std::thread;

        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let store1 = store.clone();
        let store2 = store.clone();

        let handle1 = thread::spawn(move || {
            for i in 0..50 {
                let hash = format!("thread1_hash{}", i);
                let file = create_test_file(Utc::now());
                store1.insert(&hash, file).unwrap();
            }
        });

        let handle2 = thread::spawn(move || {
            for i in 0..50 {
                let hash = format!("thread2_hash{}", i);
                let file = create_test_file(Utc::now());
                store2.insert(&hash, file).unwrap();
            }
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        // Should have 100 entries total
        assert_eq!(store.count().unwrap(), 100);
    }

    #[test]
    fn test_update_existing_entry() {
        let dir = temp_dir();
        let store = HashStore::new(&dir).unwrap();

        let hash = "hash1";
        let file1 = create_test_file(Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap());
        let file2 = create_test_file(Utc.with_ymd_and_hms(2024, 2, 1, 12, 0, 0).unwrap());

        // Insert first version
        store.insert(hash, file1).unwrap();
        let retrieved1 = store.get(hash).unwrap().unwrap();
        assert_eq!(
            retrieved1.uploaded_at,
            Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap()
        );

        // Update with second version
        store.insert(hash, file2).unwrap();
        let retrieved2 = store.get(hash).unwrap().unwrap();
        assert_eq!(
            retrieved2.uploaded_at,
            Utc.with_ymd_and_hms(2024, 2, 1, 12, 0, 0).unwrap()
        );

        // Should still have only 1 entry
        assert_eq!(store.count().unwrap(), 1);
    }
}
