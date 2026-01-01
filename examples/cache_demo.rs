use anyhow::Result;
use chrono::Utc;
use std::fs::File;
use std::io::Write;
use smugmug_cli::cache::hash_store::{HashStore, UploadedFile};
use smugmug_cli::uploader::calculate_file_hash;

fn main() -> Result<()> {
    println!("SmugMug CLI - Cache System Demo");
    println!("================================\n");

    // 1. Create a test file
    println!("1. Creating test file...");
    let test_file = std::env::temp_dir().join("demo_image.jpg");
    let mut file = File::create(&test_file)?;
    file.write_all(b"This is a demo image file content")?;
    drop(file);
    println!("   Created: {:?}\n", test_file);

    // 2. Calculate SHA256 hash
    println!("2. Calculating SHA256 hash...");
    let hash = calculate_file_hash(&test_file)?;
    println!("   Hash: {}\n", hash);

    // 3. Initialize HashStore
    println!("3. Initializing HashStore...");
    let cache_dir = std::env::temp_dir().join("smugmug_demo_cache");
    std::fs::create_dir_all(&cache_dir)?;
    let store = HashStore::new(cache_dir.to_str().unwrap())?;
    println!("   Cache directory: {:?}\n", cache_dir);

    // 4. Check if file is already uploaded (should be None)
    println!("4. Checking cache for file...");
    match store.get(&hash)? {
        Some(cached) => {
            println!("   Found in cache!");
            println!("   SmugMug URI: {}", cached.smugmug_uri);
            println!("   Uploaded at: {}\n", cached.uploaded_at);
        }
        None => {
            println!("   Not found in cache (first upload)\n");
        }
    }

    // 5. Simulate upload and cache the result
    println!("5. Simulating file upload...");
    let uploaded_file = UploadedFile {
        smugmug_uri: "https://api.smugmug.com/api/v2/image/ABC123-0".to_string(),
        album_key: "DEMO456".to_string(),
        image_key: "IMG789".to_string(),
        uploaded_at: Utc::now(),
        file_size: 34,
        original_path: test_file.to_str().unwrap().to_string(),
    };
    store.insert(&hash, uploaded_file)?;
    println!("   File cached successfully!\n");

    // 6. Check cache again
    println!("6. Checking cache again...");
    match store.get(&hash)? {
        Some(cached) => {
            println!("   Found in cache!");
            println!("   SmugMug URI: {}", cached.smugmug_uri);
            println!("   Album Key: {}", cached.album_key);
            println!("   Image Key: {}", cached.image_key);
            println!("   File Size: {} bytes", cached.file_size);
            println!("   Uploaded at: {}\n", cached.uploaded_at);
        }
        None => {
            println!("   ERROR: Should be in cache now!\n");
        }
    }

    // 7. Show cache statistics
    println!("7. Cache statistics:");
    let stats = store.stats()?;
    println!("   Total entries: {}", stats.total_entries);
    println!("   Total size: {} bytes\n", stats.total_size);

    // 8. Demonstrate deduplication
    println!("8. Testing deduplication...");
    println!("   Creating identical file with different name...");
    let test_file2 = std::env::temp_dir().join("duplicate_image.jpg");
    let mut file2 = File::create(&test_file2)?;
    file2.write_all(b"This is a demo image file content")?; // Same content
    drop(file2);

    let hash2 = calculate_file_hash(&test_file2)?;
    println!("   Hash of new file: {}", hash2);
    println!("   Hashes match: {}", hash == hash2);

    if hash == hash2 {
        println!("   This file would be skipped (already uploaded)!\n");
    }

    // 9. Clean up demo
    println!("9. Cleaning up...");
    store.clear()?;
    std::fs::remove_file(&test_file).ok();
    std::fs::remove_file(&test_file2).ok();
    std::fs::remove_dir_all(&cache_dir).ok();
    println!("   Done!\n");

    println!("================================");
    println!("Demo completed successfully!");

    Ok(())
}
