use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Mutex;

use crate::api::images::AlbumImage;
use crate::api::SmugMugClient;

pub struct DownloadOptions {
    pub album_key: String,
    pub output_dir: PathBuf,
    pub client: Arc<SmugMugClient>,
    pub threads: usize,
}

pub struct DownloadStats {
    pub total_images: usize,
    pub downloaded: usize,
    pub failed: usize,
    pub total_bytes: u64,
}

pub async fn download_album(options: DownloadOptions) -> Result<DownloadStats> {
    // List images in the album
    let images = options
        .client
        .list_album_images(&options.album_key)
        .await
        .context("Failed to list album images")?;

    if images.is_empty() {
        println!("No images found in album");
        return Ok(DownloadStats {
            total_images: 0,
            downloaded: 0,
            failed: 0,
            total_bytes: 0,
        });
    }

    println!("Found {} images to download", images.len());

    // Create output directory if it doesn't exist
    fs::create_dir_all(&options.output_dir)
        .await
        .context("Failed to create output directory")?;

    // Create download queue
    let mut queue = VecDeque::new();
    for image in &images {
        queue.push_back(image.clone());
    }

    // Setup progress bars
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(images.len() as u64));
    overall_progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Track statistics
    let stats = Arc::new(Mutex::new(DownloadStats {
        total_images: images.len(),
        downloaded: 0,
        failed: 0,
        total_bytes: 0,
    }));

    // Process images with concurrent workers
    let queue = Arc::new(Mutex::new(queue));
    let output_dir = Arc::new(options.output_dir);
    let mut handles = vec![];

    for _ in 0..options.threads {
        let queue = queue.clone();
        let stats = stats.clone();
        let progress = overall_progress.clone();
        let output_dir = output_dir.clone();

        let handle = tokio::spawn(async move {
            let http_client = reqwest::Client::new();

            loop {
                // Get next image from queue
                let image = {
                    let mut q = queue.lock().await;
                    q.pop_front()
                };

                let Some(image) = image else {
                    break;
                };

                // Download the image
                match download_image(&http_client, &image, &output_dir).await {
                    Ok(bytes_downloaded) => {
                        let mut stats = stats.lock().await;
                        stats.downloaded += 1;
                        stats.total_bytes += bytes_downloaded;
                        progress.set_message(format!("Downloaded: {}", image.file_name));
                        progress.inc(1);
                    }
                    Err(e) => {
                        let mut stats = stats.lock().await;
                        stats.failed += 1;
                        progress.set_message(format!("Failed: {} - {}", image.file_name, e));
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

    overall_progress.finish_with_message("Download complete");

    // Return final statistics
    let final_stats = stats.lock().await;
    Ok(DownloadStats {
        total_images: final_stats.total_images,
        downloaded: final_stats.downloaded,
        failed: final_stats.failed,
        total_bytes: final_stats.total_bytes,
    })
}

async fn download_image(
    client: &reqwest::Client,
    image: &AlbumImage,
    output_dir: &PathBuf,
) -> Result<u64> {
    // Download the image from the archived URI
    let response = client
        .get(&image.archived_uri)
        .send()
        .await
        .context("Failed to download image")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status: {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response bytes")?;

    let bytes_len = bytes.len() as u64;

    // Determine output file path, handling filename conflicts
    let mut output_path = output_dir.join(&image.file_name);
    let mut counter = 1;

    while output_path.exists() {
        // Extract extension and base name
        let extension = output_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let stem = output_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");

        // Create new filename with counter
        let new_filename = if extension.is_empty() {
            format!("{}_{}", stem, counter)
        } else {
            format!("{}_{}.{}", stem, counter, extension)
        };

        output_path = output_dir.join(new_filename);
        counter += 1;
    }

    // Write the file
    fs::write(&output_path, bytes)
        .await
        .context("Failed to write image file")?;

    Ok(bytes_len)
}
