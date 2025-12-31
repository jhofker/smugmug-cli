use anyhow::{Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::api::upload::upload_image;
use crate::api::SmugMugClient;
use crate::cache::hash_store::{HashStore, UploadedFile};

pub struct UploadWorkerContext {
    pub client: Arc<SmugMugClient>,
    pub album_uri: String,
    pub album_key: String,
    pub hash_store: Arc<Mutex<HashStore>>,
    pub dry_run: bool,
}

pub async fn upload_worker(
    file_path: &Path,
    context: Arc<UploadWorkerContext>,
) -> Result<UploadStatus> {
    // Calculate file hash
    let hash = calculate_file_hash(file_path)
        .context("Failed to calculate file hash")?;

    // Check cache
    {
        let store = context.hash_store.lock().await;
        if let Some(cached) = store.get(&hash)? {
            return Ok(UploadStatus::Skipped {
                reason: format!("Already uploaded: {}", cached.smugmug_uri),
                hash,
            });
        }
    }

    // Get file size
    let file_size = std::fs::metadata(file_path)
        .context("Failed to get file metadata")?
        .len();

    // Skip actual upload if dry run
    if context.dry_run {
        return Ok(UploadStatus::DryRun { hash, file_size });
    }

    // Upload file
    let upload_result = upload_image(
        &context.client,
        &context.album_uri,
        file_path,
    )
    .await
    .context("Failed to upload image")?;

    // Update cache
    let uploaded_file = UploadedFile {
        smugmug_uri: upload_result.image_uri.clone(),
        album_key: context.album_key.clone(),
        image_key: upload_result.image_key.clone(),
        uploaded_at: Utc::now(),
        file_size,
        original_path: file_path.to_string_lossy().to_string(),
    };

    {
        let store = context.hash_store.lock().await;
        store.insert(&hash, uploaded_file)?;
    }

    Ok(UploadStatus::Uploaded {
        hash,
        image_key: upload_result.image_key,
        image_uri: upload_result.image_uri,
        file_size,
    })
}

pub enum UploadStatus {
    Uploaded {
        hash: String,
        image_key: String,
        image_uri: String,
        file_size: u64,
    },
    Skipped {
        reason: String,
        hash: String,
    },
    DryRun {
        hash: String,
        file_size: u64,
    },
}

fn calculate_file_hash(file_path: &Path) -> Result<String> {
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

    Ok(format!("{:x}", hasher.finalize()))
}
