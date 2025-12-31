use anyhow::Result;
use chrono::Utc;
use std::fs::File;
use std::io::Write;
use smugmug_cli::cache::{HashStore, UploadedFile, calculate_file_hash};

#[test]
fn test_hash_calculation() -> Result<()> {
    // Create a test file
    let test_file = std::env::temp_dir().join("test_hash_calc.txt");
    let mut file = File::create(&test_file)?;
    file.write_all(b"Test content for hashing")?;
    drop(file);

    // Calculate hash
    let hash = calculate_file_hash(&test_file)?;

    // Verify it's a valid SHA256 hash (64 hex characters)
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

    // Calculate again to verify consistency
    let hash2 = calculate_file_hash(&test_file)?;
    assert_eq!(hash, hash2);

    // Cleanup
    std::fs::remove_file(&test_file).ok();

    Ok(())
}

#[test]
fn test_hash_store_operations() -> Result<()> {
    // Create a temporary cache directory
    let cache_dir = std::env::temp_dir().join(format!("smugmug_test_{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir)?;

    // Initialize store
    let store = HashStore::new(cache_dir.to_str().unwrap())?;

    // Test hash
    let test_hash = "a".repeat(64); // Valid SHA256 format

    // Should not exist initially
    let result = store.get(&test_hash)?;
    assert!(result.is_none());

    // Insert a record
    let uploaded_file = UploadedFile {
        smugmug_uri: "https://api.smugmug.com/api/v2/image/12345".to_string(),
        album_key: "TEST123".to_string(),
        image_key: "IMG456".to_string(),
        uploaded_at: Utc::now(),
        file_size: 1024,
        original_path: "/tmp/test.jpg".to_string(),
    };

    store.insert(&test_hash, uploaded_file.clone())?;

    // Should exist now
    let retrieved = store.get(&test_hash)?;
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.smugmug_uri, uploaded_file.smugmug_uri);
    assert_eq!(retrieved.album_key, uploaded_file.album_key);
    assert_eq!(retrieved.image_key, uploaded_file.image_key);
    assert_eq!(retrieved.file_size, uploaded_file.file_size);

    // Cleanup
    std::fs::remove_dir_all(&cache_dir).ok();

    Ok(())
}

#[test]
fn test_cache_stats() -> Result<()> {
    // Create a temporary cache directory
    let cache_dir = std::env::temp_dir().join(format!("smugmug_test_stats_{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir)?;

    let store = HashStore::new(cache_dir.to_str().unwrap())?;

    // Initially empty
    let stats = store.stats()?;
    assert_eq!(stats.total_entries, 0);
    assert_eq!(stats.total_size, 0);

    // Insert multiple records
    for i in 0..5 {
        let hash = format!("{:0>64}", i); // Pad with zeros to make 64 chars
        let file = UploadedFile {
            smugmug_uri: format!("https://api.smugmug.com/api/v2/image/{}", i),
            album_key: format!("ALBUM{}", i),
            image_key: format!("IMG{}", i),
            uploaded_at: Utc::now(),
            file_size: (i + 1) * 100,
            original_path: format!("/tmp/test{}.jpg", i),
        };
        store.insert(&hash, file)?;
    }

    // Check stats
    let stats = store.stats()?;
    assert_eq!(stats.total_entries, 5);
    assert_eq!(stats.total_size, 100 + 200 + 300 + 400 + 500);

    // Cleanup
    std::fs::remove_dir_all(&cache_dir).ok();

    Ok(())
}

#[test]
fn test_cache_clear() -> Result<()> {
    // Create a temporary cache directory
    let cache_dir = std::env::temp_dir().join(format!("smugmug_test_clear_{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir)?;

    let store = HashStore::new(cache_dir.to_str().unwrap())?;

    // Insert a record
    let hash = "b".repeat(64);
    let file = UploadedFile {
        smugmug_uri: "https://api.smugmug.com/api/v2/image/test".to_string(),
        album_key: "CLEAR".to_string(),
        image_key: "TEST".to_string(),
        uploaded_at: Utc::now(),
        file_size: 500,
        original_path: "/tmp/clear_test.jpg".to_string(),
    };
    store.insert(&hash, file)?;

    // Verify it exists
    let stats = store.stats()?;
    assert_eq!(stats.total_entries, 1);

    // Clear cache
    store.clear()?;

    // Verify it's empty
    let stats = store.stats()?;
    assert_eq!(stats.total_entries, 0);

    let result = store.get(&hash)?;
    assert!(result.is_none());

    // Cleanup
    std::fs::remove_dir_all(&cache_dir).ok();

    Ok(())
}

#[test]
fn test_thread_safety() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    // Create a temporary cache directory
    let cache_dir = std::env::temp_dir().join(format!("smugmug_test_threads_{}", std::process::id()));
    std::fs::create_dir_all(&cache_dir)?;

    let store = Arc::new(HashStore::new(cache_dir.to_str().unwrap())?);

    let mut handles = vec![];

    // Spawn multiple threads that insert records
    for i in 0..5 {
        let store_clone = Arc::clone(&store);
        let handle = thread::spawn(move || -> Result<()> {
            let hash = format!("{:0>64}", i);
            let file = UploadedFile {
                smugmug_uri: format!("https://api.smugmug.com/api/v2/image/{}", i),
                album_key: format!("THREAD{}", i),
                image_key: format!("IMG{}", i),
                uploaded_at: Utc::now(),
                file_size: (i + 1) * 100,
                original_path: format!("/tmp/thread{}.jpg", i),
            };
            store_clone.insert(&hash, file)?;
            Ok(())
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap()?;
    }

    // Verify all records were inserted
    let stats = store.stats()?;
    assert_eq!(stats.total_entries, 5);

    // Cleanup
    std::fs::remove_dir_all(&cache_dir).ok();

    Ok(())
}
