//! What each local path was last seen as and uploaded to, so an unchanged
//! file (same size and modification time) is skipped without being read.
//!
//! Lives in the cache database next to the content-hash store: the `files`
//! tree maps a path to its `FileRecord`, and the `refs` tree counts the
//! paths pointing at each SmugMug image, so an image shared by identical
//! copies of a file is never overwritten when one copy is edited.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::cache::hash_store::HashStore;
use crate::dated::date::DateSource;
use crate::dated::scan::ScannedFile;

/// One local file as of its last successful upload (or permanent failure).
/// Field names are short: there's one record per file in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "m")]
    pub mtime_ns: i64,
    /// SHA-256 of the file (for a RAW: of the RAW file, not the JPEG).
    #[serde(rename = "h")]
    pub sha256: String,
    /// MD5 of what was uploaded: the file, or the JPEG rendered from a RAW.
    #[serde(rename = "u")]
    pub uploaded_md5: Option<String>,
    #[serde(rename = "d")]
    pub capture_date: NaiveDate,
    #[serde(rename = "ds")]
    pub date_source: DateSource,
    /// The SmugMug (album) image this file is, once uploaded or linked.
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub image_uri: Option<String>,
    #[serde(rename = "a", default, skip_serializing_if = "Option::is_none")]
    pub album_key: Option<String>,
    /// Why SmugMug refused this file, if it did (e.g. too big). Not retried
    /// until the file changes.
    #[serde(rename = "f", default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

impl FileRecord {
    /// The file on disk is the one this record describes (by size and
    /// modification time; contents aren't read).
    pub fn matches(&self, file: &ScannedFile) -> bool {
        self.size == file.size && self.mtime_ns == file.mtime_ns
    }

    /// Nothing left to do for this file while it's unchanged.
    pub fn is_settled(&self) -> bool {
        self.image_uri.is_some() || self.failed.is_some()
    }
}

#[derive(Clone)]
pub struct FileIndex {
    files: sled::Tree,
    refs: sled::Tree,
}

fn key(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

impl FileIndex {
    pub fn open(store: &HashStore) -> Result<Self> {
        Ok(FileIndex {
            files: store.open_tree("files")?,
            refs: store.open_tree("refs")?,
        })
    }

    pub fn get(&self, path: &Path) -> Result<Option<FileRecord>> {
        match self
            .files
            .get(key(path))
            .context("Failed to read file index")?
        {
            Some(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            None => Ok(None),
        }
    }

    /// Record `path` as `record`, moving its image reference from whatever
    /// it pointed at before.
    pub fn put(&self, path: &Path, record: &FileRecord) -> Result<()> {
        let previous = self.get(path)?;
        let old_uri = previous.and_then(|r| r.image_uri);
        if old_uri != record.image_uri {
            if let Some(uri) = &old_uri {
                self.add_ref(uri, -1)?;
            }
            if let Some(uri) = &record.image_uri {
                self.add_ref(uri, 1)?;
            }
        }
        let bytes = serde_json::to_vec(record).context("Failed to serialize file record")?;
        self.files
            .insert(key(path), bytes)
            .context("Failed to write file index")?;
        Ok(())
    }

    /// How many indexed paths point at `image_uri`.
    pub fn refs(&self, image_uri: &str) -> Result<u32> {
        Ok(self
            .refs
            .get(image_uri.as_bytes())
            .context("Failed to read image references")?
            .map(|v| u32::from_be_bytes(v.as_ref().try_into().unwrap_or([0; 4])))
            .unwrap_or(0))
    }

    fn add_ref(&self, image_uri: &str, delta: i64) -> Result<()> {
        self.refs
            .update_and_fetch(image_uri.as_bytes(), |old| {
                let count = old
                    .map(|v| u32::from_be_bytes(v.try_into().unwrap_or([0; 4])))
                    .unwrap_or(0) as i64
                    + delta;
                (count > 0).then(|| (count as u32).to_be_bytes().to_vec())
            })
            .context("Failed to update image references")?;
        Ok(())
    }

    /// Number of indexed files.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn record(uri: Option<&str>) -> FileRecord {
        FileRecord {
            size: 10,
            mtime_ns: 20,
            sha256: "abc".into(),
            uploaded_md5: Some("def".into()),
            capture_date: NaiveDate::from_ymd_opt(2014, 7, 12).unwrap(),
            date_source: DateSource::Exif,
            image_uri: uri.map(String::from),
            album_key: None,
            failed: None,
        }
    }

    fn index() -> (TempDir, FileIndex) {
        let dir = TempDir::new().unwrap();
        let store = HashStore::new(dir.path().to_str().unwrap()).unwrap();
        let index = FileIndex::open(&store).unwrap();
        (dir, index)
    }

    #[test]
    fn round_trips_records() {
        let (_dir, index) = index();
        let path = PathBuf::from("/photos/a.jpg");
        assert_eq!(index.get(&path).unwrap(), None);
        index.put(&path, &record(Some("/img/1"))).unwrap();
        assert_eq!(index.get(&path).unwrap(), Some(record(Some("/img/1"))));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn counts_paths_per_image() {
        let (_dir, index) = index();
        let a = PathBuf::from("/a.jpg");
        let b = PathBuf::from("/b.jpg");
        index.put(&a, &record(Some("/img/1"))).unwrap();
        index.put(&b, &record(Some("/img/1"))).unwrap();
        assert_eq!(index.refs("/img/1").unwrap(), 2);

        // Re-recording the same image doesn't double count.
        index.put(&a, &record(Some("/img/1"))).unwrap();
        assert_eq!(index.refs("/img/1").unwrap(), 2);

        // Moving a path to another image moves its reference.
        index.put(&a, &record(Some("/img/2"))).unwrap();
        assert_eq!(index.refs("/img/1").unwrap(), 1);
        assert_eq!(index.refs("/img/2").unwrap(), 1);

        index.put(&b, &record(None)).unwrap();
        assert_eq!(index.refs("/img/1").unwrap(), 0);
    }

    #[test]
    fn matches_on_size_and_mtime() {
        let rec = record(Some("/img/1"));
        let mut file = ScannedFile {
            path: PathBuf::from("/a.jpg"),
            size: 10,
            mtime_ns: 20,
            live_photo_of: None,
        };
        assert!(rec.matches(&file));
        file.mtime_ns = 21;
        assert!(!rec.matches(&file));
    }

    #[test]
    fn clearing_the_store_clears_the_index() {
        let dir = TempDir::new().unwrap();
        let store = HashStore::new(dir.path().to_str().unwrap()).unwrap();
        let index = FileIndex::open(&store).unwrap();
        index
            .put(Path::new("/a.jpg"), &record(Some("/img/1")))
            .unwrap();
        store.clear().unwrap();
        let index = FileIndex::open(&store).unwrap();
        assert_eq!(index.get(Path::new("/a.jpg")).unwrap(), None);
        assert_eq!(index.refs("/img/1").unwrap(), 0);
    }
}
