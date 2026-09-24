use anyhow::{Context as AnyhowContext, Result};
use chrono::Utc;
use md5::Context as Md5Context;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::api::SmugMugClient;
use crate::api::images::AlbumImage;
use crate::api::upload::{replace_image, upload_image};
use crate::cache::hash_store::{HashStore, UploadedFile};
use crate::uploader::album_series::{AlbumSeries, ClientAlbumSeries};

pub struct UploadWorkerContext {
    pub client: Arc<SmugMugClient>,
    /// Target album for new images, unless `series` is set.
    pub album_uri: String,
    pub album_key: String,
    /// When set, each new image claims room in this album series instead of
    /// going to `album_uri` (see `album_series`).
    pub series: Option<Arc<AlbumSeries<ClientAlbumSeries>>>,
    pub hash_store: Arc<Mutex<HashStore>>,
    pub remote_md5s: Option<Arc<std::collections::HashMap<String, String>>>,
    /// Existing images already in the target album, keyed by filename. Used
    /// to decide, per file, whether to skip (content unchanged), replace
    /// (same filename, different content) or create (new filename).
    pub remote_images: Option<Arc<std::collections::HashMap<String, AlbumImage>>>,
    pub dry_run: bool,
    pub no_cache: bool,
    pub retry_attempts: u32,
    pub skip_raw_files: Arc<std::sync::atomic::AtomicBool>,
}

/// Determines if an error is retryable (transient) or permanent
fn is_retryable_error(error: &anyhow::Error) -> bool {
    let error_msg = error.to_string().to_lowercase();

    // Retry on network errors, timeouts, and 5xx server errors
    error_msg.contains("timeout")
        || error_msg.contains("connection")
        || error_msg.contains("network")
        || error_msg.contains("500")
        || error_msg.contains("502")
        || error_msg.contains("503")
        || error_msg.contains("504")
        || error_msg.contains("429") // Rate limiting
}

pub async fn upload_worker(
    file_path: &Path,
    context: Arc<UploadWorkerContext>,
) -> Result<UploadStatus> {
    // Check if this is a RAW file and RAW uploads are disabled
    let is_raw = crate::scanner::is_raw_file(file_path);
    if is_raw
        && context
            .skip_raw_files
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(UploadStatus::Skipped);
    }

    // Calculate file hash (SHA256 for local cache)
    let hash = calculate_file_hash(file_path).with_context(|| "Failed to calculate file hash")?;

    // Check local cache first (unless no_cache is enabled)
    if !context.no_cache {
        let store = context.hash_store.lock().await;
        if let Some(_cached) = store.get(&hash)? {
            return Ok(UploadStatus::Skipped);
        }
    }

    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    // MD5 is needed for both the optional --check-remote scan and the
    // default same-filename skip/replace comparison below.
    let needs_md5 = context.remote_md5s.is_some() || context.remote_images.is_some();
    let md5_hash = if needs_md5 {
        Some(calculate_md5_hash(file_path).with_context(|| "Failed to calculate MD5 hash")?)
    } else {
        None
    };

    // Check remote MD5s if --check-remote is enabled (matches content
    // anywhere in the album, regardless of filename)
    if let (Some(remote_md5s), Some(md5_hash)) = (&context.remote_md5s, &md5_hash) {
        if remote_md5s.contains_key(&md5_hash.to_lowercase()) {
            return Ok(UploadStatus::Skipped);
        }
    }

    // If an image with this filename already exists in the album, either
    // skip (content is unchanged) or replace it in place (content differs)
    // instead of attempting a fresh create, which SmugMug would reject with
    // 409 Conflict for a duplicate filename.
    let mut replace_target: Option<String> = None;
    if let (Some(remote_images), Some(filename), Some(md5_hash)) =
        (&context.remote_images, &filename, &md5_hash)
    {
        if let Some(remote_image) = remote_images.get(filename) {
            let remote_md5_matches = remote_image
                .archived_md5
                .as_deref()
                .map(|m| m.eq_ignore_ascii_case(md5_hash))
                .unwrap_or(false);

            if remote_md5_matches {
                return Ok(UploadStatus::Skipped);
            }

            replace_target = Some(remote_image.uri.clone());
        }
    }

    // Get file size
    let file_size = std::fs::metadata(file_path)
        .with_context(|| "Failed to get file metadata")?
        .len();

    // A new image in an album series claims room in the first album that has
    // some, which may create that album. This happens only now, once the file
    // is known to need uploading, so skipped files never use up space. On a
    // dry run the claim is still made (without creating anything) so the
    // summary shows where files would go.
    let (album_uri, album_key, claimed) = match (&context.series, &replace_target) {
        (Some(series), None) => {
            let album = series.claim().await?;
            (album.uri.clone(), album.album_key.clone(), Some(album))
        }
        _ => (context.album_uri.clone(), context.album_key.clone(), None),
    };

    // Skip actual upload if dry run
    if context.dry_run {
        return Ok(UploadStatus::DryRun { file_size });
    }

    let result = upload_with_retries(
        &context,
        file_path,
        &hash,
        file_size,
        is_raw,
        replace_target.as_deref(),
        &album_uri,
        &album_key,
    )
    .await;

    // A failed upload gives its claimed room back.
    if let (Err(_), Some(album), Some(series)) = (&result, &claimed, &context.series) {
        series.release(album).await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn upload_with_retries(
    context: &UploadWorkerContext,
    file_path: &Path,
    hash: &str,
    file_size: u64,
    is_raw: bool,
    replace_target: Option<&str>,
    album_uri: &str,
    album_key: &str,
) -> Result<UploadStatus> {
    // Upload file with retry logic
    let mut last_error = None;
    let max_attempts = context.retry_attempts.max(1); // At least 1 attempt

    for attempt in 1..=max_attempts {
        let upload_attempt = match replace_target {
            Some(image_uri) => replace_image(&context.client, image_uri, file_path).await,
            None => upload_image(&context.client, album_uri, file_path).await,
        };

        match upload_attempt {
            Ok(upload_result) => {
                // Success! Update cache and return
                let uploaded_file = UploadedFile {
                    smugmug_uri: upload_result.image_uri.clone(),
                    album_key: album_key.to_string(),
                    image_key: upload_result.image_key.clone(),
                    uploaded_at: Utc::now(),
                    file_size,
                    original_path: file_path.to_string_lossy().to_string(),
                };

                {
                    let store = context.hash_store.lock().await;
                    store.insert(hash, uploaded_file)?;
                }

                return Ok(if replace_target.is_some() {
                    UploadStatus::Replaced { file_size }
                } else {
                    UploadStatus::Uploaded { file_size }
                });
            }
            Err(e) => {
                let error = e.context("Failed to upload image");

                // If this is a RAW file and upload failed, disable future RAW uploads
                if is_raw {
                    let was_already_set = context
                        .skip_raw_files
                        .swap(true, std::sync::atomic::Ordering::Relaxed);
                    if !was_already_set {
                        eprintln!("\n⚠️  RAW file upload failed. Skipping remaining RAW files.");
                        eprintln!("   (RAW files require a SmugMug Source subscription)");
                    }
                }

                // Check if we should retry
                if attempt < max_attempts && is_retryable_error(&error) {
                    // Calculate exponential backoff: 1s, 2s, 4s, 8s...
                    let delay_secs = 2u64.pow(attempt - 1);
                    tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)).await;
                    last_error = Some(error);
                } else {
                    // Don't retry - either last attempt or non-retryable error
                    return Err(error);
                }
            }
        }
    }

    // If we get here, all retries failed
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Upload failed after retries")))
}

pub enum UploadStatus {
    Uploaded {
        file_size: u64,
    },
    /// An existing image with the same filename was updated in place
    /// because its content had changed.
    Replaced {
        file_size: u64,
    },
    Skipped,
    DryRun {
        file_size: u64,
    },
}

pub fn calculate_file_hash(file_path: &Path) -> Result<String> {
    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hex::encode(hasher.finalize()))
}

fn calculate_md5_hash(file_path: &Path) -> Result<String> {
    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);
    let mut context = Md5Context::new();
    let mut buffer = [0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        context.consume(&buffer[..count]);
    }

    Ok(format!("{:x}", context.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Tests for worker functions: hash calculation, deduplication, and upload status
    // All tests use temporary files and avoid actual API calls

    // Helper to create a temporary test file with content
    fn create_test_file(content: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn test_calculate_file_hash() {
        let content = b"test content for hashing";
        let file = create_test_file(content);

        let hash = calculate_file_hash(file.path()).unwrap();

        // Verify hash is valid SHA256 format (64 hex characters)
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify hash is consistent
        let hash2 = calculate_file_hash(file.path()).unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_calculate_file_hash_empty_file() {
        let file = create_test_file(b"");
        let hash = calculate_file_hash(file.path()).unwrap();

        // SHA256 of empty file
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_calculate_file_hash_large_file() {
        // Create a file larger than buffer size (8192 bytes)
        let content: Vec<u8> = (0..20000).map(|i| (i % 256) as u8).collect();
        let file = create_test_file(&content);

        let hash = calculate_file_hash(file.path()).unwrap();
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_calculate_md5_hash() {
        let content = b"test content for md5";
        let file = create_test_file(content);

        let hash = calculate_md5_hash(file.path()).unwrap();

        // Verify hash is valid MD5 format (32 hex characters)
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify hash is consistent
        let hash2 = calculate_md5_hash(file.path()).unwrap();
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_calculate_md5_hash_empty_file() {
        let file = create_test_file(b"");
        let hash = calculate_md5_hash(file.path()).unwrap();

        // MD5 of empty file
        assert_eq!(hash, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_different_content_different_hashes() {
        let file1 = create_test_file(b"content1");
        let file2 = create_test_file(b"content2");

        let hash1 = calculate_file_hash(file1.path()).unwrap();
        let hash2 = calculate_file_hash(file2.path()).unwrap();

        assert_ne!(hash1, hash2);

        let md5_1 = calculate_md5_hash(file1.path()).unwrap();
        let md5_2 = calculate_md5_hash(file2.path()).unwrap();

        assert_ne!(md5_1, md5_2);
    }

    #[tokio::test]
    async fn test_upload_worker_dry_run() {
        let file = create_test_file(b"test image content");
        let temp_dir = tempfile::tempdir().unwrap();

        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let context = Arc::new(UploadWorkerContext {
            client,
            album_uri: "/api/v2/album/test".to_string(),
            album_key: "test_album".to_string(),
            series: None,
            hash_store,
            remote_md5s: None,
            remote_images: None,
            dry_run: true,
            no_cache: false,
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        let result = upload_worker(file.path(), context).await.unwrap();

        match result {
            UploadStatus::DryRun { file_size } => {
                assert!(file_size > 0);
            }
            _ => panic!("Expected DryRun status"),
        }
    }

    #[tokio::test]
    async fn test_upload_worker_skipped_local_cache() {
        let file = create_test_file(b"cached content");
        let temp_dir = tempfile::tempdir().unwrap();

        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        // Pre-populate cache
        let hash = calculate_file_hash(file.path()).unwrap();
        let uploaded_file = UploadedFile {
            smugmug_uri: "/api/v2/image/test".to_string(),
            album_key: "test_album".to_string(),
            image_key: "test_key".to_string(),
            uploaded_at: Utc::now(),
            file_size: 100,
            original_path: file.path().to_string_lossy().to_string(),
        };
        hash_store
            .lock()
            .await
            .insert(&hash, uploaded_file)
            .unwrap();

        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let context = Arc::new(UploadWorkerContext {
            client,
            album_uri: "/api/v2/album/test".to_string(),
            album_key: "test_album".to_string(),
            series: None,
            hash_store,
            remote_md5s: None,
            remote_images: None,
            dry_run: false,
            no_cache: false,
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        let result = upload_worker(file.path(), context).await.unwrap();

        match result {
            UploadStatus::Skipped => {
                // Success - file was skipped due to local cache
            }
            _ => panic!("Expected Skipped status due to local cache"),
        }
    }

    #[tokio::test]
    async fn test_upload_worker_skipped_remote_md5() {
        let file = create_test_file(b"remote content");
        let temp_dir = tempfile::tempdir().unwrap();

        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        // Calculate MD5 and add to remote map
        let md5 = calculate_md5_hash(file.path()).unwrap();
        let mut remote_md5s = HashMap::new();
        remote_md5s.insert(md5.to_lowercase(), "remote_image_key".to_string());

        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let context = Arc::new(UploadWorkerContext {
            client,
            album_uri: "/api/v2/album/test".to_string(),
            album_key: "test_album".to_string(),
            series: None,
            hash_store,
            remote_md5s: Some(Arc::new(remote_md5s)),
            remote_images: None,
            dry_run: false,
            no_cache: false,
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        let result = upload_worker(file.path(), context).await.unwrap();

        match result {
            UploadStatus::Skipped => {
                // Success - file was skipped due to remote MD5 match
            }
            _ => panic!("Expected Skipped status due to remote MD5"),
        }
    }

    #[tokio::test]
    async fn test_upload_worker_skipped_same_filename_matching_md5() {
        let file = create_test_file(b"unchanged content");
        let temp_dir = tempfile::tempdir().unwrap();

        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        let filename = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let md5 = calculate_md5_hash(file.path()).unwrap();

        let mut remote_images = HashMap::new();
        remote_images.insert(
            filename,
            AlbumImage {
                image_key: "EXISTING123".to_string(),
                file_name: file
                    .path()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
                archived_uri: "https://example.com/existing.jpg".to_string(),
                file_size: 100,
                format: "JPG".to_string(),
                uri: "/api/v2/image/EXISTING123".to_string(),
                title: None,
                archived_md5: Some(md5),
            },
        );

        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let context = Arc::new(UploadWorkerContext {
            client,
            album_uri: "/api/v2/album/test".to_string(),
            album_key: "test_album".to_string(),
            series: None,
            hash_store,
            remote_md5s: None,
            remote_images: Some(Arc::new(remote_images)),
            dry_run: false,
            no_cache: false,
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        // No local cache entry, but the remote image with the same filename
        // has matching content, so no upload/replace call should be made.
        let result = upload_worker(file.path(), context).await.unwrap();

        match result {
            UploadStatus::Skipped => {}
            _ => panic!("Expected Skipped status because remote content is unchanged"),
        }
    }

    #[tokio::test]
    async fn test_upload_worker_no_cache_flag() {
        let file = create_test_file(b"no cache test");
        let temp_dir = tempfile::tempdir().unwrap();

        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        // Pre-populate cache
        let hash = calculate_file_hash(file.path()).unwrap();
        let uploaded_file = UploadedFile {
            smugmug_uri: "/api/v2/image/test".to_string(),
            album_key: "test_album".to_string(),
            image_key: "test_key".to_string(),
            uploaded_at: Utc::now(),
            file_size: 100,
            original_path: file.path().to_string_lossy().to_string(),
        };
        hash_store
            .lock()
            .await
            .insert(&hash, uploaded_file)
            .unwrap();

        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let context = Arc::new(UploadWorkerContext {
            client,
            album_uri: "/api/v2/album/test".to_string(),
            album_key: "test_album".to_string(),
            series: None,
            hash_store,
            remote_md5s: None,
            remote_images: None,
            dry_run: true,  // Use dry run to avoid actual upload
            no_cache: true, // This should bypass local cache
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        let result = upload_worker(file.path(), context).await.unwrap();

        // With no_cache=true, should not be skipped even though in cache
        match result {
            UploadStatus::DryRun { .. } => {
                // Success - cache was bypassed
            }
            _ => panic!("Expected DryRun status (cache should be bypassed)"),
        }
    }

    #[test]
    fn test_upload_status_variants() {
        let uploaded = UploadStatus::Uploaded { file_size: 1024 };
        let skipped = UploadStatus::Skipped;
        let dry_run = UploadStatus::DryRun { file_size: 2048 };

        match uploaded {
            UploadStatus::Uploaded { file_size } => assert_eq!(file_size, 1024),
            _ => panic!("Wrong variant"),
        }

        match skipped {
            UploadStatus::Skipped => {}
            _ => panic!("Wrong variant"),
        }

        match dry_run {
            UploadStatus::DryRun { file_size } => assert_eq!(file_size, 2048),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_is_retryable_error_timeout() {
        let error = anyhow::anyhow!("Connection timeout occurred");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_connection() {
        let error = anyhow::anyhow!("Connection refused");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_network() {
        let error = anyhow::anyhow!("Network error encountered");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_500() {
        let error = anyhow::anyhow!("Server returned 500 Internal Server Error");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_502() {
        let error = anyhow::anyhow!("502 Bad Gateway");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_503() {
        let error = anyhow::anyhow!("503 Service Unavailable");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_504() {
        let error = anyhow::anyhow!("504 Gateway Timeout");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_retryable_error_rate_limit() {
        let error = anyhow::anyhow!("429 Too Many Requests");
        assert!(is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_auth() {
        let error = anyhow::anyhow!("401 Unauthorized");
        assert!(!is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_not_found() {
        let error = anyhow::anyhow!("404 Not Found");
        assert!(!is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_validation() {
        let error = anyhow::anyhow!("Invalid input: file too large");
        assert!(!is_retryable_error(&error));
    }

    #[test]
    fn test_is_not_retryable_error_forbidden() {
        let error = anyhow::anyhow!("403 Forbidden");
        assert!(!is_retryable_error(&error));
    }

    #[tokio::test]
    async fn test_upload_worker_context_creation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let hash_store = Arc::new(Mutex::new(
            HashStore::new(temp_dir.path().to_str().unwrap()).unwrap(),
        ));

        let client = Arc::new(SmugMugClient::new(
            "key".to_string(),
            "secret".to_string(),
            "token".to_string(),
            "token_secret".to_string(),
        ));

        let remote_md5s = HashMap::new();

        let context = UploadWorkerContext {
            client: client.clone(),
            album_uri: "/api/v2/album/ABC123".to_string(),
            album_key: "ABC123".to_string(),
            series: None,
            hash_store: hash_store.clone(),
            remote_md5s: Some(Arc::new(remote_md5s)),
            remote_images: None,
            dry_run: false,
            no_cache: true,
            retry_attempts: 3,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        assert_eq!(context.album_uri, "/api/v2/album/ABC123");
        assert_eq!(context.album_key, "ABC123");
        assert!(!context.dry_run);
        assert!(context.no_cache);
        assert!(context.remote_md5s.is_some());
    }
}
