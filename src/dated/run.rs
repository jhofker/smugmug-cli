//! One pass of a dated upload (see `crate::dated`).
//!
//! For each file, cheapest check first:
//! 1. Same size and modification time as the index records → done, unread.
//! 2. Read it (SHA-256 + MD5 in one pass; a RAW file whole). Same content
//!    as the index records → re-record it (touched or copied), done.
//! 3. Content already on SmugMug (the hash store knows it) → link it if it's
//!    in its day's albums, else collect it there. A RAW file isn't rendered.
//! 4. A RAW file whose rendered JPEG is what this path uploaded last time
//!    (metadata-only edit) → re-record, no upload.
//! 5. Changed content at a path that was uploaded → replace that image,
//!    unless another path shares it (then upload anew).
//! 6. Otherwise upload into the day's album, under its own name, or renamed
//!    if a different photo has that name there.

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use colored::Colorize;
use futures_util::future::join_all;
use indicatif::{ProgressBar, ProgressStyle};
use md5::Context as Md5Context;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::api::SmugMugClient;
use crate::api::upload::{
    FileUpload, UploadPayload, UploadRejected, UploadResult, UploadTarget, upload_bytes,
    upload_file,
};
use crate::cache::hash_store::{HashStore, UploadedFile};
use crate::config::{LivePhotoVideos, RawHandling};
use crate::dated::date::{self, DateSource};
use crate::dated::index::{FileIndex, FileRecord};
use crate::dated::plan::{DayAlbums, NameCheck, Smug};
use crate::dated::scan::{self, ScannedFile};
use crate::uploader::collect::{COLLECT_BATCH, CollectBackend, Pending, collect_batch};

/// Set to stop a run early: no new files are read; files already read are
/// finished.
pub type StopFlag = Arc<AtomicBool>;

pub struct RunOptions {
    pub sources: Vec<PathBuf>,
    pub excludes: Vec<String>,
    /// SmugMug folder path the year folders go in, e.g. "Backup".
    pub root_folder: String,
    /// Privacy for the root folder if it's created (`None`: SmugMug's
    /// default). Year/month folders and day albums are always private.
    pub root_privacy: Option<&'static str>,
    pub upload_threads: usize,
    pub read_threads: usize,
    pub dry_run: bool,
    /// Ignore the index and hash store (still checks file names and MD5s in
    /// the day albums, so nothing is uploaded twice).
    pub no_cache: bool,
    pub raw_handling: RawHandling,
    pub live_photo_videos: LivePhotoVideos,
    pub retry_attempts: u32,
    pub cache_path: PathBuf,
}

/// How a run went. Serialized as the backup's `last_run.json`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct RunStats {
    /// Supported files found (after exclusions and RAW/Live Photo choices).
    pub scanned: usize,
    pub unsupported: usize,
    pub scan_errors: Vec<String>,
    pub raw_rendered: usize,
    pub raw_skipped: usize,
    pub raw_skipped_with_sibling: usize,
    pub live_photo_videos_skipped: usize,
    /// Same size and modification time as last time: not read.
    pub unchanged: usize,
    /// Unchanged files SmugMug refused before (not retried).
    pub previously_refused: usize,
    /// New or changed files (read, or on a dry run, dated).
    pub to_process: usize,
    pub bytes_to_process: u64,
    /// Changed size or time but not content (or not rendered JPEG).
    pub touched: usize,
    pub uploaded: usize,
    pub replaced: usize,
    /// Already in its day album (an identical copy uploaded before).
    pub linked: usize,
    /// Already on SmugMug in another album; added to its day album.
    pub collected: usize,
    pub failed: usize,
    /// Refused by SmugMug (too big, unsupported); not retried until the
    /// file changes.
    pub refused: usize,
    pub bytes_uploaded: u64,
    pub albums_created: usize,
    /// How the processed files were dated.
    pub date_sources: BTreeMap<String, usize>,
    /// Processed files per capture day.
    pub days: BTreeMap<NaiveDate, usize>,
    /// The first few failures.
    pub errors: Vec<String>,
    pub duration_secs: u64,
    /// Stopped early (signal).
    pub interrupted: bool,
}

const MAX_ERRORS: usize = 20;

impl RunStats {
    fn error(&mut self, path: &Path, e: impl std::fmt::Display) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(format!("{}: {}", path.display(), e));
        }
    }

    fn dated(&mut self, date: NaiveDate, source: DateSource) {
        *self.days.entry(date).or_default() += 1;
        *self
            .date_sources
            .entry(source.describe().to_string())
            .or_default() += 1;
    }
}

/// What a new upload carries.
pub enum Content {
    /// The file itself, streamed from disk.
    File,
    /// A JPEG rendered from a RAW file.
    Rendered(bytes::Bytes),
}

/// Sending files to SmugMug.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait Uploads {
    async fn upload(
        &self,
        target: UploadTarget<'_>,
        path: &Path,
        content: &Content,
        size: u64,
        md5: &str,
        name: &str,
    ) -> Result<UploadResult>;

    /// The root folder's node URI, created if `create` (else `None` when
    /// missing).
    async fn root_folder(
        &self,
        path: &str,
        create: bool,
        privacy: Option<&str>,
    ) -> Result<Option<String>>;
}

impl Uploads for SmugMugClient {
    async fn upload(
        &self,
        target: UploadTarget<'_>,
        path: &Path,
        content: &Content,
        size: u64,
        md5: &str,
        name: &str,
    ) -> Result<UploadResult> {
        match content {
            Content::File => {
                upload_file(
                    self,
                    target,
                    &FileUpload {
                        path,
                        size,
                        md5_hex: md5,
                        file_name: name,
                    },
                )
                .await
            }
            Content::Rendered(data) => {
                upload_bytes(
                    self,
                    target,
                    &UploadPayload {
                        data: data.clone(),
                        file_name: name.to_string(),
                        mime_type: "image/jpeg".to_string(),
                    },
                )
                .await
            }
        }
    }

    async fn root_folder(
        &self,
        path: &str,
        create: bool,
        privacy: Option<&str>,
    ) -> Result<Option<String>> {
        if create {
            self.find_or_create_folder_path(path, privacy)
                .await
                .map(Some)
        } else {
            self.find_folder_path(path).await
        }
    }
}

/// A file that isn't known to be unchanged.
struct PendingFile {
    file: ScannedFile,
    previous: Option<FileRecord>,
}

enum Kind {
    /// Same content as the index records for this path.
    Same,
    /// A RAW file whose rendered JPEG (MD5 `md5`) is what this path
    /// uploaded last time.
    SameRender { md5: String },
    /// Content already on SmugMug.
    Known(UploadedFile),
    Upload {
        content: Content,
        name: String,
        md5: String,
        size: u64,
    },
}

/// A file read and classified, ready for the upload stage.
struct Prepared {
    file: ScannedFile,
    previous: Option<FileRecord>,
    sha256: String,
    date: NaiveDate,
    date_source: DateSource,
    kind: Kind,
}

impl Prepared {
    fn record(
        &self,
        uploaded_md5: Option<String>,
        image_uri: Option<String>,
        album_key: Option<String>,
        failed: Option<String>,
    ) -> FileRecord {
        FileRecord {
            size: self.file.size,
            mtime_ns: self.file.mtime_ns,
            sha256: self.sha256.clone(),
            uploaded_md5,
            capture_date: self.date,
            date_source: self.date_source,
            image_uri,
            album_key,
            failed,
        }
    }

    fn uploaded_file(&self, image_uri: &str, album_key: &str, size: u64) -> UploadedFile {
        UploadedFile {
            smugmug_uri: image_uri.to_string(),
            album_key: album_key.to_string(),
            image_key: image_uri.rsplit('/').next().unwrap_or("").to_string(),
            uploaded_at: Utc::now(),
            file_size: size,
            original_path: self.file.path.to_string_lossy().to_string(),
        }
    }
}

/// SHA-256 and MD5 of a file in one pass, and its length.
fn hash_file(path: &Path) -> std::io::Result<(String, String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut sha = Sha256::new();
    let mut md5 = Md5Context::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
        md5.consume(&buffer[..n]);
        len += n as u64;
    }
    Ok((
        hex::encode(sha.finalize()),
        format!("{:x}", md5.finalize()),
        len,
    ))
}

/// The day a file goes under: a Live Photo video's photo's, else its own.
fn date_for(file: &ScannedFile, data: Option<&[u8]>, index: &FileIndex) -> (NaiveDate, DateSource) {
    if let Some(photo) = &file.live_photo_of {
        if let Ok(Some(record)) = index.get(photo) {
            return (record.capture_date, DateSource::LivePhoto);
        }
        if let Some(date) = date::metadata_date(photo) {
            return (date, DateSource::LivePhoto);
        }
    }
    date::capture_date(&file.path, data, file.mtime_ns)
}

/// What readers need.
#[derive(Clone)]
struct ReadContext {
    store: HashStore,
    index: FileIndex,
    render_raw: bool,
    no_cache: bool,
}

/// Read one file and decide what it needs (steps 2–4 in the module docs).
fn prepare(pending: PendingFile, ctx: &ReadContext) -> Result<Prepared> {
    let PendingFile { file, previous } = pending;
    let same = |sha: &str| {
        previous
            .as_ref()
            .is_some_and(|r| r.sha256 == sha && r.is_settled())
    };
    let known = |sha: &str| -> Result<Option<UploadedFile>> {
        if ctx.no_cache {
            Ok(None)
        } else {
            ctx.store.get(sha)
        }
    };

    if ctx.render_raw && crate::scanner::is_raw_file(&file.path) {
        let data = std::fs::read(&file.path).context("Failed to read file")?;
        let sha256 = hex::encode(Sha256::digest(&data));
        let (date, date_source) = date_for(&file, Some(&data), &ctx.index);
        let kind = if same(&sha256) {
            Kind::Same
        } else if let Some(cached) = known(&sha256)? {
            Kind::Known(cached)
        } else {
            let rendered = crate::raw::render_jpeg_bytes(&data)
                .context("Failed to render RAW file to JPEG")?
                .data;
            let md5 = format!("{:x}", md5::compute(&rendered));
            let same_render = previous.as_ref().is_some_and(|r| {
                r.image_uri.is_some() && r.uploaded_md5.as_deref() == Some(md5.as_str())
            });
            if same_render {
                Kind::SameRender { md5 }
            } else {
                Kind::Upload {
                    size: rendered.len() as u64,
                    content: Content::Rendered(rendered.into()),
                    name: crate::raw::rendered_file_name(&file.path),
                    md5,
                }
            }
        };
        return Ok(Prepared {
            file,
            previous,
            sha256,
            date,
            date_source,
            kind,
        });
    }

    let (sha256, md5, size) = hash_file(&file.path).context("Failed to read file")?;
    let (date, date_source) = date_for(&file, None, &ctx.index);
    let kind = if same(&sha256) {
        Kind::Same
    } else if let Some(cached) = known(&sha256)? {
        Kind::Known(cached)
    } else {
        Kind::Upload {
            content: Content::File,
            name: file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_string()),
            md5,
            size,
        }
    };
    Ok(Prepared {
        file,
        previous,
        sha256,
        date,
        date_source,
        kind,
    })
}

/// Shared by the upload workers.
struct Workers<'a, T> {
    smug: &'a T,
    days: DayAlbums<T>,
    store: HashStore,
    index: FileIndex,
    options: &'a RunOptions,
    stats: std::sync::Mutex<RunStats>,
    /// Content (SHA-256) being uploaded → files with the same content
    /// waiting for it, so identical copies read at the same time are
    /// uploaded once.
    in_flight: std::sync::Mutex<HashMap<String, Vec<Prepared>>>,
    /// Files to collect into each day's albums, sent in batches.
    collects: tokio::sync::Mutex<HashMap<NaiveDate, Vec<(Prepared, UploadedFile)>>>,
    progress: ProgressBar,
}

impl<T: Smug + Uploads + CollectBackend> Workers<'_, T> {
    fn stats(&self) -> std::sync::MutexGuard<'_, RunStats> {
        self.stats.lock().unwrap()
    }

    fn put(&self, prepared: &Prepared, record: &FileRecord) {
        if let Err(e) = self.index.put(&prepared.file.path, record) {
            self.stats().error(&prepared.file.path, e);
        }
    }

    async fn handle(&self, prepared: Prepared) {
        self.stats().dated(prepared.date, prepared.date_source);
        match &prepared.kind {
            Kind::Same => {
                let mut record = prepared.previous.clone().expect("Same needs a record");
                record.size = prepared.file.size;
                record.mtime_ns = prepared.file.mtime_ns;
                self.put(&prepared, &record);
                self.stats().touched += 1;
            }
            Kind::SameRender { md5 } => {
                let previous = prepared
                    .previous
                    .as_ref()
                    .expect("SameRender needs a record");
                let uri = previous.image_uri.clone().unwrap_or_default();
                let album_key = previous.album_key.clone().unwrap_or_default();
                let size = prepared.file.size;
                let _ = self.store.insert_deferred(
                    &prepared.sha256,
                    &prepared.uploaded_file(&uri, &album_key, size),
                );
                let record = prepared.record(
                    Some(md5.clone()),
                    previous.image_uri.clone(),
                    previous.album_key.clone(),
                    None,
                );
                self.put(&prepared, &record);
                self.stats().touched += 1;
            }
            Kind::Known(cached) => {
                let cached = cached.clone();
                self.place_known(prepared, cached).await;
            }
            Kind::Upload { .. } => self.upload(prepared).await,
        }
        self.progress.inc(1);
    }

    /// A file whose content is already on SmugMug as `cached`: link it if
    /// that's in its day's albums, otherwise collect it there.
    async fn place_known(&self, prepared: Prepared, cached: UploadedFile) {
        let day = match self.days.day(prepared.date).await {
            Ok(day) => day,
            Err(e) => return self.fail_with(&prepared, &e),
        };
        if day.album_keys().await.contains(&cached.album_key) {
            let record = prepared.record(
                None,
                Some(cached.smugmug_uri.clone()),
                Some(cached.album_key.clone()),
                None,
            );
            self.put(&prepared, &record);
            self.stats().linked += 1;
            return;
        }
        let batch = {
            let mut collects = self.collects.lock().await;
            let pending = collects.entry(prepared.date).or_default();
            pending.push((prepared, cached));
            if pending.len() >= COLLECT_BATCH {
                std::mem::take(pending)
            } else {
                Vec::new()
            }
        };
        if !batch.is_empty() {
            self.collect(batch).await;
        }
    }

    /// Collect files (all of one day) into that day's albums.
    async fn collect(&self, items: Vec<(Prepared, UploadedFile)>) {
        let Some(date) = items.first().map(|(p, _)| p.date) else {
            return;
        };
        let day = match self.days.day(date).await {
            Ok(day) => day,
            Err(e) => {
                for (p, _) in &items {
                    self.fail_with(p, &e);
                }
                return;
            }
        };

        // Claim room album by album, like uploads do.
        let mut groups: Vec<(crate::api::albums::Album, Vec<(Prepared, UploadedFile)>)> =
            Vec::new();
        for (prepared, cached) in items {
            let album = match day.series.claim().await {
                Ok(album) => album,
                Err(e) => {
                    self.fail_with(&prepared, &e);
                    continue;
                }
            };
            match groups.iter_mut().find(|(a, _)| a.name == album.name) {
                Some((_, members)) => members.push((prepared, cached)),
                None => groups.push((album, vec![(prepared, cached)])),
            }
        }

        for (album, members) in groups {
            let pending: Vec<Pending> = members
                .iter()
                .map(|(p, cached)| Pending {
                    path: p.file.path.clone(),
                    hash: p.sha256.clone(),
                    file_size: p.file.size,
                    image_uri: cached.smugmug_uri.clone(),
                    image_key: cached.image_key.clone(),
                })
                .collect();
            let outcome = collect_batch(pending, &album, &self.store, self.smug).await;
            let not_done: HashMap<PathBuf, &'static str> = outcome
                .refused
                .iter()
                .map(|p| {
                    (
                        p.path.clone(),
                        "no longer on SmugMug; it'll be uploaded on the next run",
                    )
                })
                .chain(
                    outcome
                        .failed
                        .iter()
                        .map(|p| (p.path.clone(), "couldn't be added to its album; will retry")),
                )
                .collect();
            for (prepared, cached) in members {
                match not_done.get(&prepared.file.path) {
                    Some(reason) => {
                        day.series.release(&album).await;
                        self.fail(&prepared, reason);
                    }
                    None => {
                        let record = prepared.record(
                            None,
                            Some(cached.smugmug_uri.clone()),
                            Some(album.album_key.clone()),
                            None,
                        );
                        self.put(&prepared, &record);
                        self.stats().collected += 1;
                    }
                }
            }
        }
    }

    async fn flush_collects(&self) {
        let batches: Vec<_> = self.collects.lock().await.drain().map(|(_, v)| v).collect();
        for mut batch in batches {
            while !batch.is_empty() {
                let rest = batch.split_off(batch.len().min(COLLECT_BATCH));
                self.collect(batch).await;
                batch = rest;
            }
        }
    }

    async fn upload(&self, prepared: Prepared) {
        // Another copy finished uploading since this one was read.
        if !self.options.no_cache
            && let Ok(Some(cached)) = self.store.get(&prepared.sha256)
        {
            return self.place_known(prepared, cached).await;
        }
        // Another copy is uploading right now: wait for it.
        let sha = prepared.sha256.clone();
        let prepared = {
            let mut in_flight = self.in_flight.lock().unwrap();
            match in_flight.get_mut(&sha) {
                Some(waiting) => {
                    waiting.push(prepared);
                    return;
                }
                None => {
                    in_flight.insert(sha.clone(), Vec::new());
                    prepared
                }
            }
        };

        let uploaded = self.upload_one(&prepared).await;

        let waiting = self
            .in_flight
            .lock()
            .unwrap()
            .remove(&sha)
            .unwrap_or_default();
        // (Their progress was counted when they were queued.)
        for copy in waiting {
            match &uploaded {
                Some(cached) => self.place_known(copy, cached.clone()).await,
                None => self.fail(&copy, "an identical file failed to upload"),
            }
        }
    }

    /// Upload or replace; the image it's now on SmugMug as, if it worked.
    async fn upload_one(&self, prepared: &Prepared) -> Option<UploadedFile> {
        let Kind::Upload {
            content,
            name,
            md5,
            size,
        } = &prepared.kind
        else {
            return None;
        };

        // Changed content at a path that was uploaded: replace its image,
        // unless another path is that image too.
        if let Some(previous) = &prepared.previous
            && let Some(old_uri) = &previous.image_uri
            && self.index.refs(old_uri).unwrap_or(u32::MAX) <= 1
        {
            match self
                .send(
                    UploadTarget::ReplaceImage(old_uri),
                    prepared,
                    content,
                    *size,
                    md5,
                    name,
                )
                .await
            {
                Ok(result) => {
                    let album_key = previous.album_key.clone().unwrap_or_default();
                    let cached = prepared.uploaded_file(&result.image_uri, &album_key, *size);
                    if self
                        .store
                        .get(&previous.sha256)
                        .ok()
                        .flatten()
                        .is_some_and(|c| &c.smugmug_uri == old_uri)
                    {
                        let _ = self.store.remove_deferred(&previous.sha256);
                    }
                    let _ = self.store.insert_deferred(&prepared.sha256, &cached);
                    let record = prepared.record(
                        Some(md5.clone()),
                        Some(result.image_uri),
                        Some(album_key),
                        None,
                    );
                    self.put(prepared, &record);
                    let mut stats = self.stats();
                    stats.replaced += 1;
                    stats.bytes_uploaded += size;
                    return Some(cached);
                }
                // The image is gone: upload it anew.
                Err(e)
                    if e.downcast_ref::<UploadRejected>()
                        .is_some_and(|r| r.status == 404) => {}
                Err(e) => {
                    self.fail_with(prepared, &e);
                    return None;
                }
            }
        }

        let day = match self.days.day(prepared.date).await {
            Ok(day) => day,
            Err(e) => {
                self.fail_with(prepared, &e);
                return None;
            }
        };
        let name = match day.check_name(name, md5).await {
            Ok(NameCheck::Present(image)) => {
                let cached = prepared.uploaded_file(&image.uri, &image.album_key, *size);
                let _ = self.store.insert_deferred(&prepared.sha256, &cached);
                let record = prepared.record(
                    Some(md5.clone()),
                    Some(image.uri),
                    Some(image.album_key),
                    None,
                );
                self.put(prepared, &record);
                self.stats().linked += 1;
                return Some(cached);
            }
            Ok(NameCheck::Upload(name)) => name,
            Err(e) => {
                self.fail_with(prepared, &e);
                return None;
            }
        };
        let album = match day.series.claim().await {
            Ok(album) => album,
            Err(e) => {
                day.release_name(&name).await;
                self.fail_with(prepared, &e);
                return None;
            }
        };
        match self
            .send(
                UploadTarget::Album(&album.uri),
                prepared,
                content,
                *size,
                md5,
                &name,
            )
            .await
        {
            Ok(result) => {
                day.name_uploaded(&name, &result.image_uri, &album.album_key)
                    .await;
                let cached = prepared.uploaded_file(&result.image_uri, &album.album_key, *size);
                let _ = self.store.insert_deferred(&prepared.sha256, &cached);
                let record = prepared.record(
                    Some(md5.clone()),
                    Some(result.image_uri),
                    Some(album.album_key.clone()),
                    None,
                );
                self.put(prepared, &record);
                let mut stats = self.stats();
                stats.uploaded += 1;
                stats.bytes_uploaded += size;
                Some(cached)
            }
            Err(e) => {
                day.series.release(&album).await;
                day.release_name(&name).await;
                self.fail_with(prepared, &e);
                None
            }
        }
    }

    /// Upload with retries for transient failures (network, 429, 5xx).
    async fn send(
        &self,
        target: UploadTarget<'_>,
        prepared: &Prepared,
        content: &Content,
        size: u64,
        md5: &str,
        name: &str,
    ) -> Result<UploadResult> {
        let attempts = self.options.retry_attempts.max(1);
        let mut attempt = 1;
        loop {
            match self
                .smug
                .upload(target, &prepared.file.path, content, size, md5, name)
                .await
            {
                Ok(result) => return Ok(result),
                Err(e) if attempt < attempts && is_transient(&e, target) => {
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Count a failure, to be retried next run.
    fn fail(&self, prepared: &Prepared, reason: &str) {
        let mut stats = self.stats();
        stats.failed += 1;
        stats.error(&prepared.file.path, reason);
    }

    /// Count a failed request. A file SmugMug refuses outright (too big,
    /// unsupported) is recorded so it isn't retried until it changes;
    /// anything else is retried next run.
    fn fail_with(&self, prepared: &Prepared, error: &anyhow::Error) {
        let refused = error
            .downcast_ref::<UploadRejected>()
            .is_some_and(UploadRejected::is_permanent);
        if !refused {
            return self.fail(prepared, &format!("{:#}", error));
        }
        let record = prepared.record(None, None, None, Some(error.to_string()));
        self.put(prepared, &record);
        let mut stats = self.stats();
        stats.refused += 1;
        stats.error(&prepared.file.path, error);
    }
}

/// Worth trying again. A new upload is only retried when SmugMug can't have
/// stored it (rate limited, or the connection never opened): after a 5xx or
/// a timeout the image may exist, and sending it again would duplicate it.
/// The next run finds such an image by name and MD5 and links it instead.
/// Replacing an image's content is safe to repeat.
fn is_transient(error: &anyhow::Error, target: UploadTarget<'_>) -> bool {
    let repeatable = matches!(target, UploadTarget::ReplaceImage(_));
    if let Some(rejected) = error.downcast_ref::<UploadRejected>() {
        return rejected.status == 429 || (repeatable && rejected.status >= 500);
    }
    match error.downcast_ref::<reqwest::Error>() {
        Some(e) if e.is_connect() => true,
        Some(e) => repeatable && (e.is_timeout() || e.is_request() || e.is_body()),
        // Reading the file failed: trying again won't help
        None => false,
    }
}

fn progress_bar(len: u64) -> ProgressBar {
    let bar = ProgressBar::new(len);
    bar.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
    );
    bar
}

/// Run once over `options.sources`.
pub async fn run<T: Smug + Uploads + CollectBackend>(
    smug: Arc<T>,
    options: &RunOptions,
    stop: &StopFlag,
) -> Result<RunStats> {
    let started = Instant::now();
    let store = HashStore::new(&options.cache_path.to_string_lossy())
        .context("Failed to open the cache")?;
    let index = FileIndex::open(&store)?;
    let mut stats = RunStats::default();

    // Scan (stat only).
    let scanned = {
        let sources = options.sources.clone();
        let excludes = options.excludes.clone();
        let raw = options.raw_handling;
        let live = options.live_photo_videos;
        tokio::task::spawn_blocking(move || scan::scan(&sources, &excludes, raw, live)).await??
    };
    stats.scanned = scanned.files.len();
    stats.unsupported = scanned.unsupported;
    stats.scan_errors = scanned.errors;
    stats.raw_rendered = scanned.raw_selection.rendered;
    stats.raw_skipped = scanned.raw_selection.skipped;
    stats.raw_skipped_with_sibling = scanned.raw_selection.skipped_with_sibling;
    stats.live_photo_videos_skipped = scanned.live_photo_videos_skipped;

    // Leave out files the index knows are unchanged.
    let (pending, unchanged, previously_refused) = {
        let index = index.clone();
        let no_cache = options.no_cache;
        let files = scanned.files;
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut pending = VecDeque::new();
            let (mut unchanged, mut refused) = (0, 0);
            for file in files {
                let previous = if no_cache {
                    None
                } else {
                    index.get(&file.path)?
                };
                match &previous {
                    Some(r) if r.matches(&file) && r.is_settled() => {
                        unchanged += 1;
                        if r.failed.is_some() {
                            refused += 1;
                        }
                    }
                    _ => pending.push_back(PendingFile { file, previous }),
                }
            }
            Ok((pending, unchanged, refused))
        })
        .await??
    };
    stats.unchanged = unchanged;
    stats.previously_refused = previously_refused;
    stats.to_process = pending.len();
    stats.bytes_to_process = pending.iter().map(|p| p.file.size).sum();

    if pending.is_empty() {
        stats.duration_secs = started.elapsed().as_secs();
        return Ok(stats);
    }

    if options.dry_run {
        let dated = dry_run_dates(pending, options.read_threads, &index, stop).await?;
        for (date, source) in dated {
            stats.dated(date, source);
        }
        stats.interrupted = stop.load(Ordering::Relaxed);
        stats.duration_secs = started.elapsed().as_secs();
        return Ok(stats);
    }

    let root = smug
        .root_folder(&options.root_folder, true, options.root_privacy)
        .await
        .with_context(|| format!("Failed to find or create folder {}", options.root_folder))?;
    let progress = progress_bar(pending.len() as u64);
    let workers = Workers {
        smug: smug.as_ref(),
        days: DayAlbums::new(smug.clone(), root, false),
        store: store.clone(),
        index: index.clone(),
        options,
        stats: std::sync::Mutex::new(stats),
        in_flight: std::sync::Mutex::new(HashMap::new()),
        collects: tokio::sync::Mutex::new(HashMap::new()),
        progress: progress.clone(),
    };

    // Readers: a few blocking threads reading files in path order.
    let queue = Arc::new(std::sync::Mutex::new(pending));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Prepared, (PathBuf, anyhow::Error)>>(
        options.upload_threads.max(1) * 2,
    );
    let read_ctx = ReadContext {
        store: store.clone(),
        index: index.clone(),
        render_raw: options.raw_handling == RawHandling::Render,
        no_cache: options.no_cache,
    };
    let readers: Vec<_> = (0..options.read_threads.max(1))
        .map(|_| {
            let queue = queue.clone();
            let tx = tx.clone();
            let ctx = read_ctx.clone();
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(next) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let path = next.file.path.clone();
                    let result = prepare(next, &ctx).map_err(|e| (path, e));
                    if tx.blocking_send(result).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);

    // Uploaders: several workers on this task (uploads are I/O-bound).
    let rx = tokio::sync::Mutex::new(rx);
    let uploaders = (0..options.upload_threads.max(1)).map(|_| async {
        loop {
            let next = rx.lock().await.recv().await;
            match next {
                Some(Ok(prepared)) => workers.handle(prepared).await,
                Some(Err((path, e))) => {
                    let mut stats = workers.stats();
                    stats.failed += 1;
                    stats.error(&path, format!("{:#}", e));
                    drop(stats);
                    workers.progress.inc(1);
                }
                None => break,
            }
        }
    });

    let done = tokio::sync::Notify::new();
    let ticker = async {
        if std::io::stderr().is_terminal() {
            return;
        }
        loop {
            tokio::select! {
                _ = done.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    let s = workers.stats();
                    println!(
                        "… {}/{} processed: {} uploaded ({}), {} replaced, {} already there, {} added from other albums, {} failed",
                        progress.position(),
                        progress.length().unwrap_or(0),
                        s.uploaded,
                        human_bytes(s.bytes_uploaded),
                        s.replaced,
                        s.linked + s.touched,
                        s.collected,
                        s.failed + s.refused,
                    );
                }
            }
        }
    };

    let work = async {
        let (_, readers) = tokio::join!(join_all(uploaders), join_all(readers));
        for reader in readers {
            if let Err(e) = reader {
                eprintln!("{}", format!("A reader thread failed: {}", e).red());
            }
        }
        workers.flush_collects().await;
        done.notify_one();
    };
    tokio::join!(work, ticker);
    progress.finish_and_clear();

    store.flush()?;
    let mut stats = workers.stats.into_inner().unwrap();
    for day in workers.days.loaded_days() {
        stats.albums_created += day
            .series
            .albums()
            .await
            .iter()
            .filter(|a| a.created)
            .count();
    }
    stats.interrupted = stop.load(Ordering::Relaxed);
    stats.duration_secs = started.elapsed().as_secs();
    Ok(stats)
}

/// A dry run only dates files (reading their metadata, not their content)
/// and touches nothing on SmugMug.
async fn dry_run_dates(
    pending: VecDeque<PendingFile>,
    threads: usize,
    index: &FileIndex,
    stop: &StopFlag,
) -> Result<Vec<(NaiveDate, DateSource)>> {
    let queue = Arc::new(std::sync::Mutex::new(pending));
    let handles: Vec<_> = (0..threads.max(1))
        .map(|_| {
            let queue = queue.clone();
            let index = index.clone();
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    let Some(next) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    out.push(date_for(&next.file, None, &index));
                }
                out
            })
        })
        .collect();
    let mut dated = Vec::new();
    for handle in handles {
        dated.extend(handle.await?);
    }
    Ok(dated)
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::images::{AlbumImage, CollectResult};
    use crate::dated::plan::tests::FakeSmug;
    use std::fs;
    use std::time::SystemTime;
    use tempfile::TempDir;

    impl Uploads for FakeSmug {
        async fn upload(
            &self,
            target: UploadTarget<'_>,
            _path: &Path,
            _content: &Content,
            size: u64,
            md5: &str,
            name: &str,
        ) -> Result<UploadResult> {
            if self.refuse.lock().unwrap().contains(name) {
                return Err(UploadRejected {
                    status: 413,
                    body: "too big".into(),
                }
                .into());
            }
            match target {
                UploadTarget::Album(album_uri) => {
                    self.calls
                        .lock()
                        .unwrap()
                        .push(format!("upload {} {}", album_uri, name));
                    let key = album_uri.rsplit('/').next().unwrap().to_string();
                    let uri = format!("{}/image/{}-{}", album_uri, self.id(), name);
                    self.images
                        .lock()
                        .unwrap()
                        .entry(key.clone())
                        .or_default()
                        .push(AlbumImage {
                            image_key: uri.rsplit('/').next().unwrap().into(),
                            file_name: name.into(),
                            archived_uri: String::new(),
                            file_size: size,
                            format: "JPG".into(),
                            uri: uri.clone(),
                            title: None,
                            archived_md5: Some(md5.into()),
                        });
                    Ok(UploadResult {
                        image_key: uri.rsplit('/').next().unwrap().into(),
                        image_uri: uri,
                        status_code: 200,
                    })
                }
                UploadTarget::ReplaceImage(uri) => {
                    self.calls.lock().unwrap().push(format!("replace {}", uri));
                    Ok(UploadResult {
                        image_key: uri.rsplit('/').next().unwrap().into(),
                        image_uri: uri.into(),
                        status_code: 200,
                    })
                }
            }
        }

        async fn root_folder(
            &self,
            _path: &str,
            _create: bool,
            _privacy: Option<&str>,
        ) -> Result<Option<String>> {
            Ok(Some("/node/root".into()))
        }
    }

    impl CollectBackend for FakeSmug {
        async fn collect(&self, album_key: &str, uris: &[String]) -> Result<CollectResult> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("collect {} {}", album_key, uris.len()));
            Ok(CollectResult::default())
        }
    }

    struct Setup {
        photos: TempDir,
        cache: TempDir,
        smug: Arc<FakeSmug>,
    }

    impl Setup {
        fn new() -> Self {
            let (smug, _) = FakeSmug::with_root();
            Setup {
                photos: TempDir::new().unwrap(),
                cache: TempDir::new().unwrap(),
                smug,
            }
        }

        fn write(&self, rel: &str, data: &[u8]) -> PathBuf {
            let path = self.photos.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, data).unwrap();
            path
        }

        fn options(&self) -> RunOptions {
            RunOptions {
                sources: vec![self.photos.path().to_path_buf()],
                excludes: Vec::new(),
                root_folder: "Backup".into(),
                root_privacy: Some("Private"),
                upload_threads: 4,
                read_threads: 2,
                dry_run: false,
                no_cache: false,
                raw_handling: RawHandling::Render,
                live_photo_videos: LivePhotoVideos::Upload,
                retry_attempts: 1,
                cache_path: self.cache.path().join("db"),
            }
        }

        async fn run(&self) -> RunStats {
            self.run_with(self.options()).await
        }

        async fn run_with(&self, options: RunOptions) -> RunStats {
            let stop: StopFlag = Arc::new(AtomicBool::new(false));
            let stats = run(self.smug.clone(), &options, &stop).await.unwrap();
            assert!(
                stats.errors.is_empty() || stats.failed + stats.refused > 0,
                "{:?}",
                stats.errors
            );
            stats
        }

        fn calls(&self, prefix: &str) -> usize {
            self.smug.calls(prefix)
        }

        fn album_names(&self) -> Vec<String> {
            let nodes = self.smug.nodes.lock().unwrap();
            let mut names: Vec<String> = nodes
                .values()
                .flatten()
                .filter(|c| c.node_type == "Album")
                .map(|c| c.name.clone())
                .collect();
            names.sort();
            names
        }
    }

    fn set_mtime(path: &Path, secs_ago: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
            .unwrap();
    }

    #[tokio::test]
    async fn uploads_into_day_albums_then_skips_unchanged_files_unread() {
        let s = Setup::new();
        s.write("a/IMG_20140712_0001.jpg", b"one");
        s.write("b/IMG_20140712_0002.jpg", b"two");
        s.write("b/IMG_20150101_0003.jpg", b"three");

        let stats = s.run().await;
        assert_eq!(stats.uploaded, 3);
        assert_eq!(stats.albums_created, 2);
        assert_eq!(s.album_names(), vec!["2014-07-12", "2015-01-01"]);

        // Second run: nothing read, nothing sent, no lookups at all.
        let calls_before = s.smug.calls.lock().unwrap().len();
        let stats = s.run().await;
        assert_eq!(stats.unchanged, 3);
        assert_eq!(stats.to_process, 0);
        assert_eq!(s.smug.calls.lock().unwrap().len(), calls_before);
    }

    #[tokio::test]
    async fn touched_files_are_rerecorded_not_uploaded() {
        let s = Setup::new();
        let path = s.write("IMG_20140712_0001.jpg", b"one");
        s.run().await;
        set_mtime(&path, 3600);

        let stats = s.run().await;
        assert_eq!(stats.touched, 1);
        assert_eq!(s.calls("upload"), 1);

        // And now it's unchanged again.
        assert_eq!(s.run().await.unchanged, 1);
    }

    #[tokio::test]
    async fn edited_files_replace_their_image() {
        let s = Setup::new();
        let path = s.write("IMG_20140712_0001.jpg", b"one");
        s.run().await;
        fs::write(&path, b"one, edited").unwrap();
        set_mtime(&path, 10);

        let stats = s.run().await;
        assert_eq!(stats.replaced, 1);
        assert_eq!(s.calls("replace"), 1);
        assert_eq!(s.calls("upload"), 1);
    }

    #[tokio::test]
    async fn identical_copies_upload_once() {
        let s = Setup::new();
        for i in 0..6 {
            s.write(&format!("copy{}/IMG_20140712_0001.jpg", i), b"same bytes");
        }
        let stats = s.run().await;
        assert_eq!(stats.uploaded, 1);
        assert_eq!(stats.linked, 5);
        assert_eq!(s.calls("upload"), 1);
    }

    #[tokio::test]
    async fn editing_one_of_two_copies_uploads_it_anew() {
        let s = Setup::new();
        let a = s.write("a/IMG_20140712_0001.jpg", b"same");
        s.write("b/IMG_20140712_0001.jpg", b"same");
        s.run().await;
        fs::write(&a, b"different now").unwrap();
        set_mtime(&a, 10);

        let stats = s.run().await;
        // The shared image keeps b's content; a's edit goes up separately,
        // renamed since the name is taken by other content.
        assert_eq!(stats.replaced, 0);
        assert_eq!(stats.uploaded, 1);
        assert_eq!(s.calls("replace"), 0);
        assert!(
            s.smug
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.contains("IMG_20140712_0001~"))
        );
    }

    #[tokio::test]
    async fn same_name_different_content_is_renamed() {
        let s = Setup::new();
        s.write("cam1/IMG_20140712_0001.jpg", b"camera one");
        s.write("cam2/IMG_20140712_0001.jpg", b"camera two");
        let stats = s.run().await;
        assert_eq!(stats.uploaded, 2);
        let names: Vec<String> = s
            .smug
            .images
            .lock()
            .unwrap()
            .values()
            .flatten()
            .map(|i| i.file_name.clone())
            .collect();
        assert!(names.contains(&"IMG_20140712_0001.jpg".to_string()));
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("IMG_20140712_0001~") && n.ends_with(".jpg"))
        );
    }

    #[tokio::test]
    async fn content_already_in_the_day_album_is_linked_without_cache() {
        let s = Setup::new();
        s.write("IMG_20140712_0001.jpg", b"one");
        s.run().await;
        // A fresh cache (new container) finds it by name and MD5.
        let mut options = s.options();
        options.cache_path = s.cache.path().join("fresh");
        let stats = s.run_with(options).await;
        assert_eq!(stats.linked, 1);
        assert_eq!(s.calls("upload"), 1);
    }

    #[tokio::test]
    async fn files_in_other_albums_are_collected() {
        let s = Setup::new();
        let path = s.write("IMG_20140712_0001.jpg", b"one");
        // The cache knows this content from an album elsewhere.
        {
            let store = HashStore::new(&s.cache.path().join("db").to_string_lossy()).unwrap();
            let (sha, _, size) = hash_file(&path).unwrap();
            store
                .insert(
                    &sha,
                    UploadedFile {
                        smugmug_uri: "/api/v2/album/ELSEWHERE/image/X-0".into(),
                        album_key: "ELSEWHERE".into(),
                        image_key: "X-0".into(),
                        uploaded_at: Utc::now(),
                        file_size: size,
                        original_path: "elsewhere".into(),
                    },
                )
                .unwrap();
        }
        let stats = s.run().await;
        assert_eq!(stats.collected, 1);
        assert_eq!(s.calls("collect"), 1);
        assert_eq!(s.calls("upload"), 0);
        assert_eq!(s.run().await.unchanged, 1);
    }

    #[tokio::test]
    async fn live_photo_videos_go_to_their_photos_day() {
        let s = Setup::new();
        s.write("IMG_20140712_0001.HEIC", b"photo");
        let video = s.write("IMG_20140712_0001.MOV", b"video");
        // Without the pairing, the video's mtime would date it today.
        set_mtime(&video, 0);
        let stats = s.run().await;
        assert_eq!(stats.uploaded, 2);
        assert_eq!(s.album_names(), vec!["2014-07-12"]);
    }

    fn fake_raw(extra: &[u8]) -> Vec<u8> {
        let mut raw = crate::raw::exif::tests::fake_tiff_raw_be();
        raw.extend(crate::raw::jpeg::tests::fake_jpeg(0xC0, 6000, 4000, None));
        raw.extend_from_slice(extra);
        raw
    }

    #[tokio::test]
    async fn raw_files_are_rendered_once() {
        let s = Setup::new();
        let path = s.write("IMG_0001.CR2", &fake_raw(b""));
        let stats = s.run().await;
        assert_eq!(stats.uploaded, 1);
        // Dated from the RAW's EXIF.
        assert_eq!(s.album_names(), vec!["2024-06-01"]);
        let uploaded = s
            .smug
            .images
            .lock()
            .unwrap()
            .values()
            .flatten()
            .next()
            .unwrap()
            .clone();
        assert_eq!(uploaded.file_name, "IMG_0001.jpg");

        // Touched: same content, nothing rendered or sent.
        set_mtime(&path, 3600);
        assert_eq!(s.run().await.touched, 1);

        // Changed bytes outside the preview (a metadata edit): rendered
        // again, but the JPEG is the same, so nothing is sent.
        fs::write(&path, fake_raw(b"\0\0\0\0")).unwrap();
        set_mtime(&path, 10);
        let stats = s.run().await;
        assert_eq!(stats.touched, 1);
        assert_eq!(s.calls("upload") + s.calls("replace"), 1);

        // Moved: found by content, linked without rendering.
        let moved = s.write("moved/IMG_0001.CR2", &fs::read(&path).unwrap());
        fs::remove_file(&path).unwrap();
        let _ = moved;
        let stats = s.run().await;
        assert_eq!(stats.linked, 1);
        assert_eq!(s.calls("upload") + s.calls("replace"), 1);
    }

    #[tokio::test]
    async fn refused_files_are_not_retried_until_they_change() {
        let s = Setup::new();
        let path = s.write("IMG_20140712_0001.jpg", b"huge");
        s.smug
            .refuse
            .lock()
            .unwrap()
            .insert("IMG_20140712_0001.jpg".into());
        let stats = s.run().await;
        assert_eq!(stats.refused, 1);

        let stats = s.run().await;
        assert_eq!(stats.previously_refused, 1);
        assert_eq!(stats.to_process, 0);

        s.smug.refuse.lock().unwrap().clear();
        fs::write(&path, b"smaller").unwrap();
        set_mtime(&path, 10);
        assert_eq!(s.run().await.uploaded, 1);
    }

    #[tokio::test]
    async fn dry_run_dates_files_and_touches_nothing() {
        let s = Setup::new();
        s.write("IMG_20140712_0001.jpg", b"one");
        s.write("IMG_20140713_0001.jpg", b"two");
        let mut options = s.options();
        options.dry_run = true;
        let stats = s.run_with(options).await;
        assert_eq!(stats.to_process, 2);
        assert_eq!(stats.days.len(), 2);
        assert_eq!(stats.date_sources.get("file name"), Some(&2));
        assert!(s.smug.calls.lock().unwrap().is_empty());
        // Nothing recorded either.
        assert_eq!(s.run().await.uploaded, 2);
    }

    #[tokio::test]
    async fn excluded_paths_are_left_out() {
        let s = Setup::new();
        s.write("keep/IMG_20140712_0001.jpg", b"one");
        s.write("old_bak/IMG_20140712_0002.jpg", b"two");
        let mut options = s.options();
        options.excludes = vec!["old_bak/".into()];
        let stats = s.run_with(options).await;
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.uploaded, 1);
    }

    #[test]
    fn new_uploads_are_retried_only_when_nothing_can_have_been_stored() {
        let rejected = |status| {
            anyhow::Error::from(UploadRejected {
                status,
                body: String::new(),
            })
        };
        let album = UploadTarget::Album("/api/v2/album/A");
        let replace = UploadTarget::ReplaceImage("/api/v2/album/A/image/B-0");
        assert!(is_transient(&rejected(429), album));
        assert!(!is_transient(&rejected(500), album));
        assert!(!is_transient(&rejected(503), album));
        assert!(is_transient(&rejected(503), replace));
        assert!(!is_transient(&rejected(413), replace));
        assert!(!is_transient(&anyhow::anyhow!("file vanished"), replace));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
