use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod queue;
pub mod worker;

use crate::api::albums::Album;
use crate::api::SmugMugClient;
use crate::cache::hash_store::HashStore;
use crate::scanner::scan_directory;
use queue::UploadQueue;
use worker::{upload_worker, UploadStatus, UploadWorkerContext};

pub struct UploadOptions {
    pub path: PathBuf,
    pub album: Album,
    pub client: Arc<SmugMugClient>,
    pub threads: usize,
    pub dry_run: bool,
    pub cache_path: PathBuf,
}

pub struct UploadStats {
    pub total_files: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_bytes: u64,
}

pub async fn upload_files(options: UploadOptions) -> Result<UploadStats> {
    // Initialize hash store for deduplication
    let hash_store = Arc::new(Mutex::new(
        HashStore::new(&options.cache_path.to_string_lossy())
            .context("Failed to initialize hash store")?,
    ));

    // Scan directory for files
    let scanned_files = scan_directory(&options.path)
        .context("Failed to scan directory")?;

    if scanned_files.is_empty() {
        println!("No files found to upload");
        return Ok(UploadStats {
            total_files: 0,
            uploaded: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
        });
    }

    // Create upload queue
    let mut queue = UploadQueue::new();
    for file in &scanned_files {
        queue.add(file.path.clone());
    }

    println!("Found {} files to process", scanned_files.len());

    // Setup progress bars
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(scanned_files.len() as u64));
    overall_progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Create worker context
    let context = Arc::new(UploadWorkerContext {
        client: options.client,
        album_uri: format!("/api/v2/album/{}", options.album.album_key),
        album_key: options.album.album_key.clone(),
        hash_store: hash_store.clone(),
        dry_run: options.dry_run,
    });

    // Track statistics
    let stats = Arc::new(Mutex::new(UploadStats {
        total_files: scanned_files.len(),
        uploaded: 0,
        skipped: 0,
        failed: 0,
        total_bytes: 0,
    }));

    // Process files with concurrent workers
    let queue = Arc::new(Mutex::new(queue));
    let mut handles = vec![];

    for _ in 0..options.threads {
        let queue = queue.clone();
        let context = context.clone();
        let stats = stats.clone();
        let progress = overall_progress.clone();

        let handle = tokio::spawn(async move {
            loop {
                // Get next file from queue
                let file_path = {
                    let mut q = queue.lock().await;
                    q.next()
                };

                let Some(file_path) = file_path else {
                    break;
                };

                // Process the file
                match upload_worker(&file_path, context.clone()).await {
                    Ok(status) => {
                        let mut stats = stats.lock().await;
                        match status {
                            UploadStatus::Uploaded { file_size, .. } => {
                                stats.uploaded += 1;
                                stats.total_bytes += file_size;
                                progress.set_message(format!("Uploaded: {}", file_path.display()));
                            }
                            UploadStatus::Skipped { .. } => {
                                stats.skipped += 1;
                                progress.set_message(format!("Skipped: {}", file_path.display()));
                            }
                            UploadStatus::DryRun { file_size, .. } => {
                                stats.uploaded += 1;
                                stats.total_bytes += file_size;
                                progress.set_message(format!("Would upload: {}", file_path.display()));
                            }
                        }
                        progress.inc(1);
                    }
                    Err(e) => {
                        let mut stats = stats.lock().await;
                        stats.failed += 1;
                        progress.set_message(format!("Failed: {} - {}", file_path.display(), e));
                        progress.inc(1);
                    }
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all workers to complete
    for handle in handles {
        handle.await?;
    }

    overall_progress.finish_with_message("Upload complete");

    // Return final statistics
    let final_stats = stats.lock().await;
    Ok(UploadStats {
        total_files: final_stats.total_files,
        uploaded: final_stats.uploaded,
        skipped: final_stats.skipped,
        failed: final_stats.failed,
        total_bytes: final_stats.total_bytes,
    })
}
