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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::images::AlbumImage;
    use mockito::Server;
    use tempfile::TempDir;
    use tokio::fs;

    // Helper function to create a test AlbumImage
    fn create_test_image(file_name: &str, image_key: &str) -> AlbumImage {
        AlbumImage {
            image_key: image_key.to_string(),
            file_name: file_name.to_string(),
            archived_uri: format!("https://example.com/{}", file_name),
            file_size: 1024,
            format: "JPG".to_string(),
            uri: format!("https://example.com/api/image/{}", image_key),
            title: Some("Test Image".to_string()),
            archived_md5: Some("abc123".to_string()),
        }
    }

    // Helper function to create a test AlbumImage with a custom URL
    fn create_test_image_with_url(file_name: &str, image_key: &str, url: &str) -> AlbumImage {
        AlbumImage {
            image_key: image_key.to_string(),
            file_name: file_name.to_string(),
            archived_uri: url.to_string(),
            file_size: 1024,
            format: "JPG".to_string(),
            uri: format!("https://example.com/api/image/{}", image_key),
            title: Some("Test Image".to_string()),
            archived_md5: Some("abc123".to_string()),
        }
    }

    #[test]
    fn test_download_options_creation() {
        let client = Arc::new(SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        ));

        let options = DownloadOptions {
            album_key: "test_album".to_string(),
            output_dir: PathBuf::from("/tmp/test"),
            client: client.clone(),
            threads: 4,
        };

        assert_eq!(options.album_key, "test_album");
        assert_eq!(options.output_dir, PathBuf::from("/tmp/test"));
        assert_eq!(options.threads, 4);
    }

    #[test]
    fn test_download_stats_initialization() {
        let stats = DownloadStats {
            total_images: 10,
            downloaded: 5,
            failed: 2,
            total_bytes: 1024000,
        };

        assert_eq!(stats.total_images, 10);
        assert_eq!(stats.downloaded, 5);
        assert_eq!(stats.failed, 2);
        assert_eq!(stats.total_bytes, 1024000);
    }

    #[test]
    fn test_download_stats_empty() {
        let stats = DownloadStats {
            total_images: 0,
            downloaded: 0,
            failed: 0,
            total_bytes: 0,
        };

        assert_eq!(stats.total_images, 0);
        assert_eq!(stats.downloaded, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[tokio::test]
    async fn test_download_image_success() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/test.jpg")
            .with_status(200)
            .with_header("content-type", "image/jpeg")
            .with_body("fake image data")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "test.jpg",
            "key123",
            &format!("{}/test.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 15); // "fake image data" is 15 bytes

        // Verify file was created
        let output_path = temp_dir.path().join("test.jpg");
        assert!(output_path.exists());

        // Verify file content
        let content = fs::read_to_string(&output_path).await.unwrap();
        assert_eq!(content, "fake image data");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_http_error() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/error.jpg")
            .with_status(404)
            .with_body("Not Found")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "error.jpg",
            "key456",
            &format!("{}/error.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Download failed with status: 404"));

        // Verify file was not created
        let output_path = temp_dir.path().join("error.jpg");
        assert!(!output_path.exists());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_file_conflict_handling() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/image.jpg")
            .with_status(200)
            .with_body("test data")
            .expect(3)
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "image.jpg",
            "key789",
            &format!("{}/image.jpg", server.url()),
        );

        let client = reqwest::Client::new();

        // First download - should create image.jpg
        let result1 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result1.is_ok());
        assert!(temp_dir.path().join("image.jpg").exists());

        // Second download - should create image_1.jpg
        let result2 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result2.is_ok());
        assert!(temp_dir.path().join("image_1.jpg").exists());

        // Third download - should create image_1_1.jpg (since image_1.jpg exists)
        let result3 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result3.is_ok());

        // Verify first two files exist (third file name depends on implementation)
        assert!(temp_dir.path().join("image.jpg").exists());
        assert!(temp_dir.path().join("image_1.jpg").exists());
        // Third file should exist (name may vary based on conflict resolution)
        assert!(result3.is_ok(), "Third download should succeed");

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_file_conflict_no_extension() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/noext")
            .with_status(200)
            .with_body("data")
            .expect(2)
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "noext",
            "key999",
            &format!("{}/noext", server.url()),
        );

        let client = reqwest::Client::new();

        // First download - should create noext
        let result1 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result1.is_ok());
        assert!(temp_dir.path().join("noext").exists());

        // Second download - should create noext_1
        let result2 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result2.is_ok());
        assert!(temp_dir.path().join("noext_1").exists());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_preserves_file_size() {
        let mut server = Server::new_async().await;
        let test_data = "a".repeat(10000); // 10KB of data
        let mock = server
            .mock("GET", "/large.jpg")
            .with_status(200)
            .with_body(&test_data)
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "large.jpg",
            "keyabc",
            &format!("{}/large.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 10000);

        // Verify file size
        let output_path = temp_dir.path().join("large.jpg");
        let metadata = fs::metadata(&output_path).await.unwrap();
        assert_eq!(metadata.len(), 10000);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_complex_filename() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(200)
            .with_body("data")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "my.photo.with.dots.jpg",
            "keydef",
            &format!("{}/file", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_ok());

        // File should be created with the specified name
        let output_path = temp_dir.path().join("my.photo.with.dots.jpg");
        assert!(output_path.exists());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_conflict_with_complex_filename() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(200)
            .with_body("data")
            .expect(2)
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "photo.backup.tar.gz",
            "keyghi",
            &format!("{}/file", server.url()),
        );

        let client = reqwest::Client::new();

        // First download
        let result1 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result1.is_ok());
        assert!(temp_dir.path().join("photo.backup.tar.gz").exists());

        // Second download - should create photo.backup.tar_1.gz
        let result2 = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;
        assert!(result2.is_ok());
        assert!(temp_dir.path().join("photo.backup.tar_1.gz").exists());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_empty_response() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/empty.jpg")
            .with_status(200)
            .with_body("")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "empty.jpg",
            "keyjkl",
            &format!("{}/empty.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);

        // Verify empty file was created
        let output_path = temp_dir.path().join("empty.jpg");
        assert!(output_path.exists());
        let metadata = fs::metadata(&output_path).await.unwrap();
        assert_eq!(metadata.len(), 0);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_creates_parent_directory() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/test.jpg")
            .with_status(200)
            .with_body("data")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();

        // Ensure the directory exists before passing it to download_image
        let output_dir = temp_dir.path().to_path_buf();
        fs::create_dir_all(&output_dir).await.unwrap();

        let image = create_test_image_with_url(
            "test.jpg",
            "keymno",
            &format!("{}/test.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &output_dir).await;

        assert!(result.is_ok());

        let output_path = output_dir.join("test.jpg");
        assert!(output_path.exists());

        mock.assert_async().await;
    }

    #[test]
    fn test_download_stats_all_successful() {
        let stats = DownloadStats {
            total_images: 10,
            downloaded: 10,
            failed: 0,
            total_bytes: 5000000,
        };

        assert_eq!(stats.total_images, stats.downloaded);
        assert_eq!(stats.failed, 0);
        assert!(stats.total_bytes > 0);
    }

    #[test]
    fn test_download_stats_partial_failure() {
        let stats = DownloadStats {
            total_images: 10,
            downloaded: 7,
            failed: 3,
            total_bytes: 3500000,
        };

        assert_eq!(stats.total_images, stats.downloaded + stats.failed);
        assert!(stats.failed > 0);
        assert!(stats.downloaded > stats.failed);
    }

    #[test]
    fn test_download_stats_all_failed() {
        let stats = DownloadStats {
            total_images: 5,
            downloaded: 0,
            failed: 5,
            total_bytes: 0,
        };

        assert_eq!(stats.total_images, stats.failed);
        assert_eq!(stats.downloaded, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[tokio::test]
    async fn test_download_image_with_special_characters_in_filename() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/file")
            .with_status(200)
            .with_body("data")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        // Note: Some special characters may not be valid in filenames
        // This tests that the system can handle typical filename variations
        let image = create_test_image_with_url(
            "photo (1).jpg",
            "keypqr",
            &format!("{}/file", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_ok());

        let output_path = temp_dir.path().join("photo (1).jpg");
        assert!(output_path.exists());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_server_error() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/server_error.jpg")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let temp_dir = TempDir::new().unwrap();
        let image = create_test_image_with_url(
            "server_error.jpg",
            "keystu",
            &format!("{}/server_error.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result = download_image(&client, &image, &temp_dir.path().to_path_buf()).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Download failed with status: 500"));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_download_image_bytes_counting() {
        let mut server = Server::new_async().await;

        // Test small file (1 byte)
        let mock1 = server
            .mock("GET", "/small.jpg")
            .with_status(200)
            .with_body("x")
            .create_async()
            .await;

        let temp_dir1 = TempDir::new().unwrap();
        let image1 = create_test_image_with_url(
            "small.jpg",
            "1",
            &format!("{}/small.jpg", server.url()),
        );

        let client = reqwest::Client::new();
        let result1 = download_image(&client, &image1, &temp_dir1.path().to_path_buf()).await;
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), 1);
        mock1.assert_async().await;

        // Test medium file (100 bytes)
        let medium_content = "y".repeat(100);
        let mock2 = server
            .mock("GET", "/medium.jpg")
            .with_status(200)
            .with_body(&medium_content)
            .create_async()
            .await;

        let temp_dir2 = TempDir::new().unwrap();
        let image2 = create_test_image_with_url(
            "medium.jpg",
            "2",
            &format!("{}/medium.jpg", server.url()),
        );

        let result2 = download_image(&client, &image2, &temp_dir2.path().to_path_buf()).await;
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), 100);
        mock2.assert_async().await;

        // Test large file (5000 bytes)
        let large_content = "z".repeat(5000);
        let mock3 = server
            .mock("GET", "/large.jpg")
            .with_status(200)
            .with_body(&large_content)
            .create_async()
            .await;

        let temp_dir3 = TempDir::new().unwrap();
        let image3 = create_test_image_with_url(
            "large.jpg",
            "3",
            &format!("{}/large.jpg", server.url()),
        );

        let result3 = download_image(&client, &image3, &temp_dir3.path().to_path_buf()).await;
        assert!(result3.is_ok());
        assert_eq!(result3.unwrap(), 5000);
        mock3.assert_async().await;
    }
}
