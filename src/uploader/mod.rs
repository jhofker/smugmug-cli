use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
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
    pub check_remote: bool,
    pub no_cache: bool,
    pub cache_path: PathBuf,
}

pub struct UploadStats {
    pub total_files: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_bytes: u64,
    pub folders_created: usize,
    pub albums_created: usize,
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
            folders_created: 0,
            albums_created: 0,
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

    // Fetch remote MD5s if check_remote is enabled
    let remote_md5s = if options.check_remote {
        println!("Fetching existing images from SmugMug...");
        match options.client.list_album_images(&options.album.album_key).await {
            Ok(images) => {
                let md5_map: std::collections::HashMap<String, String> = images
                    .iter()
                    .filter_map(|img| {
                        img.archived_md5.as_ref().map(|md5| (md5.to_lowercase(), img.image_key.clone()))
                    })
                    .collect();
                println!("Found {} images with MD5 hashes on SmugMug\n", md5_map.len());
                Some(Arc::new(md5_map))
            }
            Err(e) => {
                println!("Warning: Failed to fetch remote MD5s: {}", e);
                println!("Continuing with local cache only\n");
                None
            }
        }
    } else {
        None
    };

    // Create worker context
    let context = Arc::new(UploadWorkerContext {
        client: options.client,
        album_uri: format!("/api/v2/album/{}", options.album.album_key),
        album_key: options.album.album_key.clone(),
        hash_store: hash_store.clone(),
        remote_md5s,
        dry_run: options.dry_run,
        no_cache: options.no_cache,
    });

    // Track statistics
    let stats = Arc::new(Mutex::new(UploadStats {
        total_files: scanned_files.len(),
        uploaded: 0,
        skipped: 0,
        failed: 0,
        total_bytes: 0,
        folders_created: 0,
        albums_created: 0,
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
        folders_created: 0,
        albums_created: 0,
    })
}

pub struct UploadStructureOptions {
    pub path: PathBuf,
    pub client: Arc<SmugMugClient>,
    pub dry_run: bool,
    pub check_remote: bool,
    pub no_cache: bool,
    pub cache_path: PathBuf,
}

pub async fn upload_with_structure(options: UploadStructureOptions) -> Result<UploadStats> {
    use std::collections::HashMap;

    // Initialize hash store for deduplication
    let hash_store = Arc::new(Mutex::new(
        HashStore::new(&options.cache_path.to_string_lossy())
            .context("Failed to initialize hash store")?,
    ));

    // Walk the directory structure and build a map of folders to files
    println!("Scanning directory structure...");
    let mut folder_map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    fn scan_directory_recursive(
        dir: &std::path::Path,
        folder_map: &mut HashMap<PathBuf, Vec<PathBuf>>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Recursively scan subdirectories
                scan_directory_recursive(&path, folder_map)?;
            } else if path.is_file() {
                // Check if this is an image file
                if let Some(ext) = path.extension() {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if matches!(ext_lower.as_str(), "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "webp" | "heic") {
                        let parent = path.parent().unwrap().to_path_buf();
                        folder_map.entry(parent).or_insert_with(Vec::new).push(path);
                    }
                }
            }
        }
        Ok(())
    }

    scan_directory_recursive(&options.path, &mut folder_map)?;

    if folder_map.is_empty() {
        println!("No image files found to upload");
        return Ok(UploadStats {
            total_files: 0,
            uploaded: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
        });
    }

    // Count total files
    let total_files: usize = folder_map.values().map(|v| v.len()).sum();
    println!("Found {} files in {} folders\n", total_files, folder_map.len());

    // Track statistics
    let stats = Arc::new(Mutex::new(UploadStats {
        total_files,
        uploaded: 0,
        skipped: 0,
        failed: 0,
        total_bytes: 0,
        folders_created: 0,
        albums_created: 0,
    }));

    // Get the authenticated user's node URI
    let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
    let response = options.client.get_with_auth(auth_user_url).await?;
    let body_text = response.text().await?;

    #[derive(serde::Deserialize)]
    struct UserResponse {
        #[serde(rename = "Response")]
        response: UserResponseData,
    }

    #[derive(serde::Deserialize)]
    struct UserResponseData {
        #[serde(rename = "User")]
        user: UserInfo,
    }

    #[derive(serde::Deserialize)]
    struct UserInfo {
        #[serde(rename = "Uris")]
        uris: UserUris,
    }

    #[derive(serde::Deserialize)]
    struct UserUris {
        #[serde(rename = "Node")]
        node: NodeUriInfo,
    }

    #[derive(serde::Deserialize)]
    struct NodeUriInfo {
        #[serde(rename = "Uri")]
        uri: String,
    }

    let user_data: UserResponse = serde_json::from_str(&body_text)?;
    let root_node_uri = user_data.response.user.uris.node.uri;

    // Create a map to cache node URIs for each folder path
    let node_cache: Arc<Mutex<HashMap<PathBuf, String>>> = Arc::new(Mutex::new(HashMap::new()));
    node_cache.lock().await.insert(options.path.clone(), root_node_uri);

    // Setup progress bar
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(total_files as u64));
    overall_progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Process each folder
    for (folder_path, files) in folder_map {
        // Get or create the folder/album structure
        let album = match get_or_create_folder_structure(
            &options.client,
            &options.path,
            &folder_path,
            &node_cache,
            &stats,
        ).await {
            Ok(album) => album,
            Err(e) => {
                println!("✗ Failed to create folder structure for {}: {}", folder_path.display(), e);
                let mut s = stats.lock().await;
                s.failed += files.len();
                continue;
            }
        };

        // Fetch remote MD5s if check_remote is enabled
        let remote_md5s = if options.check_remote {
            match options.client.list_album_images(&album.album_key).await {
                Ok(images) => {
                    let md5_map: std::collections::HashMap<String, String> = images
                        .iter()
                        .filter_map(|img| {
                            img.archived_md5.as_ref().map(|md5| (md5.to_lowercase(), img.image_key.clone()))
                        })
                        .collect();
                    Some(Arc::new(md5_map))
                }
                Err(e) => {
                    println!("Warning: Failed to fetch remote MD5s for {}: {}", album.name, e);
                    None
                }
            }
        } else {
            None
        };

        // Create worker context for this album
        let context = Arc::new(UploadWorkerContext {
            client: options.client.clone(),
            album_uri: format!("/api/v2/album/{}", album.album_key),
            album_key: album.album_key.clone(),
            hash_store: hash_store.clone(),
            remote_md5s,
            dry_run: options.dry_run,
            no_cache: options.no_cache,
        });

        // Upload files in this folder
        for file_path in files {
            match upload_worker(&file_path, context.clone()).await {
                Ok(status) => {
                    let mut s = stats.lock().await;
                    match status {
                        UploadStatus::Uploaded { file_size, .. } => {
                            s.uploaded += 1;
                            s.total_bytes += file_size;
                            overall_progress.set_message(format!("Uploaded: {}", file_path.display()));
                        }
                        UploadStatus::Skipped { .. } => {
                            s.skipped += 1;
                            overall_progress.set_message(format!("Skipped: {}", file_path.display()));
                        }
                        UploadStatus::DryRun { file_size, .. } => {
                            s.uploaded += 1;
                            s.total_bytes += file_size;
                            overall_progress.set_message(format!("Would upload: {}", file_path.display()));
                        }
                    }
                    overall_progress.inc(1);
                }
                Err(e) => {
                    let mut s = stats.lock().await;
                    s.failed += 1;
                    overall_progress.set_message(format!("Failed: {} - {}", file_path.display(), e));
                    overall_progress.inc(1);
                }
            }
        }
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
        folders_created: final_stats.folders_created,
        albums_created: final_stats.albums_created,
    })
}

async fn get_or_create_folder_structure(
    client: &Arc<SmugMugClient>,
    base_path: &std::path::Path,
    target_path: &std::path::Path,
    node_cache: &Arc<Mutex<HashMap<PathBuf, String>>>,
    stats: &Arc<Mutex<UploadStats>>,
) -> Result<Album> {
    // Get the relative path from base to target
    let rel_path = target_path.strip_prefix(base_path)?;

    // If this is the base directory, create an album at the root
    if rel_path.as_os_str().is_empty() {
        let album_name = base_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Uploads")
            .to_string();

        let cache = node_cache.lock().await;
        let parent_node_uri = cache.get(base_path).unwrap().clone();
        drop(cache);

        let album = client.create_album(&album_name, Some(&parent_node_uri)).await?;
        let mut s = stats.lock().await;
        s.albums_created += 1;
        return Ok(album);
    }

    // Walk through each component of the path, creating folders/albums as needed
    let mut current_path = base_path.to_path_buf();
    let components: Vec<_> = rel_path.components().collect();

    for (i, component) in components.iter().enumerate() {
        let component_name = component.as_os_str().to_str().unwrap();
        current_path.push(component_name);

        // Check if we already have a node for this path
        let existing_node = {
            let cache = node_cache.lock().await;
            cache.get(&current_path).cloned()
        };

        if existing_node.is_none() {
            // Get parent node URI
            let parent_path = current_path.parent().unwrap().to_path_buf();
            let parent_node_uri = {
                let cache = node_cache.lock().await;
                cache.get(&parent_path).unwrap().clone()
            };

            // Determine if this should be a folder or album
            let is_last = i == components.len() - 1;

            if is_last {
                // Last component - create an album
                let album = client.create_album(component_name, Some(&parent_node_uri)).await?;

                // Cache the album's node URI
                let mut cache = node_cache.lock().await;
                cache.insert(current_path.clone(), format!("/api/v2/node/{}", album.node_id));

                let mut s = stats.lock().await;
                s.albums_created += 1;

                return Ok(album);
            } else {
                // Intermediate component - create a folder
                let folder_node_uri = create_folder(client, component_name, &parent_node_uri).await?;

                let mut cache = node_cache.lock().await;
                cache.insert(current_path.clone(), folder_node_uri);

                let mut s = stats.lock().await;
                s.folders_created += 1;
            }
        }
    }

    anyhow::bail!("Failed to create folder structure")
}

async fn create_folder(
    client: &Arc<SmugMugClient>,
    name: &str,
    parent_node_uri: &str,
) -> Result<String> {
    let create_url = format!("https://api.smugmug.com{}!children", parent_node_uri);

    let body = serde_json::json!({
        "Type": "Folder",
        "Name": name,
    });

    let response = client.post_with_auth(&create_url, body).await?;

    let status = response.status();
    let body_text = response.text().await?;

    if !status.is_success() {
        anyhow::bail!("Failed to create folder: {} - {}", status, body_text);
    }

    #[derive(serde::Deserialize)]
    struct CreateNodeResponse {
        #[serde(rename = "Response")]
        response: CreateNodeResponseData,
    }

    #[derive(serde::Deserialize)]
    struct CreateNodeResponseData {
        #[serde(rename = "Node")]
        node: NodeInfo,
    }

    #[derive(serde::Deserialize)]
    struct NodeInfo {
        #[serde(rename = "Uri")]
        uri: String,
    }

    let node_response: CreateNodeResponse = serde_json::from_str(&body_text)?;
    Ok(node_response.response.node.uri)
}
