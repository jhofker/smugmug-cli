use anyhow::{Context, Result};
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod album_series;
pub mod queue;
pub mod worker;

use crate::api::SmugMugClient;
use crate::api::albums::Album;
use crate::api::images::AlbumImage;
use crate::cache::hash_store::HashStore;
use album_series::{AlbumSeries, ClientAlbumSeries};
use queue::UploadQueue;
use worker::{UploadStatus, UploadWorkerContext, upload_worker};
// Re-exported for use in tests and examples
#[allow(unused_imports)]
pub use worker::calculate_file_hash;

/// Fetch the images already present in an album and build the lookup maps
/// used to decide, per local file, whether to skip it (content already
/// present under the same filename), replace it in place (same filename,
/// different content), or create it fresh (new filename). This runs by
/// default so re-running an upload after locally editing photos updates the
/// existing images instead of failing with a 409 Conflict.
///
/// The MD5 index (used by `--check-remote` to detect the same content
/// uploaded anywhere in the album, under any filename) is built from the
/// same listing when `include_md5_index` is set, avoiding a second API call.
async fn fetch_remote_image_maps(
    client: &SmugMugClient,
    album_key: &str,
    include_md5_index: bool,
) -> Result<(
    Option<Arc<HashMap<String, AlbumImage>>>,
    Option<Arc<HashMap<String, String>>>,
)> {
    let images = client.list_album_images(album_key).await?;

    let by_filename: HashMap<String, AlbumImage> = images
        .iter()
        .cloned()
        .map(|img| (img.file_name.clone(), img))
        .collect();

    let by_md5 = if include_md5_index {
        let md5_map: HashMap<String, String> = images
            .iter()
            .filter_map(|img| {
                img.archived_md5
                    .as_ref()
                    .map(|md5| (md5.to_lowercase(), img.image_key.clone()))
            })
            .collect();
        Some(Arc::new(md5_map))
    } else {
        None
    };

    Ok((Some(Arc::new(by_filename)), by_md5))
}

pub struct UploadOptions {
    /// Files to upload (RAW files already filtered out if they can't be
    /// uploaded; see `without_unsupported_raw`).
    pub files: Vec<PathBuf>,
    /// Albums the files go into; new images claim room album by album.
    pub series: Arc<AlbumSeries<ClientAlbumSeries>>,
    pub client: Arc<SmugMugClient>,
    pub threads: usize,
    pub dry_run: bool,
    pub check_remote: bool,
    pub no_cache: bool,
    pub cache_path: PathBuf,
    pub retry_attempts: u32,
}

pub struct UploadStats {
    pub total_files: usize,
    pub uploaded: usize,
    /// Files that replaced an existing, differently-content image with the
    /// same filename already in the album.
    pub replaced: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_bytes: u64,
    pub folders_created: usize,
    pub albums_created: usize,
    pub duration_secs: u64,
}

impl UploadStats {
    fn empty(start_time: std::time::Instant) -> Self {
        UploadStats {
            total_files: 0,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: start_time.elapsed().as_secs(),
        }
    }
}

/// Drop RAW files when the account can't take them (RAW uploads need a
/// SmugMug Source subscription). Returns the files to upload and how many
/// RAW files were dropped.
pub fn without_unsupported_raw(
    files: Vec<PathBuf>,
    has_smugmug_source: bool,
) -> (Vec<PathBuf>, usize) {
    if has_smugmug_source {
        return (files, 0);
    }
    let before = files.len();
    let kept: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| !crate::scanner::is_raw_file(f))
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

pub async fn upload_files(options: UploadOptions) -> Result<UploadStats> {
    let start_time = std::time::Instant::now();

    // Initialize hash store for deduplication
    let hash_store = Arc::new(Mutex::new(
        HashStore::new(&options.cache_path.to_string_lossy())
            .context("Failed to initialize hash store")?,
    ));

    let total_files = options.files.len();

    if total_files == 0 {
        println!("No files found to upload");
        return Ok(UploadStats::empty(start_time));
    }

    // Images already in the series' albums, so unchanged files are skipped
    // and locally-edited files are replaced in place instead of hitting a 409
    // (or being uploaded again into a later album of the series).
    let mut by_filename: HashMap<String, AlbumImage> = HashMap::new();
    let mut by_md5: HashMap<String, String> = HashMap::new();
    let mut remote_ok = true;
    for existing in options.series.existing_albums().await {
        println!(
            "Fetching existing images from album '{}'...",
            existing.album.name
        );
        match fetch_remote_image_maps(
            &options.client,
            &existing.album.album_key,
            options.check_remote,
        )
        .await
        {
            Ok((images, md5s)) => {
                if let Some(images) = images {
                    println!("Found {} existing images", images.len());
                    by_filename.extend(images.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                if let Some(md5s) = md5s {
                    by_md5.extend(md5s.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
            Err(e) => {
                println!("Warning: Failed to fetch existing album images: {}", e);
                remote_ok = false;
            }
        }
    }
    if !remote_ok {
        println!("Continuing without remote duplicate/replace detection");
    }
    let (remote_images, remote_md5s) = if remote_ok {
        (
            Some(Arc::new(by_filename)),
            options.check_remote.then(|| Arc::new(by_md5)),
        )
    } else {
        (None, None)
    };

    println!("\nFound {} files to process", total_files);

    // Setup progress bars
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(total_files as u64));
    overall_progress.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
    );

    // Track statistics
    let stats = Arc::new(Mutex::new(UploadStats {
        total_files,
        ..UploadStats::empty(start_time)
    }));

    let context = Arc::new(UploadWorkerContext {
        client: options.client.clone(),
        album_uri: String::new(),
        album_key: String::new(),
        series: Some(options.series.clone()),
        hash_store: hash_store.clone(),
        remote_md5s,
        remote_images,
        dry_run: options.dry_run,
        no_cache: options.no_cache,
        retry_attempts: options.retry_attempts,
        skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    let mut queue = UploadQueue::new();
    for file in options.files {
        queue.add(file);
    }

    run_upload_workers(
        queue,
        context,
        options.threads,
        stats.clone(),
        overall_progress.clone(),
    )
    .await?;

    overall_progress.finish_with_message("Upload complete");

    let albums_created = options
        .series
        .albums()
        .await
        .iter()
        .filter(|a| a.created)
        .count();

    // Return final statistics
    let final_stats = stats.lock().await;
    Ok(UploadStats {
        total_files: final_stats.total_files,
        uploaded: final_stats.uploaded,
        replaced: final_stats.replaced,
        skipped: final_stats.skipped,
        failed: final_stats.failed,
        total_bytes: final_stats.total_bytes,
        folders_created: 0,
        albums_created,
        duration_secs: start_time.elapsed().as_secs(),
    })
}

/// Drain `queue` with `threads` concurrent workers uploading into the album
/// described by `context`, recording results in `stats`.
async fn run_upload_workers(
    queue: UploadQueue,
    context: Arc<UploadWorkerContext>,
    threads: usize,
    stats: Arc<Mutex<UploadStats>>,
    progress: ProgressBar,
) -> Result<()> {
    let queue = Arc::new(Mutex::new(queue));
    let mut handles = vec![];

    for _ in 0..threads.max(1) {
        let queue = queue.clone();
        let context = context.clone();
        let stats = stats.clone();
        let progress = progress.clone();

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
                            UploadStatus::Replaced { file_size, .. } => {
                                stats.replaced += 1;
                                stats.total_bytes += file_size;
                                progress.set_message(format!("Replaced: {}", file_path.display()));
                            }
                            UploadStatus::Skipped { .. } => {
                                stats.skipped += 1;
                                progress.set_message(format!("Skipped: {}", file_path.display()));
                            }
                            UploadStatus::DryRun { file_size, .. } => {
                                stats.uploaded += 1;
                                stats.total_bytes += file_size;
                                progress
                                    .set_message(format!("Would upload: {}", file_path.display()));
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
    Ok(())
}

pub struct UploadStructureOptions {
    pub path: PathBuf,
    pub client: Arc<SmugMugClient>,
    pub dry_run: bool,
    pub check_remote: bool,
    pub no_cache: bool,
    pub cache_path: PathBuf,
    pub retry_attempts: u32,
    pub has_smugmug_source: bool,
}

pub async fn upload_with_structure(options: UploadStructureOptions) -> Result<UploadStats> {
    use std::collections::HashMap;

    let start_time = std::time::Instant::now();

    // Initialize hash store for deduplication
    let hash_store = Arc::new(Mutex::new(
        HashStore::new(&options.cache_path.to_string_lossy())
            .context("Failed to initialize hash store")?,
    ));

    // Walk the directory structure and build a map of folders to files
    println!("Scanning directory structure...");
    let mut folder_map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut all_files: Vec<PathBuf> = Vec::new();

    fn scan_directory_recursive(
        dir: &std::path::Path,
        folder_map: &mut HashMap<PathBuf, Vec<PathBuf>>,
        all_files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Recursively scan subdirectories
                scan_directory_recursive(&path, folder_map, all_files)?;
            } else if path.is_file() {
                // Check if this is a supported file using the scanner module
                if crate::scanner::is_supported_file(&path) {
                    let parent = path.parent().unwrap().to_path_buf();
                    folder_map
                        .entry(parent)
                        .or_insert_with(Vec::new)
                        .push(path.clone());
                    all_files.push(path);
                }
            }
        }
        Ok(())
    }

    scan_directory_recursive(&options.path, &mut folder_map, &mut all_files)?;

    // Check for RAW files and warn if SmugMug Source is not enabled
    let raw_files: Vec<_> = all_files
        .iter()
        .filter(|f| crate::scanner::is_raw_file(f))
        .collect();

    if !raw_files.is_empty() && !options.has_smugmug_source {
        println!(
            "\n{} {}",
            "⚠".yellow().bold(),
            format!("Warning: {} RAW files detected", raw_files.len())
                .yellow()
                .bold()
        );
        println!(
            "   {}",
            "RAW file uploads require a SmugMug Source subscription.".yellow()
        );
        println!(
            "   {}",
            "These uploads will likely fail without SmugMug Source.".yellow()
        );
        println!(
            "   {}\n",
            "(Update config with 'smugmug-cli init' if you have Source)".bright_black()
        );

        use dialoguer::Confirm;
        let proceed = Confirm::new()
            .with_prompt("Continue anyway?")
            .default(false)
            .interact()?;

        if !proceed {
            println!("{}", "Upload cancelled.".red());
            return Ok(UploadStats {
                total_files: 0,
                uploaded: 0,
                replaced: 0,
                skipped: 0,
                failed: 0,
                total_bytes: 0,
                folders_created: 0,
                albums_created: 0,
                duration_secs: start_time.elapsed().as_secs(),
            });
        }
    }

    if folder_map.is_empty() {
        println!("No image files found to upload");
        return Ok(UploadStats {
            total_files: 0,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: start_time.elapsed().as_secs(),
        });
    }

    // Count total files
    let total_files: usize = folder_map.values().map(|v| v.len()).sum();
    println!(
        "Found {} files in {} folders\n",
        total_files,
        folder_map.len()
    );

    // Track statistics
    let stats = Arc::new(Mutex::new(UploadStats {
        total_files,
        uploaded: 0,
        replaced: 0,
        skipped: 0,
        failed: 0,
        total_bytes: 0,
        folders_created: 0,
        albums_created: 0,
        duration_secs: 0,
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
    node_cache
        .lock()
        .await
        .insert(options.path.clone(), root_node_uri);

    // Setup progress bar
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(total_files as u64));
    overall_progress.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
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
        )
        .await
        {
            Ok(album) => album,
            Err(e) => {
                println!(
                    "✗ Failed to create folder structure for {}: {}",
                    folder_path.display(),
                    e
                );
                let mut s = stats.lock().await;
                s.failed += files.len();
                continue;
            }
        };

        // Fetch existing images in this album so unchanged files are skipped
        // and locally-edited files are replaced in place instead of 409ing.
        let (remote_images, remote_md5s) =
            match fetch_remote_image_maps(&options.client, &album.album_key, options.check_remote)
                .await
            {
                Ok((images, md5s)) => (images, md5s),
                Err(e) => {
                    println!(
                        "Warning: Failed to fetch existing images for {}: {}",
                        album.name, e
                    );
                    (None, None)
                }
            };

        // Create worker context for this album
        let context = Arc::new(UploadWorkerContext {
            client: options.client.clone(),
            album_uri: format!("/api/v2/album/{}", album.album_key),
            album_key: album.album_key.clone(),
            series: None,
            hash_store: hash_store.clone(),
            remote_md5s,
            remote_images,
            dry_run: options.dry_run,
            no_cache: options.no_cache,
            retry_attempts: options.retry_attempts,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
                            overall_progress
                                .set_message(format!("Uploaded: {}", file_path.display()));
                        }
                        UploadStatus::Replaced { file_size, .. } => {
                            s.replaced += 1;
                            s.total_bytes += file_size;
                            overall_progress
                                .set_message(format!("Replaced: {}", file_path.display()));
                        }
                        UploadStatus::Skipped { .. } => {
                            s.skipped += 1;
                            overall_progress
                                .set_message(format!("Skipped: {}", file_path.display()));
                        }
                        UploadStatus::DryRun { file_size, .. } => {
                            s.uploaded += 1;
                            s.total_bytes += file_size;
                            overall_progress
                                .set_message(format!("Would upload: {}", file_path.display()));
                        }
                    }
                    overall_progress.inc(1);
                }
                Err(e) => {
                    let mut s = stats.lock().await;
                    s.failed += 1;
                    overall_progress.set_message(format!(
                        "Failed: {} - {}",
                        file_path.display(),
                        e
                    ));
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
        replaced: final_stats.replaced,
        skipped: final_stats.skipped,
        failed: final_stats.failed,
        total_bytes: final_stats.total_bytes,
        folders_created: final_stats.folders_created,
        albums_created: final_stats.albums_created,
        duration_secs: start_time.elapsed().as_secs(),
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
        let album_name = base_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Uploads")
            .to_string();

        let cache = node_cache.lock().await;
        let parent_node_uri = cache.get(base_path).unwrap().clone();
        drop(cache);

        let album = match client
            .create_album(&album_name, Some(&parent_node_uri), "Private")
            .await
        {
            Ok(album) => {
                let mut s = stats.lock().await;
                s.albums_created += 1;
                album
            }
            Err(e) => {
                // If we get a conflict (409), try to find the existing album
                let error_msg = e.to_string();
                if error_msg.contains("409") || error_msg.contains("Conflict") {
                    // Album already exists, try to find it
                    match client
                        .find_album_in_folder(&parent_node_uri, &album_name)
                        .await?
                    {
                        Some(album) => album,
                        None => return Err(e), // Album should exist but we can't find it
                    }
                } else {
                    return Err(e);
                }
            }
        };
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
                // Last component - create an album (private by default)
                let album = match client
                    .create_album(component_name, Some(&parent_node_uri), "Private")
                    .await
                {
                    Ok(album) => {
                        let mut s = stats.lock().await;
                        s.albums_created += 1;
                        album
                    }
                    Err(e) => {
                        // If we get a conflict (409), try to find the existing album
                        let error_msg = e.to_string();
                        if error_msg.contains("409") || error_msg.contains("Conflict") {
                            // Album already exists, try to find it
                            match client
                                .find_album_in_folder(&parent_node_uri, component_name)
                                .await?
                            {
                                Some(album) => album,
                                None => return Err(e), // Album should exist but we can't find it
                            }
                        } else {
                            return Err(e);
                        }
                    }
                };

                // Cache the album's node URI
                let mut cache = node_cache.lock().await;
                cache.insert(
                    current_path.clone(),
                    format!("/api/v2/node/{}", album.node_id),
                );

                return Ok(album);
            } else {
                // Intermediate component - create a folder
                let folder_node_uri = match create_folder(client, component_name, &parent_node_uri)
                    .await
                {
                    Ok(uri) => {
                        let mut s = stats.lock().await;
                        s.folders_created += 1;
                        uri
                    }
                    Err(e) => {
                        // If we get a conflict (409), try to find the existing folder
                        let error_msg = e.to_string();
                        if error_msg.contains("409") || error_msg.contains("Conflict") {
                            // Folder already exists, find it
                            find_existing_folder(client, component_name, &parent_node_uri).await?
                        } else {
                            return Err(e);
                        }
                    }
                };

                let mut cache = node_cache.lock().await;
                cache.insert(current_path.clone(), folder_node_uri);
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

async fn find_existing_folder(
    client: &Arc<SmugMugClient>,
    name: &str,
    parent_node_uri: &str,
) -> Result<String> {
    // Get children of the parent node
    let children_url = format!("https://api.smugmug.com{}!children", parent_node_uri);
    let response = client.get_with_auth(&children_url).await?;

    #[derive(serde::Deserialize)]
    struct ChildrenResponse {
        #[serde(rename = "Response")]
        response: ChildrenResponseData,
    }

    #[derive(serde::Deserialize)]
    struct ChildrenResponseData {
        #[serde(rename = "Node")]
        nodes: Vec<NodeInfo>,
    }

    #[derive(serde::Deserialize)]
    struct NodeInfo {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "Type")]
        node_type: String,
        #[serde(rename = "Uri")]
        uri: String,
    }

    let children: ChildrenResponse = response.json().await?;

    // Find the folder with the matching name
    for node in children.response.nodes {
        if node.node_type == "Folder" && node.name == name {
            return Ok(node.uri);
        }
    }

    anyhow::bail!("Folder '{}' not found in parent node", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for upload orchestration, options, statistics, and concurrent operations
    // Focuses on structure initialization, stats tracking, and thread safety

    #[test]
    fn test_without_unsupported_raw() {
        let files = vec![
            PathBuf::from("a.jpg"),
            PathBuf::from("b.CR2"),
            PathBuf::from("c.png"),
            PathBuf::from("d.nef"),
        ];

        let (kept, dropped) = without_unsupported_raw(files.clone(), false);
        assert_eq!(kept, vec![PathBuf::from("a.jpg"), PathBuf::from("c.png")]);
        assert_eq!(dropped, 2);

        // With SmugMug Source, RAW files stay.
        let (kept, dropped) = without_unsupported_raw(files.clone(), true);
        assert_eq!(kept, files);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn test_upload_stats_initial() {
        let stats = UploadStats {
            total_files: 10,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: 0,
        };

        assert_eq!(stats.total_files, 10);
        assert_eq!(stats.uploaded, 0);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.folders_created, 0);
        assert_eq!(stats.albums_created, 0);
    }

    #[test]
    fn test_upload_stats_progress() {
        let mut stats = UploadStats {
            total_files: 10,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: 0,
        };

        // Simulate progress
        stats.uploaded += 5;
        stats.skipped += 2;
        stats.failed += 1;
        stats.total_bytes += 5000000; // 5MB

        assert_eq!(stats.uploaded, 5);
        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.total_bytes, 5000000);
        assert_eq!(stats.uploaded + stats.skipped + stats.failed, 8);
    }

    #[test]
    fn test_upload_structure_options_creation() {
        let client = Arc::new(SmugMugClient::new(
            "key".to_string(),
            "secret".to_string(),
            "token".to_string(),
            "token_secret".to_string(),
        ));

        let options = UploadStructureOptions {
            path: PathBuf::from("/test/structure"),
            client: client.clone(),
            dry_run: false,
            check_remote: true,
            no_cache: false,
            cache_path: PathBuf::from("/cache/path"),
            retry_attempts: 3,
            has_smugmug_source: false,
        };

        assert_eq!(options.path, PathBuf::from("/test/structure"));
        assert!(!options.dry_run);
        assert!(options.check_remote);
        assert!(!options.no_cache);
        assert_eq!(options.cache_path, PathBuf::from("/cache/path"));
    }

    #[tokio::test]
    async fn test_queue_thread_safety() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let queue = Arc::new(Mutex::new(UploadQueue::new()));

        // Add files to queue
        for i in 0..10 {
            let mut q = queue.lock().await;
            q.add(PathBuf::from(format!("/test/file{}.jpg", i)));
        }

        // Spawn multiple workers to consume from queue
        let mut handles = vec![];
        for _ in 0..3 {
            let q = queue.clone();
            let handle = tokio::spawn(async move {
                let mut count = 0;
                loop {
                    let item = {
                        let mut queue = q.lock().await;
                        queue.next()
                    };
                    if item.is_none() {
                        break;
                    }
                    count += 1;
                }
                count
            });
            handles.push(handle);
        }

        // Collect results
        let mut total_processed = 0;
        for handle in handles {
            total_processed += handle.await.unwrap();
        }

        assert_eq!(total_processed, 10);

        // Queue should be empty
        let mut q = queue.lock().await;
        assert!(q.next().is_none());
    }

    #[tokio::test]
    async fn test_upload_stats_concurrent_updates() {
        let stats = Arc::new(Mutex::new(UploadStats {
            total_files: 100,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: 0,
        }));

        let mut handles = vec![];

        // Spawn multiple tasks updating stats
        for _ in 0..10 {
            let s = stats.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..10 {
                    let mut stats = s.lock().await;
                    stats.uploaded += 1;
                    stats.total_bytes += 1024;
                }
            });
            handles.push(handle);
        }

        // Wait for all tasks
        for handle in handles {
            handle.await.unwrap();
        }

        let final_stats = stats.lock().await;
        assert_eq!(final_stats.uploaded, 100);
        assert_eq!(final_stats.total_bytes, 102400);
    }

    #[test]
    fn test_upload_stats_zero_state() {
        let stats = UploadStats {
            total_files: 0,
            uploaded: 0,
            replaced: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: 0,
        };

        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.uploaded + stats.skipped + stats.failed, 0);
    }

    #[test]
    fn test_upload_stats_large_numbers() {
        let stats = UploadStats {
            total_files: 10000,
            uploaded: 8500,
            replaced: 0,
            skipped: 1200,
            failed: 300,
            total_bytes: 50_000_000_000, // 50GB
            folders_created: 100,
            albums_created: 50,
            duration_secs: 0,
        };

        assert_eq!(stats.total_files, 10000);
        assert_eq!(stats.uploaded, 8500);
        assert_eq!(stats.skipped, 1200);
        assert_eq!(stats.failed, 300);
        assert_eq!(stats.total_bytes, 50_000_000_000);
        assert_eq!(stats.folders_created, 100);
        assert_eq!(stats.albums_created, 50);
    }

    #[tokio::test]
    async fn test_multiple_workers_processing_queue() {
        let queue = Arc::new(Mutex::new(UploadQueue::new()));

        // Add 100 files
        for i in 0..100 {
            let mut q = queue.lock().await;
            q.add(PathBuf::from(format!("/test/image{}.jpg", i)));
        }

        let processed = Arc::new(Mutex::new(Vec::new()));
        let mut handles = vec![];

        // Create 5 workers
        for worker_id in 0..5 {
            let q = queue.clone();
            let p = processed.clone();

            let handle = tokio::spawn(async move {
                loop {
                    let file = {
                        let mut queue = q.lock().await;
                        queue.next()
                    };

                    match file {
                        Some(path) => {
                            let mut proc = p.lock().await;
                            proc.push((worker_id, path));
                        }
                        None => break,
                    }
                }
            });

            handles.push(handle);
        }

        // Wait for all workers
        for handle in handles {
            handle.await.unwrap();
        }

        let proc = processed.lock().await;
        assert_eq!(proc.len(), 100);

        // Verify all files were processed
        let mut file_indices: Vec<usize> = proc
            .iter()
            .map(|(_, path)| {
                let name = path.file_name().unwrap().to_str().unwrap();
                let num_str = name
                    .strip_prefix("image")
                    .unwrap()
                    .strip_suffix(".jpg")
                    .unwrap();
                num_str.parse().unwrap()
            })
            .collect();

        file_indices.sort();
        assert_eq!(file_indices, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn test_upload_stats_calculation() {
        let stats = UploadStats {
            total_files: 100,
            uploaded: 70,
            replaced: 0,
            skipped: 20,
            failed: 10,
            total_bytes: 1_073_741_824, // 1GB
            folders_created: 5,
            albums_created: 3,
            duration_secs: 0,
        };

        // Verify all files accounted for
        assert_eq!(
            stats.uploaded + stats.skipped + stats.failed,
            stats.total_files
        );

        // Check individual values
        assert_eq!(stats.uploaded, 70);
        assert_eq!(stats.skipped, 20);
        assert_eq!(stats.failed, 10);
        assert_eq!(stats.total_bytes, 1_073_741_824);
    }
}
