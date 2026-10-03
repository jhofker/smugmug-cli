use anyhow::{Context, Result};
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod album_series;
pub mod capture_time;
pub mod collect;
pub mod queue;
pub mod worker;

use crate::api::SmugMugClient;
use crate::api::albums::Album;
use crate::api::images::AlbumImage;
use crate::cache::hash_store::HashStore;
use crate::config::{RawHandling, RawMode};
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
    /// Files to upload (RAW files already selected; see `select_raw_files`).
    pub files: Vec<PathBuf>,
    /// How the RAW files in `files` are uploaded.
    pub raw_handling: RawHandling,
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
    /// Files already on SmugMug that were added to the album instead of
    /// uploaded again (see `collect`).
    pub collected: usize,
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
            collected: 0,
            skipped: 0,
            failed: 0,
            total_bytes: 0,
            folders_created: 0,
            albums_created: 0,
            duration_secs: start_time.elapsed().as_secs(),
        }
    }
}

/// What `select_raw_files` did with the RAW files it was given.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RawSelection {
    /// RAW files kept, to be uploaded as JPEGs rendered from them.
    pub rendered: usize,
    /// RAW files left out because RAW handling is `Skip`.
    pub skipped: usize,
    /// RAW files left out because a JPEG or HEIC with the same name sits in
    /// the same directory (as when shooting RAW+JPEG), or because an
    /// earlier RAW there renders to the same name.
    pub skipped_with_sibling: usize,
}

/// Decide which RAW files to upload, per `handling`. Non-RAW files are
/// always kept.
///
/// When rendering, a RAW file `IMG_1234.CR2` becomes `IMG_1234.jpg`, which
/// would collide with the camera's own `IMG_1234.JPG` next to it; the
/// camera's JPEG is kept and the RAW dropped.
pub fn select_raw_files(
    files: Vec<PathBuf>,
    handling: RawHandling,
) -> (Vec<PathBuf>, RawSelection) {
    use std::collections::HashSet;

    let mut selection = RawSelection::default();
    let key = |f: &PathBuf| {
        let stem = f.file_stem().map(|s| s.to_string_lossy().to_lowercase());
        (f.parent().map(|p| p.to_path_buf()), stem)
    };

    // Names already taken by a camera JPEG/HEIC in each directory.
    let mut taken: HashSet<_> = HashSet::new();
    if handling == RawHandling::Render {
        for f in &files {
            let ext = f
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if matches!(ext.as_str(), "jpg" | "jpeg" | "heic" | "heif") {
                taken.insert(key(f));
            }
        }
    }

    let kept = files
        .into_iter()
        .filter(|f| {
            if !crate::scanner::is_raw_file(f) {
                return true;
            }
            match handling {
                RawHandling::Upload => true,
                RawHandling::Skip => {
                    selection.skipped += 1;
                    false
                }
                RawHandling::Render => {
                    if taken.insert(key(f)) {
                        selection.rendered += 1;
                        true
                    } else {
                        selection.skipped_with_sibling += 1;
                        false
                    }
                }
            }
        })
        .collect();
    (kept, selection)
}

/// Tell the user what's happening to their RAW files, if anything unusual.
pub fn print_raw_selection(selection: &RawSelection, mode: RawMode, has_smugmug_source: bool) {
    if selection.rendered > 0 {
        println!(
            "{} Uploading {} RAW files as JPEGs (the preview the camera embedded in each)",
            "•".cyan(),
            selection.rendered
        );
    }
    if selection.skipped_with_sibling > 0 {
        println!(
            "{} Skipping {} RAW files that have a JPEG or HEIC of the same name next to them",
            "•".cyan(),
            selection.skipped_with_sibling
        );
    }
    if selection.skipped > 0 {
        let reason = if mode == RawMode::Original && !has_smugmug_source {
            "RAW originals need a SmugMug Source subscription"
        } else {
            "raw_mode is \"skip\""
        };
        println!(
            "{}",
            format!("⚠ Skipping {} RAW files: {}", selection.skipped, reason).yellow()
        );
        if mode == RawMode::Original && !has_smugmug_source {
            println!(
                "  {}",
                "(Set raw_mode = \"auto\" in the config to upload JPEGs rendered from them, or run 'smugmug-cli init' if you have Source)"
                    .bright_black()
            );
        }
    }
    if selection.rendered + selection.skipped + selection.skipped_with_sibling > 0 {
        println!();
    }
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

    // Files already on SmugMug in other albums are collected into the series
    // instead of uploaded again. Needs the cache or --check-remote to find
    // them, and the listing above to know what the albums already hold.
    let mut files = options.files;
    let mut collected = 0;
    let mut collect_failed = 0;
    let mut known_hashes = None;
    if remote_ok && (!options.no_cache || options.check_remote) {
        let store = hash_store.lock().await.clone();
        let target = collect::TargetContents {
            album_keys: options
                .series
                .existing_albums()
                .await
                .into_iter()
                .map(|a| a.album.album_key)
                .collect(),
            file_names: remote_images
                .iter()
                .flat_map(|images| images.keys().map(String::as_str))
                .collect(),
            md5s: remote_md5s.as_deref(),
        };
        let collect_options = collect::CollectOptions {
            use_cache: !options.no_cache,
            search: options.check_remote,
            render_raw: options.raw_handling == RawHandling::Render,
            dry_run: options.dry_run,
        };
        let outcome = collect::plan_collects(
            files,
            &target,
            &store,
            &collect_options,
            options.client.as_ref(),
            options.series.as_ref(),
        )
        .await?;
        files = outcome.to_upload;
        collected = outcome.collected;
        collect_failed = outcome.failed;
        known_hashes = Some(Arc::new(outcome.hashes));
    }

    // Setup progress bars
    let multi_progress = MultiProgress::new();
    let overall_progress = multi_progress.add(ProgressBar::new(files.len() as u64));
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
        render_raw: options.raw_handling == RawHandling::Render,
        skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        known_hashes,
    });

    let mut queue = UploadQueue::new();
    for file in files {
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
        collected,
        skipped: final_stats.skipped,
        failed: final_stats.failed + collect_failed,
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
    pub raw_mode: RawMode,
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
    let mut all_files: Vec<PathBuf> = Vec::new();

    fn scan_directory_recursive(dir: &std::path::Path, all_files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Recursively scan subdirectories
                scan_directory_recursive(&path, all_files)?;
            } else if path.is_file() {
                // Check if this is a supported file using the scanner module
                if crate::scanner::is_supported_file(&path) {
                    all_files.push(path);
                }
            }
        }
        Ok(())
    }

    scan_directory_recursive(&options.path, &mut all_files)?;

    let raw_handling = options.raw_mode.handling(options.has_smugmug_source);
    let (all_files, raw_selection) = select_raw_files(all_files, raw_handling);
    print_raw_selection(&raw_selection, options.raw_mode, options.has_smugmug_source);

    let mut folder_map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for path in all_files {
        let parent = path.parent().unwrap().to_path_buf();
        folder_map.entry(parent).or_default().push(path);
    }

    if folder_map.is_empty() {
        println!("No image files found to upload");
        return Ok(UploadStats {
            total_files: 0,
            uploaded: 0,
            replaced: 0,
            collected: 0,
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
        collected: 0,
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
            render_raw: raw_handling == RawHandling::Render,
            skip_raw_files: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            known_hashes: None,
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
        collected: 0,
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
    #[derive(serde::Deserialize)]
    struct NodeInfo {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "Type")]
        node_type: String,
        #[serde(rename = "Uri")]
        uri: String,
    }

    // Get every child of the parent node (all pages)
    let children_url = format!("https://api.smugmug.com{}!children", parent_node_uri);
    let nodes: Vec<NodeInfo> = client.get_all_pages(&children_url, "Node").await?;

    // Find the folder with the matching name
    for node in nodes {
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
    fn test_select_raw_files_upload_and_skip() {
        let files = vec![
            PathBuf::from("a.jpg"),
            PathBuf::from("b.CR2"),
            PathBuf::from("c.png"),
            PathBuf::from("d.nef"),
        ];

        let (kept, selection) = select_raw_files(files.clone(), RawHandling::Skip);
        assert_eq!(kept, vec![PathBuf::from("a.jpg"), PathBuf::from("c.png")]);
        assert_eq!(selection.skipped, 2);

        let (kept, selection) = select_raw_files(files.clone(), RawHandling::Upload);
        assert_eq!(kept, files);
        assert_eq!(selection, RawSelection::default());
    }

    #[test]
    fn test_select_raw_files_render_skips_siblings() {
        let files = vec![
            PathBuf::from("/p/IMG_1.CR2"),
            PathBuf::from("/p/img_1.JPG"), // camera JPEG for IMG_1
            PathBuf::from("/p/IMG_2.CR2"),
            PathBuf::from("/p/IMG_2.dng"), // renders to the same name as IMG_2.CR2
            PathBuf::from("/q/IMG_1.CR2"), // other directory, no JPEG there
            PathBuf::from("/q/IMG_3.nef"),
            PathBuf::from("/q/IMG_3.heic"),
        ];

        let (kept, selection) = select_raw_files(files, RawHandling::Render);
        assert_eq!(
            kept,
            vec![
                PathBuf::from("/p/img_1.JPG"),
                PathBuf::from("/p/IMG_2.CR2"),
                PathBuf::from("/q/IMG_1.CR2"),
                PathBuf::from("/q/IMG_3.heic"),
            ]
        );
        assert_eq!(
            selection,
            RawSelection {
                rendered: 2,
                skipped: 0,
                skipped_with_sibling: 3,
            }
        );
    }

    #[test]
    fn test_upload_stats_initial() {
        let stats = UploadStats {
            total_files: 10,
            uploaded: 0,
            replaced: 0,
            collected: 0,
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
            collected: 0,
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
            raw_mode: RawMode::Auto,
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
            collected: 0,
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
            collected: 0,
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
            collected: 0,
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
            collected: 0,
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
