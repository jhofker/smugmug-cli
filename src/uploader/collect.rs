//! Adding photos that are already on SmugMug to the target album
//! ("collecting" them) instead of uploading a second copy.
//!
//! The cache maps each uploaded file to its SmugMug image, and the upload
//! workers skip any file it knows. So a file uploaded to album A and then
//! uploaded to album B would be skipped and never appear in B. SmugMug
//! doesn't deduplicate uploads (uploading it again would store a second,
//! separate image), but an image can be collected into any number of
//! albums. Before the workers start, `plan_collects` collects files the
//! cache knows are in another album into the target album, from the cached
//! image URI, in batches of `COLLECT_BATCH` per album.
//!
//! Collected images take room in the album series like uploads do. Files
//! SmugMug refuses (the image was deleted) go on to the upload workers.

use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::api::SmugMugClient;
use crate::api::albums::Album;
use crate::api::images::CollectResult;
use crate::cache::hash_store::{HashStore, UploadedFile};
use crate::uploader::album_series::{AlbumSeries, AlbumSeriesBackend};
use crate::uploader::worker::calculate_file_hash;

/// Image URIs per `!collectimages` request.
const COLLECT_BATCH: usize = 100;

/// How collects reach SmugMug.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait CollectBackend {
    async fn collect(&self, album_key: &str, uris: &[String]) -> Result<CollectResult>;
}

impl CollectBackend for SmugMugClient {
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
}

#[derive(Debug, Default)]
pub struct CollectOutcome {
    /// Files left for the upload workers.
    pub to_upload: Vec<PathBuf>,
    /// SHA-256 of files the planner hashed, so the workers needn't again.
    pub hashes: HashMap<PathBuf, String>,
    /// Files collected (on a dry run: that would be).
    pub collected: usize,
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
}

/// Collect the files in `files` that the cache knows are in another album
/// into the series' albums, and return the rest for uploading.
pub async fn plan_collects<C: CollectBackend, S: AlbumSeriesBackend>(
    files: Vec<PathBuf>,
    target: &TargetContents<'_>,
    store: &HashStore,
    dry_run: bool,
    backend: &C,
    series: &AlbumSeries<S>,
) -> Result<CollectOutcome> {
    let mut outcome = CollectOutcome::default();

    // Hash and look up every file, in parallel and without requests.
    let classified: Vec<(PathBuf, Option<String>, Option<Pending>)> = files
        .into_par_iter()
        .map(|path| {
            let (hash, pending) = classify(&path, target, store);
            (path, hash, pending)
        })
        .collect();

    let mut pending = Vec::new();
    for (path, hash, collect) in classified {
        if let Some(hash) = hash {
            outcome.hashes.insert(path.clone(), hash);
        }
        match collect {
            Some(p) => pending.push(p),
            None => outcome.to_upload.push(path),
        }
    }

    if !pending.is_empty() {
        let collect = collect_pending(pending, store, dry_run, backend, series).await?;
        outcome.collected = collect.collected;
        outcome.failed = collect.failed;
        outcome.to_upload.extend(collect.refused);
    }

    Ok(outcome)
}

/// The file's hash, and what to collect if the cache knows the file is in
/// another album. Everything else is left to the worker.
fn classify(
    path: &PathBuf,
    target: &TargetContents<'_>,
    store: &HashStore,
) -> (Option<String>, Option<Pending>) {
    // Same name already in the album: the worker skips or replaces it.
    let file_name = path.file_name().map(|n| n.to_string_lossy());
    if file_name.is_some_and(|n| target.file_names.contains(n.as_ref())) {
        return (None, None);
    }
    // Errors here resurface (and are reported) in the worker.
    let Ok(hash) = calculate_file_hash(path) else {
        return (None, None);
    };
    let Ok(Some(cached)) = store.get(&hash) else {
        return (Some(hash), None);
    };
    // Already in the series: the worker skips it.
    if target.album_keys.contains(&cached.album_key) {
        return (Some(hash), None);
    }
    let pending = Pending {
        path: path.clone(),
        hash: hash.clone(),
        file_size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        image_uri: cached.smugmug_uri,
        image_key: cached.image_key,
    };
    (Some(hash), Some(pending))
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
    dry_run: bool,
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
    if dry_run {
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
    for p in &refused {
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
    use std::sync::Mutex as StdMutex;

    /// In-memory SmugMug collects.
    #[derive(Default)]
    struct FakeSmugMug {
        /// URIs `collect` refuses ("Does not exist").
        missing: HashSet<String>,
        fail_collect: bool,
        /// Fail every collect after the first.
        fail_on_retry: bool,
        /// (album key, URIs) per collect call.
        collects: StdMutex<Vec<(String, Vec<String>)>>,
    }

    impl CollectBackend for FakeSmugMug {
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

    fn photo(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("photo {}", name)).unwrap();
        path
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
    async fn test_cache_hit_elsewhere_is_collected() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg");
        let b = photo(s.dir.path(), "b.jpg");
        cached(&s.store, &a, "key-Other", "AAA");
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(
            vec![a.clone(), b.clone()],
            &empty_target(),
            &s.store,
            false,
            &smugmug,
            &series,
        )
        .await
        .unwrap();

        assert_eq!(outcome.collected, 1);
        assert_eq!(outcome.to_upload, vec![b.clone()]);
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
        // Both files were hashed for the workers.
        assert!(outcome.hashes.contains_key(&b));
        assert_eq!(series.albums().await[0].claimed, 1);
    }

    #[tokio::test]
    async fn test_cache_hit_in_target_or_same_name_is_left_to_worker() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg");
        let b = photo(s.dir.path(), "b.jpg");
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
            false,
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
    async fn test_stale_cache_entry_is_forgotten_and_uploaded() {
        let s = setup();
        let a = photo(s.dir.path(), "a.jpg");
        let b = photo(s.dir.path(), "b.jpg");
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
            false,
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
        let a = photo(s.dir.path(), "a.jpg");
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
            false,
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
        let a = photo(s.dir.path(), "a.jpg");
        let b = photo(s.dir.path(), "b.jpg");
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
            false,
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
                let p = photo(s.dir.path(), &format!("{i}.jpg"));
                cached(&s.store, &p, "key-Other", &format!("K{i}"));
                p
            })
            .collect();
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 4)], 5).await;

        let outcome = plan_collects(files, &empty_target(), &s.store, false, &smugmug, &series)
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
        let a = photo(s.dir.path(), "a.jpg");
        cached(&s.store, &a, "key-Other", "AAA");
        let smugmug = FakeSmugMug::default();
        let series = series(&[("Trip", 0)], MAX_ALBUM_IMAGES).await;

        let outcome = plan_collects(vec![a], &empty_target(), &s.store, true, &smugmug, &series)
            .await
            .unwrap();

        assert_eq!(outcome.collected, 1);
        assert!(outcome.to_upload.is_empty());
        assert!(smugmug.collects.lock().unwrap().is_empty());
    }
}
