//! Finding the files to upload: walking the sources without opening any
//! file (just a `stat` each), so a re-run over a big, unchanged library
//! costs a directory walk and nothing more.
//!
//! Excluded paths are pruned (an excluded directory is never entered), and
//! so are hidden files and directories (`.DS_Store`, AppleDouble `._*`
//! files, `.thumbnails`) and NAS metadata directories. A `.smugmugignore`
//! file in any directory adds .gitignore-style patterns for it.

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::config::{LivePhotoVideos, RawHandling};
use crate::uploader::{RawSelection, select_raw_files};

/// Name of the per-directory ignore file.
pub const IGNORE_FILE: &str = ".smugmugignore";

/// Always left out: NAS metadata and recycle bins, never photos of anyone's.
const DEFAULT_EXCLUDES: &[&str] = &["@eaDir/", "#recycle/", "#snapshot/", "$RECYCLE.BIN/"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// For the video half of a Live Photo: its photo.
    pub live_photo_of: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    /// Sorted by path, so files are read directory by directory.
    pub files: Vec<ScannedFile>,
    /// Files of types SmugMug doesn't take (sidecars, catalogs, documents).
    pub unsupported: usize,
    /// Entries that couldn't be read (permissions, vanished mid-walk).
    pub errors: Vec<String>,
    pub raw_selection: RawSelection,
    /// Live Photo videos left out (`LivePhotoVideos::Skip`).
    pub live_photo_videos_skipped: usize,
    /// Live Photo videos kept, dated by their photo.
    pub live_photo_videos: usize,
}

/// Walk `sources`, leaving out `excludes` (.gitignore syntax, relative to
/// each source).
pub fn scan(
    sources: &[PathBuf],
    excludes: &[String],
    raw_handling: RawHandling,
    live_photo_videos: LivePhotoVideos,
) -> Result<ScanResult> {
    let mut result = ScanResult::default();
    let mut found = Vec::new();

    for source in sources {
        if !source.exists() {
            anyhow::bail!("{} doesn't exist", source.display());
        }
        // A single file is just that file.
        if source.is_file() {
            match stat(source) {
                Ok(f) if crate::scanner::is_supported_file(source) => found.push(f),
                Ok(_) => result.unsupported += 1,
                Err(e) => result.errors.push(format!("{}: {}", source.display(), e)),
            }
            continue;
        }

        let mut overrides = OverrideBuilder::new(source);
        for pattern in DEFAULT_EXCLUDES
            .iter()
            .copied()
            .chain(excludes.iter().map(String::as_str))
        {
            // In an override, a plain glob whitelists; "!" makes it ignore.
            overrides
                .add(&format!("!{}", pattern))
                .with_context(|| format!("Invalid exclude pattern '{}'", pattern))?;
        }
        let overrides = overrides.build().context("Invalid exclude patterns")?;

        let walker = WalkBuilder::new(source)
            .standard_filters(false)
            .hidden(true)
            .follow_links(true)
            .add_custom_ignore_filename(IGNORE_FILE)
            .overrides(overrides)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    result.errors.push(e.to_string());
                    continue;
                }
            };
            if !entry
                .file_type()
                .is_some_and(|t| t.is_file() || t.is_symlink())
            {
                continue;
            }
            let path = entry.path();
            if !crate::scanner::is_supported_file(path) {
                result.unsupported += 1;
                continue;
            }
            match stat(path) {
                Ok(f) => found.push(f),
                Err(e) => result.errors.push(format!("{}: {}", path.display(), e)),
            }
        }
    }

    // Overlapping sources would list a file twice.
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);

    // RAW+JPEG pairs keep the JPEG (see `select_raw_files`).
    let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
    let (kept, raw_selection) = select_raw_files(paths, raw_handling);
    result.raw_selection = raw_selection;
    let kept: HashSet<PathBuf> = kept.into_iter().collect();
    found.retain(|f| kept.contains(&f.path));

    pair_live_photos(&mut found);
    if live_photo_videos == LivePhotoVideos::Skip {
        let before = found.len();
        found.retain(|f| f.live_photo_of.is_none());
        result.live_photo_videos_skipped = before - found.len();
    } else {
        result.live_photo_videos = found.iter().filter(|f| f.live_photo_of.is_some()).count();
    }

    result.files = found;
    Ok(result)
}

fn stat(path: &Path) -> std::io::Result<ScannedFile> {
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    Ok(ScannedFile {
        path: path.to_path_buf(),
        size: meta.len(),
        mtime_ns,
        live_photo_of: None,
    })
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_still(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "jpg" | "jpeg" | "heic" | "heif" | "png" | "dng"
    ) || crate::scanner::is_raw_file(path)
}

fn is_live_video(path: &Path) -> bool {
    matches!(extension(path).as_str(), "mov" | "mp4")
}

/// Mark each video that shares its directory and name (ignoring case and
/// extension) with a photo as that photo's Live Photo video.
fn pair_live_photos(files: &mut [ScannedFile]) {
    let key = |p: &Path| {
        (
            p.parent().map(Path::to_path_buf),
            p.file_stem().map(|s| s.to_string_lossy().to_lowercase()),
        )
    };
    let stills: HashMap<_, PathBuf> = files
        .iter()
        .filter(|f| is_still(&f.path))
        .map(|f| (key(&f.path), f.path.clone()))
        .collect();
    for f in files.iter_mut() {
        if is_live_video(&f.path) {
            f.live_photo_of = stills.get(&key(&f.path)).cloned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn rel(result: &ScanResult, root: &Path) -> Vec<String> {
        result
            .files
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    fn scan_one(root: &Path, excludes: &[&str]) -> ScanResult {
        let excludes: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();
        scan(
            &[root.to_path_buf()],
            &excludes,
            RawHandling::Render,
            LivePhotoVideos::Upload,
        )
        .unwrap()
    }

    #[test]
    fn excluded_directories_hidden_files_and_nas_metadata_are_left_out() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "keep/a.jpg");
        touch(root, "old_backup/b.jpg");
        touch(root, "keep/deep/old_backup/c.jpg");
        touch(root, "keep/._a.jpg");
        touch(root, ".hidden/d.jpg");
        touch(root, "keep/@eaDir/a.jpg/SYNOPHOTO_THUMB_XL.jpg");
        touch(root, "keep/Thumbs.db");
        touch(root, "keep/a.xmp");

        let result = scan_one(root, &["/old_backup/"]);
        assert_eq!(
            rel(&result, root),
            vec!["keep/a.jpg", "keep/deep/old_backup/c.jpg"]
        );
        assert_eq!(result.unsupported, 2);
    }

    #[test]
    fn unanchored_patterns_match_at_any_depth() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "a/old_backup/b.jpg");
        touch(root, "a/c.jpg");
        touch(root, "a/screenshot.png");

        let result = scan_one(root, &["old_backup/", "*.png"]);
        assert_eq!(rel(&result, root), vec!["a/c.jpg"]);
    }

    #[test]
    fn smugmugignore_files_add_patterns() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "a/keep.jpg");
        touch(root, "a/skip/x.jpg");
        fs::write(root.join("a").join(IGNORE_FILE), "skip/\n").unwrap();

        let result = scan_one(root, &[]);
        assert_eq!(rel(&result, root), vec!["a/keep.jpg"]);
    }

    #[test]
    fn records_size_and_mtime() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.jpg"), b"12345").unwrap();
        let result = scan_one(dir.path(), &[]);
        assert_eq!(result.files[0].size, 5);
        assert!(result.files[0].mtime_ns > 0);
    }

    #[test]
    fn live_photo_videos_are_paired_with_their_photo() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "IMG_1.HEIC");
        touch(root, "IMG_1.MOV");
        touch(root, "IMG_2.jpg");
        touch(root, "img_2.mp4");
        touch(root, "VID_3.mov");
        touch(root, "other/IMG_1.MOV");

        let result = scan_one(root, &[]);
        let paired: Vec<(String, String)> = result
            .files
            .iter()
            .filter_map(|f| {
                let photo = f.live_photo_of.as_ref()?;
                Some((
                    f.path.file_name()?.to_string_lossy().into_owned(),
                    photo.file_name()?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            paired,
            vec![
                ("IMG_1.MOV".to_string(), "IMG_1.HEIC".to_string()),
                ("img_2.mp4".to_string(), "IMG_2.jpg".to_string()),
            ]
        );
        assert_eq!(result.live_photo_videos, 2);

        let skipped = scan(
            &[root.to_path_buf()],
            &[],
            RawHandling::Render,
            LivePhotoVideos::Skip,
        )
        .unwrap();
        assert_eq!(skipped.live_photo_videos_skipped, 2);
        assert_eq!(skipped.files.len(), 4);
    }

    #[test]
    fn raw_files_with_a_jpeg_sibling_are_dropped() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "IMG_1.CR2");
        touch(root, "IMG_1.JPG");
        touch(root, "IMG_2.CR2");

        let result = scan_one(root, &[]);
        assert_eq!(rel(&result, root), vec!["IMG_1.JPG", "IMG_2.CR2"]);
        assert_eq!(result.raw_selection.skipped_with_sibling, 1);
    }

    #[test]
    fn overlapping_sources_list_files_once() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(root, "a/b.jpg");
        let result = scan(
            &[root.to_path_buf(), root.join("a")],
            &[],
            RawHandling::Render,
            LivePhotoVideos::Upload,
        )
        .unwrap();
        assert_eq!(result.files.len(), 1);
    }

    #[test]
    fn a_missing_source_is_an_error() {
        let dir = TempDir::new().unwrap();
        assert!(
            scan(
                &[dir.path().join("nope")],
                &[],
                RawHandling::Render,
                LivePhotoVideos::Upload
            )
            .is_err()
        );
    }
}
