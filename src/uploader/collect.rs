//! Adding photos that are already on SmugMug to the target album
//! ("collecting" them) instead of uploading a second copy.
//!
//! SmugMug doesn't deduplicate uploads: the same file uploaded to two albums
//! becomes two separate images. An image can, however, be collected into
//! any number of albums. Before the upload workers start, `plan_collects`
//! picks out files that already exist elsewhere in the account and collects
//! them, spending as few requests as possible:
//!
//! - Files the local cache says were uploaded to another album are collected
//!   from the cached image URI, with no lookup at all.
//! - With `--check-remote`, other files are looked up with `image!search`
//!   over exact capture-time ranges (see `capture_time`): photos taken within
//!   `BURST_GAP` of each other share one search, and matches are confirmed
//!   by MD5. Files without an EXIF capture time are just uploaded.
//! - Collects are sent in batches of `COLLECT_BATCH` per album.
//!
//! Collected images take room in the album series like uploads do. Whatever
//! can't be collected goes on to the upload workers.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use colored::Colorize;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::api::SmugMugClient;
use crate::api::albums::Album;
use crate::api::images::{CollectResult, SearchImage};
use crate::cache::hash_store::{HashStore, UploadedFile};
use crate::uploader::album_series::{AlbumSeries, AlbumSeriesBackend};
use crate::uploader::capture_time::{read_capture_time, smugmug_capture_span};
use crate::uploader::worker::{calculate_file_hash, calculate_md5_hash};

/// Photos taken this close together are looked up with one search.
const BURST_GAP: Duration = Duration::minutes(5);

/// Image URIs per `!collectimages` request.
const COLLECT_BATCH: usize = 100;

/// How searches and collects reach SmugMug.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait CollectBackend {
    /// The account URI searches are scoped to.
    async fn search_scope(&self) -> Result<String>;
    async fn search(
        &self,
        scope: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<SearchImage>>;
    async fn collect(&self, album_key: &str, uris: &[String]) -> Result<CollectResult>;
}

impl CollectBackend for SmugMugClient {
    async fn search_scope(&self) -> Result<String> {
        let user = self.get_auth_user().await?;
        user["Response"]["User"]["Uri"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Couldn't find the account's URI"))
    }

    async fn search(
        &self,
        scope: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<SearchImage>> {
        self.search_images_taken_between(scope, start, end).await
    }

    async fn collect(&self, album_key: &str, uris: &[String]) -> Result<CollectResult> {
        self.collect_images(album_key, uris).await
    }
}

/// What the target albums already hold, as listed before the upload.
pub struct TargetContents<'a> {
    /// Keys of the series' albums.
    pub album_keys: HashSet<String>,
    /// File names of images in them.
    pub file_names: HashSet<&'a str>,
    /// MD5s of images in them (only listed with `--check-remote`).
    pub md5s: Option<&'a HashMap<String, String>>,
}

pub struct CollectOptions {
    /// Read the cache (false with `--no-cache`; collects are still recorded).
    pub use_cache: bool,
    /// Search the account for files the cache doesn't know (`--check-remote`).
    pub search: bool,
    /// RAW files are uploaded as rendered JPEGs, whose bytes aren't stable
    /// enough to look up by MD5.
    pub render_raw: bool,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct CollectOutcome {
    /// Files left for the upload workers.
    pub to_upload: Vec<PathBuf>,
    /// SHA-256 of files the planner hashed, so the workers needn't again.
    pub hashes: HashMap<PathBuf, String>,
    /// Files collected (on a dry run: that would be).
    pub collected: usize,
    /// `image!search` requests made (not counting extra pages).
    pub searches: usize,
    /// Files that couldn't be collected because a request failed. They're
    /// neither collected nor uploaded (that would duplicate them); their
    /// cache entries stay, so the next run tries again.
    pub failed: usize,
}

/// A file to collect and the image it's already on SmugMug as.
struct Pending {
    path: PathBuf,
    hash: String,
    file_size: u64,
    image_uri: String,
    image_key: String,
    /// Found through the cache (rather than a search).
    from_cache: bool,
}

/// A file to look up with a search.
struct Candidate {
    path: PathBuf,
    hash: String,
    md5: String,
    file_size: u64,
    span: (DateTime<Utc>, DateTime<Utc>),
}

/// What the local pass found out about one file.
enum Local {
    Upload,
    Collect(Pending),
    Search(Candidate),
}

/// Collect the files in `files` that already exist elsewhere on SmugMug into
/// the series' albums, and return the rest for uploading.
pub async fn plan_collects<C: CollectBackend, S: AlbumSeriesBackend>(
    files: Vec<PathBuf>,
    target: &TargetContents<'_>,
    store: &HashStore,
    options: &CollectOptions,
    backend: &C,
    series: &AlbumSeries<S>,
) -> Result<CollectOutcome> {
    let mut outcome = CollectOutcome::default();

    // Local pass: hashes, cache and EXIF, in parallel and without requests.
    let classified: Vec<(PathBuf, Option<String>, Local)> = files
        .into_par_iter()
        .map(|path| {
            let (hash, local) = classify(&path, target, store, options);
            (path, hash, local)
        })
        .collect();

    let mut pending = Vec::new();
    let mut candidates = Vec::new();
    for (path, hash, local) in classified {
        if let Some(hash) = hash {
            outcome.hashes.insert(path.clone(), hash);
        }
        match local {
            Local::Upload => outcome.to_upload.push(path),
            Local::Collect(p) => pending.push(p),
            Local::Search(c) => candidates.push(c),
        }
    }

    if !candidates.is_empty() {
        let (found, not_found, searches) = search_bursts(candidates, backend).await;
        outcome.searches = searches;
        pending.extend(found);
        outcome.to_upload.extend(not_found);
    }

    if !pending.is_empty() {
        let collect = collect_pending(pending, store, options, backend, series).await?;
        outcome.collected = collect.collected;
        outcome.failed = collect.failed;
        outcome.to_upload.extend(collect.refused);
    }

    Ok(outcome)
}

fn classify(
    path: &PathBuf,
    target: &TargetContents<'_>,
    store: &HashStore,
    options: &CollectOptions,
) -> (Option<String>, Local) {
    // Same name already in the album: the worker skips or replaces it.
    let file_name = path.file_name().map(|n| n.to_string_lossy());
    if file_name.is_some_and(|n| target.file_names.contains(n.as_ref())) {
        return (None, Local::Upload);
    }
    // Errors here resurface (and are reported) in the worker.
    let Ok(hash) = calculate_file_hash(path) else {
        return (None, Local::Upload);
    };
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    if options.use_cache
        && let Ok(Some(cached)) = store.get(&hash)
    {
        // Already in the series: the worker skips it.
        if target.album_keys.contains(&cached.album_key) {
            return (Some(hash), Local::Upload);
        }
        let pending = Pending {
            path: path.clone(),
            hash: hash.clone(),
            file_size,
            image_uri: cached.smugmug_uri,
            image_key: cached.image_key,
            from_cache: true,
        };
        return (Some(hash), Local::Collect(pending));
    }

    let rendered = options.render_raw && crate::scanner::is_raw_file(path);
    if !options.search || rendered {
        return (Some(hash), Local::Upload);
    }
    let Ok(md5) = calculate_md5_hash(path) else {
        return (Some(hash), Local::Upload);
    };
    // Same content already in the album: the worker skips it.
    if target.md5s.is_some_and(|m| m.contains_key(&md5)) {
        return (Some(hash), Local::Upload);
    }
    let Some(camera_time) = read_capture_time(path) else {
        return (Some(hash), Local::Upload);
    };
    let candidate = Candidate {
        path: path.clone(),
        hash: hash.clone(),
        md5,
        file_size,
        span: smugmug_capture_span(camera_time),
    };
    (Some(hash), Local::Search(candidate))
}

/// Group candidates into bursts of photos taken within `BURST_GAP` of each
/// other, each covering exactly its first to last capture time.
fn bursts(mut candidates: Vec<Candidate>) -> Vec<(DateTime<Utc>, DateTime<Utc>, Vec<Candidate>)> {
    candidates.sort_by_key(|c| c.span);
    let mut bursts: Vec<(DateTime<Utc>, DateTime<Utc>, Vec<Candidate>)> = Vec::new();
    for candidate in candidates {
        match bursts.last_mut() {
            Some((_, end, members)) if candidate.span.0 - *end <= BURST_GAP => {
                *end = (*end).max(candidate.span.1);
                members.push(candidate);
            }
            _ => bursts.push((candidate.span.0, candidate.span.1, vec![candidate])),
        }
    }
    bursts
}

/// Look the candidates up, one search per burst. Returns the files found
/// (to collect), the rest (to upload) and the number of searches made.
async fn search_bursts<C: CollectBackend>(
    candidates: Vec<Candidate>,
    backend: &C,
) -> (Vec<Pending>, Vec<PathBuf>, usize) {
    let mut found = Vec::new();
    let mut not_found = Vec::new();
    let mut searches = 0;

    let scope = match backend.search_scope().await {
        Ok(scope) => scope,
        Err(e) => {
            println!("Warning: Can't search SmugMug for existing copies: {}", e);
            return (found, candidates.into_iter().map(|c| c.path).collect(), 0);
        }
    };

    let bursts = bursts(candidates);
    println!(
        "Searching SmugMug for {} files already uploaded elsewhere ({} {})...",
        bursts.iter().map(|b| b.2.len()).sum::<usize>(),
        bursts.len(),
        if bursts.len() == 1 {
            "search"
        } else {
            "searches"
        }
    );
    for (start, end, members) in bursts {
        searches += 1;
        let images = match backend.search(&scope, start, end).await {
            Ok(images) => images,
            Err(e) => {
                println!(
                    "Warning: Search failed, uploading those files instead: {}",
                    e
                );
                not_found.extend(members.into_iter().map(|c| c.path));
                continue;
            }
        };
        let by_md5: HashMap<String, &SearchImage> = images
            .iter()
            .filter(|i| i.collectable)
            .filter_map(|i| Some((i.archived_md5.as_ref()?.to_lowercase(), i)))
            .collect();
        for candidate in members {
            match by_md5.get(&candidate.md5) {
                Some(image) => found.push(Pending {
                    path: candidate.path,
                    hash: candidate.hash,
                    file_size: candidate.file_size,
                    image_uri: image.uri.clone(),
                    image_key: image.image_key.clone(),
                    from_cache: false,
                }),
                None => not_found.push(candidate.path),
            }
        }
    }
    (found, not_found, searches)
}

/// How collecting went.
#[derive(Default)]
struct Collected {
    collected: usize,
    /// Files SmugMug refused to collect (the image is gone or can't be
    /// collected), to upload instead.
    refused: Vec<PathBuf>,
    /// Files not collected because a request failed.
    failed: usize,
}

/// Claim room for each file in the series and collect them album by album.
async fn collect_pending<C: CollectBackend, S: AlbumSeriesBackend>(
    pending: Vec<Pending>,
    store: &HashStore,
    options: &CollectOptions,
    backend: &C,
    series: &AlbumSeries<S>,
) -> Result<Collected> {
    // Group by album, keeping the series' order.
    let mut groups: Vec<(Album, Vec<Pending>)> = Vec::new();
    for p in pending {
        let album = series.claim().await?;
        match groups.iter_mut().find(|(a, _)| a.name == album.name) {
            Some((_, members)) => members.push(p),
            None => groups.push((album, vec![p])),
        }
    }

    let total: usize = groups.iter().map(|(_, m)| m.len()).sum();
    if options.dry_run {
        return Ok(Collected {
            collected: total,
            ..Default::default()
        });
    }

    println!("Adding {} files already on SmugMug to the album...", total);
    let mut outcome = Collected::default();
    for (album, members) in groups {
        let mut members = members.into_iter().peekable();
        while members.peek().is_some() {
            let batch: Vec<Pending> = members.by_ref().take(COLLECT_BATCH).collect();
            let batch = collect_batch(batch, &album, store, backend).await;
            outcome.collected += batch.collected;
            for _ in 0..batch.refused.len() + batch.failed.len() {
                series.release(&album).await;
            }
            outcome
                .refused
                .extend(batch.refused.into_iter().map(|p| p.path));
            outcome.failed += batch.failed.len();
        }
    }

    if !outcome.refused.is_empty() {
        println!(
            "{}",
            format!(
                "⚠ SmugMug refused to add {} files (no longer there?); uploading them instead",
                outcome.refused.len()
            )
            .yellow()
        );
    }
    if outcome.failed > 0 {
        println!(
            "{}",
            format!(
                "⚠ {} files couldn't be added from SmugMug; run the upload again to retry",
                outcome.failed
            )
            .yellow()
        );
    }
    Ok(outcome)
}

/// How one batch went.
struct BatchOutcome {
    collected: usize,
    refused: Vec<Pending>,
    failed: Vec<Pending>,
}

/// Collect one batch into `album`, recording each collected file in the
/// cache.
async fn collect_batch<C: CollectBackend>(
    batch: Vec<Pending>,
    album: &Album,
    store: &HashStore,
    backend: &C,
) -> BatchOutcome {
    let uris: Vec<String> = batch.iter().map(|p| p.image_uri.clone()).collect();
    let (mut good, refused) = match backend.collect(&album.album_key, &uris).await {
        Ok(result) => batch
            .into_iter()
            .partition::<Vec<Pending>, _>(|p| !result.rejected.contains_key(&p.image_uri)),
        Err(e) => {
            println!("Warning: Couldn't add files to '{}': {}", album.name, e);
            return BatchOutcome {
                collected: 0,
                refused: Vec::new(),
                failed: batch,
            };
        }
    };

    // The cache points at an image that's gone or unusable: forget it, so
    // the worker uploads the file instead of skipping it as a duplicate.
    for p in refused.iter().filter(|p| p.from_cache) {
        let _ = store.remove(&p.hash);
    }

    // A refused URI fails the whole request, though SmugMug may have
    // collected the others; collecting them again settles it.
    let mut failed = Vec::new();
    if !refused.is_empty() && !good.is_empty() {
        let uris: Vec<String> = good.iter().map(|p| p.image_uri.clone()).collect();
        match backend.collect(&album.album_key, &uris).await {
            Ok(result) if result.rejected.is_empty() => {}
            outcome => {
                if let Err(e) = outcome {
                    println!("Warning: Couldn't add files to '{}': {}", album.name, e);
                }
                failed.append(&mut good);
            }
        }
    }

    for p in &good {
        let _ = store.insert(
            &p.hash,
            UploadedFile {
                smugmug_uri: p.image_uri.clone(),
                album_key: album.album_key.clone(),
                image_key: p.image_key.clone(),
                uploaded_at: Utc::now(),
                file_size: p.file_size,
                original_path: p.path.to_string_lossy().to_string(),
            },
        );
    }
    BatchOutcome {
        collected: good.len(),
        refused,
        failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uploader::album_series::MAX_ALBUM_IMAGES;
    use little_exif::exif_tag::ExifTag;
    use little_exif::metadata::Metadata;
    use std::sync::Mutex as StdMutex;

    /// In-memory SmugMug: images by URI, with MD5 and capture time.
    #[derive(Default)]
    struct FakeSmugMug {
        images: Vec<(SearchImage, DateTime<Utc>)>,
        /// URIs `collect` refuses ("Does not exist").
        missing: HashSet<String>,
        fail_collect: bool,
        /// Fail every collect after the first.
        fail_on_retry: bool,
        searches: StdMutex<Vec<(DateTime<Utc>, DateTime<Utc>)>>,
        /// (album key, URIs) per collect call.
        collects: StdMutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeSmugMug {
        fn with_image(mut self, key: &str, md5: &str, taken: &str) -> Self {
            self.images.push((
                SearchImage {
                    image_key: key.to_string(),
                    uri: format!("/api/v2/image/{}-0", key),
                    archived_md5: Some(md5.to_string()),
                    collectable: true,
                },
                DateTime::parse_from_rfc3339(taken).unwrap().to_utc(),
            ));
            self
        }
    }

    impl CollectBackend for FakeSmugMug {
        async fn search_scope(&self) -> Result<String> {
            Ok("/api/v2/user/test".to_string())
        }

        async fn search(
            &self,
            _scope: &str,
            start: DateTime<Utc>,
            end: DateTime<Utc>,
        ) -> Result<Vec<SearchImage>> {
            self.searches.lock().unwrap().push((start, end));
            Ok(self
                .images
                .iter()
                .filter(|(_, t)| *t >= start && *t <= end)
                .map(|(i, _)| i.clone())
                .collect())
        }

        async fn collect(&self, album_key: &str, uris: &[String]) -> Result<CollectResult> {
            self.collects
                .lock()
                .unwrap()
                .push((album_key.to_string(), uris.to_vec()));
            let calls = self.collects.lock().unwrap().len();
            if self.fail_collect || (self.fail_on_retry && calls > 1) {
                anyhow::bail!("503 Service Unavailable");
            }
            let rejected = uris
                .iter()
                .filter(|u| self.missing.contains(*u))
                .map(|u| (u.clone(), vec!["Does not exist".to_string()]))
                .collect();
            Ok(CollectResult { rejected })
        }
    }

    /// Albums of a series: name -> image count.
    struct FakeSeries(StdMutex<HashMap<String, u64>>);

    impl AlbumSeriesBackend for FakeSeries {
        async fn find_album(&self, name: &str) -> Result<Option<Album>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .contains_key(name)
                .then(|| album(name)))
        }
        async fn image_count(&self, album: &Album) -> Result<u64> {
            Ok(self.0.lock().unwrap()[&album.name])
        }
        async fn create_album(&self, name: &str) -> Result<Album> {
            self.0.lock().unwrap().insert(name.to_string(), 0);
            Ok(album(name))
        }
    }

    fn album(name: &str) -> Album {
        Album {
            album_key: format!("key-{}", name),
            name: name.to_string(),
            url_name: String::new(),
            node_id: String::new(),
            uri: format!("/api/v2/album/key-{}", name),
            web_uri: None,
            uris: None,
            image_count: None,
        }
    }

    async fn series(albums: &[(&str, u64)], capacity: u64) -> AlbumSeries<FakeSeries> {
        let backend = FakeSeries(StdMutex::new(
            albums.iter().map(|(n, c)| (n.to_string(), *c)).collect(),
        ));
        AlbumSeries::load(backend, "Trip", capacity, false)
            .await
            .unwrap()
    }

    /// A JPEG with the given content and, optionally, EXIF DateTimeOriginal.
    fn photo(dir: &std::path::Path, name: &str, content: &str, taken: Option<&str>) -> PathBuf {
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xFE, 0x00, (content.len() + 2) as u8]);
        jpeg.extend_from_slice(content.as_bytes());
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        if let Some(taken) = taken {
            let mut metadata = Metadata::new();
            metadata.set_tag(ExifTag::DateTimeOriginal(taken.into()));
            metadata
                .write_to_vec(&mut jpeg, little_exif::filetype::FileExtension::JPEG)
                .unwrap();
        }
        let path = dir.join(name);
        std::fs::write(&path, jpeg).unwrap();
        path
    }

    fn md5_of(path: &std::path::Path) -> String {
        calculate_md5_hash(path).unwrap()
    }

    fn cached(store: &HashStore, path: &std::path::Path, album_key: &str, image_key: &str) {
        store
            .insert(
                &calculate_file_hash(path).unwrap(),
                UploadedFile {
                    smugmug_uri: format!("/api/v2/image/{}-0", image_key),
                    album_key: album_key.to_string(),
                    image_key: image_key.to_string(),
                    uploaded_at: Utc::now(),
                    file_size: 1,
                    original_path: path.to_string_lossy().to_string(),
                },
            )
            .unwrap();
    }

    fn empty_target() -> TargetContents<'static> {
        TargetContents {
            album_keys: HashSet::from(["key-Trip".to_string()]),
            file_names: HashSet::new(),
            md5s: None,
        }
    }

    fn options(search: bool) -> CollectOptions {
        CollectOptions {
            use_cache: true,
            search,
            render_raw: false,
            dry_run: false,
        }
    }

    struct Setup {
        dir: tempfile::TempDir,
        store: HashStore,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let store = HashStore::new(&dir.path().join("cache").to_string_lossy()).unwrap();
        Setup { dir, store }
    }

    #[tokio::test]
    async fn test_cache_hit_elsewhere_is_collected_without_lookups() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        let b = photo(s.dir.path(), "b.jpg", "b", None);
        cached(&s.store, &a, "key-Other", "AAA");
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a.clone(), b.clone()],
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 1);
        assert_eq!(outcome.to_upload, vec![b]);
        assert_eq!(outcome.searches, 0);
        assert!(smugmug.searches.lock().unwrap().is_empty());
        assert_eq!(
            *smugmug.collects.lock().unwrap(),
            vec![(
                "key-Trip".to_string(),
                vec!["/api/v2/image/AAA-0".to_string()]
            )]
        );
        // The cache now knows the file is in this album.
        let entry = s.store.get(&outcome.hashes[&a]).unwrap().unwrap();
        assert_eq!(entry.album_key, "key-Trip");
        assert_eq!(series.albums().await[0].claimed, 1);
    }

    #[tokio::test]
    async fn test_cache_hit_in_target_or_same_name_is_left_to_worker() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        let b = photo(s.dir.path(), "b.jpg", "b", None);
        cached(&s.store, &a, "key-Trip", "AAA");
        cached(&s.store, &b, "key-Other", "BBB");
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;
        let target = TargetContents {
            file_names: HashSet::from(["b.jpg"]),
            ..empty_target()
        };

        let outcome = plan_collects(
            vec![a.clone(), b.clone()],
            &target,
            &s.store,
            &options(true),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 0);
        assert_eq!(outcome.to_upload.len(), 2);
        assert!(smugmug.collects.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_search_finds_by_capture_time_and_md5_in_one_request_per_burst() {
        let s = setup();
        // A burst of three (one not on SmugMug), and one taken a day later.
        let a = photo(s.dir.path(), "a.jpg", "a", Some("2026:05:01 12:00:00"));
        let b = photo(s.dir.path(), "b.jpg", "b", Some("2026:05:01 12:00:01"));
        let c = photo(s.dir.path(), "c.jpg", "c", Some("2026:05:01 12:03:00"));
        let d = photo(s.dir.path(), "d.jpg", "d", Some("2026:05:02 09:00:00"));
        let smugmug = FakeSmugMug::default()
            .with_image("AAA", &md5_of(&a), "2026-05-01T19:00:00Z")
            .with_image("CCC", &md5_of(&c), "2026-05-01T19:03:00Z")
            .with_image("DDD", &md5_of(&d), "2026-05-02T16:00:00Z")
            // Same second as a, different photo.
            .with_image("XXX", "0000", "2026-05-01T19:00:00Z");
        let series = series(&[], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![d.clone(), c.clone(), b.clone(), a.clone()],
            &empty_target(),
            &s.store,
            &options(true),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 3);
        assert_eq!(outcome.to_upload, vec![b]);
        assert_eq!(outcome.searches, 2);
        let utc = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().to_utc();
        assert_eq!(
            *smugmug.searches.lock().unwrap(),
            vec![
                (utc("2026-05-01T19:00:00Z"), utc("2026-05-01T19:03:00Z")),
                (utc("2026-05-02T16:00:00Z"), utc("2026-05-02T16:00:00Z")),
            ]
        );
        let collects = smugmug.collects.lock().unwrap();
        assert_eq!(collects.len(), 1);
        assert_eq!(collects[0].1.len(), 3);
    }

    #[tokio::test]
    async fn test_search_skips_files_without_capture_time_or_in_album() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        let b = photo(s.dir.path(), "b.jpg", "b", Some("2026:05:01 12:00:00"));
        let smugmug = FakeSmugMug::default().with_image("BBB", &md5_of(&b), "2026-05-01T19:00:00Z");
        let series = series(&[("Trip", 1)], MAX_ALBUM_IMAGES).await;
        let md5s = HashMap::from([(md5_of(&b), "BBB".to_string())]);
        let target = TargetContents {
            md5s: Some(&md5s),
            ..empty_target()
        };

        let outcome = plan_collects(
            vec![a, b],
            &target,
            &s.store,
            &options(true),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 0);
        assert_eq!(outcome.to_upload.len(), 2);
        assert!(smugmug.searches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_no_search_without_check_remote() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", Some("2026:05:01 12:00:00"));
        let smugmug = FakeSmugMug::default().with_image("AAA", &md5_of(&a), "2026-05-01T19:00:00Z");
        let series = series(&[], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a],
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 0);
        assert!(smugmug.searches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_stale_cache_entry_is_forgotten_and_uploaded() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        let b = photo(s.dir.path(), "b.jpg", "b", None);
        cached(&s.store, &a, "key-Other", "GONE");
        cached(&s.store, &b, "key-Other", "BBB");
        let smugmug = FakeSmugMug {
            missing: HashSet::from(["/api/v2/image/GONE-0".to_string()]),
            ..Default::default()
        };
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a.clone(), b.clone()],
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 1);
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.to_upload, vec![a.clone()]);
        assert!(s.store.get(&outcome.hashes[&a]).unwrap().is_none());
        // The refused file's room went back.
        assert_eq!(series.albums().await[0].claimed, 1);
        // The good one is collected again after the refused batch.
        let collects = smugmug.collects.lock().unwrap();
        assert_eq!(collects.len(), 2);
        assert_eq!(collects[1].1, vec!["/api/v2/image/BBB-0".to_string()]);
    }

    #[tokio::test]
    async fn test_failed_collect_fails_without_uploading_or_forgetting() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        cached(&s.store, &a, "key-Other", "AAA");
        let smugmug = FakeSmugMug {
            fail_collect: true,
            ..Default::default()
        };
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a.clone()],
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        // Not uploaded (it's on SmugMug; that would duplicate it) and not
        // forgotten, so the next run tries again; reported as failed.
        assert_eq!(outcome.collected, 0);
        assert_eq!(outcome.failed, 1);
        assert!(outcome.to_upload.is_empty());
        assert!(s.store.get(&outcome.hashes[&a]).unwrap().is_some());
        assert_eq!(series.albums().await[0].claimed, 0);
    }

    #[tokio::test]
    async fn test_failed_retry_after_refusal_counts_as_failed() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        let b = photo(s.dir.path(), "b.jpg", "b", None);
        cached(&s.store, &a, "key-Other", "GONE");
        cached(&s.store, &b, "key-Other", "BBB");
        let smugmug = FakeSmugMug {
            missing: HashSet::from(["/api/v2/image/GONE-0".to_string()]),
            fail_on_retry: true,
            ..Default::default()
        };
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a.clone(), b.clone()],
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        // a was refused: uploaded, and its stale entry forgotten. b's retry
        // failed: kept for the next run.
        assert_eq!(outcome.collected, 0);
        assert_eq!(outcome.to_upload, vec![a.clone()]);
        assert_eq!(outcome.failed, 1);
        assert!(s.store.get(&outcome.hashes[&a]).unwrap().is_none());
        assert!(s.store.get(&outcome.hashes[&b]).unwrap().is_some());
        assert_eq!(series.albums().await[0].claimed, 0);
    }

    #[tokio::test]
    async fn test_collects_roll_over_into_the_next_album() {
        let s = setup();
        let files: Vec<PathBuf> = (0..3)
            .map(|i| {
                let p = photo(s.dir.path(), &format!("{i}.jpg"), &i.to_string(), None);
                cached(&s.store, &p, "key-Other", &format!("K{i}"));
                p
            })
            .collect();
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 4)], 5).await;

        let outcome = plan_collects(
            files,
            &empty_target(),
            &s.store,
            &options(false),
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 3);
        let collects = smugmug.collects.lock().unwrap();
        let per_album: Vec<(String, usize)> =
            collects.iter().map(|(k, u)| (k.clone(), u.len())).collect();
        assert_eq!(
            per_album,
            vec![("key-Trip".to_string(), 1), ("key-Trip (2)".to_string(), 2)]
        );
    }

    #[tokio::test]
    async fn test_dry_run_collects_nothing() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg", "a", None);
        cached(&s.store, &a, "key-Other", "AAA");
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a],
            &empty_target(),
            &s.store,
            &CollectOptions {
                dry_run: true,
                ..options(false)
            },
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 1);
        assert!(outcome.to_upload.is_empty());
        assert!(smugmug.collects.lock().unwrap().is_empty());
    }
}
