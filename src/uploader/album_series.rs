//! Spreading an upload across a series of albums so no album exceeds
//! SmugMug's per-gallery limit. The series is `Name`, `Name (2)`,
//! `Name (3)`, ...; files fill the first album with room, then continue in
//! the next, creating albums as needed.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::SmugMugClient;
use crate::api::albums::Album;

/// SmugMug's documented maximum number of photos and videos per gallery.
pub const MAX_ALBUM_IMAGES: u64 = 5000;

/// Safety stop for the series walk; far beyond any real upload.
const MAX_SERIES_LEN: usize = 1000;

/// One album and the files planned to go into it.
pub struct AlbumBatch {
    pub album: Album,
    pub files: Vec<PathBuf>,
    /// The album doesn't exist yet: it was just created, or on a dry run it
    /// would be (and `album.album_key` is empty).
    pub new_album: bool,
}

/// `n`-th album name in a series, counting from 1.
pub fn series_album_name(base: &str, n: usize) -> String {
    if n <= 1 {
        base.to_string()
    } else {
        format!("{} ({})", base, n)
    }
}

/// Where the albums of a series live and how to look them up or create them.
// Only implemented and used inside this crate, so the Send-bound caveat of
// async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait AlbumSeriesBackend {
    async fn find_album(&self, name: &str) -> Result<Option<Album>>;
    async fn image_count(&self, album: &Album) -> Result<u64>;
    async fn create_album(&self, name: &str) -> Result<Album>;
}

/// Assign `files` to albums of the series `base`, filling each album up to
/// `capacity` images. Existing albums that are already full are skipped.
/// Missing albums are created, except on a dry run, where a placeholder
/// (empty album key) stands in for them.
pub async fn plan_album_batches<B: AlbumSeriesBackend>(
    backend: &B,
    base: &str,
    mut files: Vec<PathBuf>,
    capacity: u64,
    dry_run: bool,
) -> Result<Vec<AlbumBatch>> {
    let mut batches = Vec::new();
    let mut n = 1;

    while !files.is_empty() {
        if n > MAX_SERIES_LEN {
            anyhow::bail!("Gave up after {} albums named '{}'", MAX_SERIES_LEN, base);
        }
        let name = series_album_name(base, n);
        n += 1;

        let existing = backend.find_album(&name).await?;
        let used = match &existing {
            Some(album) => backend.image_count(album).await?,
            None => 0,
        };
        let room = capacity.saturating_sub(used);
        if room == 0 {
            continue;
        }
        let take = files.len().min(room as usize);
        let batch_files: Vec<PathBuf> = files.drain(..take).collect();

        let (album, new_album) = match existing {
            Some(album) => (album, false),
            None if dry_run => (placeholder_album(&name), true),
            None => (backend.create_album(&name).await?, true),
        };
        batches.push(AlbumBatch {
            album,
            files: batch_files,
            new_album,
        });
    }

    Ok(batches)
}

fn placeholder_album(name: &str) -> Album {
    Album {
        album_key: String::new(),
        name: name.to_string(),
        url_name: String::new(),
        node_id: String::new(),
        uri: String::new(),
        web_uri: None,
        uris: None,
        image_count: Some(0),
    }
}

/// Where a series' albums are looked up and created.
pub enum AlbumScope {
    /// Directly inside this folder node (by node URI).
    Folder(String),
    /// Matched by name anywhere in the account, created at the root. This is
    /// how `--album` without `--parent` has always found its album.
    Anywhere,
    /// The folder doesn't exist yet (dry run): no albums exist in it and
    /// none can be created.
    MissingFolder,
}

/// Album series backed by the SmugMug API. New albums are private.
pub struct ClientAlbumSeries {
    client: Arc<SmugMugClient>,
    scope: AlbumScope,
    all_albums: tokio::sync::OnceCell<Vec<Album>>,
}

impl ClientAlbumSeries {
    pub fn new(client: Arc<SmugMugClient>, scope: AlbumScope) -> Self {
        ClientAlbumSeries {
            client,
            scope,
            all_albums: tokio::sync::OnceCell::new(),
        }
    }
}

impl AlbumSeriesBackend for ClientAlbumSeries {
    async fn find_album(&self, name: &str) -> Result<Option<Album>> {
        match &self.scope {
            AlbumScope::Folder(node_uri) => self.client.find_album_in_folder(node_uri, name).await,
            AlbumScope::Anywhere => {
                let albums = self
                    .all_albums
                    .get_or_try_init(|| self.client.list_albums())
                    .await?;
                Ok(albums.iter().find(|a| a.name == name).cloned())
            }
            AlbumScope::MissingFolder => Ok(None),
        }
    }

    async fn image_count(&self, album: &Album) -> Result<u64> {
        // Listings don't always carry ImageCount; the album itself does.
        let fetched = self.client.get_album(&album.album_key).await?;
        Ok(fetched.image_count.unwrap_or(0))
    }

    async fn create_album(&self, name: &str) -> Result<Album> {
        let parent = match &self.scope {
            AlbumScope::Folder(node_uri) => Some(node_uri.as_str()),
            AlbumScope::Anywhere => None,
            AlbumScope::MissingFolder => {
                anyhow::bail!("Can't create album '{}': its folder doesn't exist", name)
            }
        };
        self.client.create_album(name, parent, "Private").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory albums: name -> image count.
    struct FakeBackend {
        albums: Mutex<HashMap<String, u64>>,
        created: Mutex<Vec<String>>,
    }

    impl FakeBackend {
        fn new(existing: &[(&str, u64)]) -> Self {
            FakeBackend {
                albums: Mutex::new(
                    existing
                        .iter()
                        .map(|(name, count)| (name.to_string(), *count))
                        .collect(),
                ),
                created: Mutex::new(Vec::new()),
            }
        }

        fn album(name: &str) -> Album {
            Album {
                album_key: format!("key-{}", name),
                ..placeholder_album(name)
            }
        }
    }

    impl AlbumSeriesBackend for FakeBackend {
        async fn find_album(&self, name: &str) -> Result<Option<Album>> {
            Ok(self
                .albums
                .lock()
                .unwrap()
                .contains_key(name)
                .then(|| FakeBackend::album(name)))
        }

        async fn image_count(&self, album: &Album) -> Result<u64> {
            Ok(self.albums.lock().unwrap()[&album.name])
        }

        async fn create_album(&self, name: &str) -> Result<Album> {
            self.albums.lock().unwrap().insert(name.to_string(), 0);
            self.created.lock().unwrap().push(name.to_string());
            Ok(FakeBackend::album(name))
        }
    }

    fn files(count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|i| PathBuf::from(format!("{}.jpg", i)))
            .collect()
    }

    fn summary(batches: &[AlbumBatch]) -> Vec<(String, usize, bool)> {
        batches
            .iter()
            .map(|b| (b.album.name.clone(), b.files.len(), b.new_album))
            .collect()
    }

    #[test]
    fn test_series_album_name() {
        assert_eq!(series_album_name("2026-09", 1), "2026-09");
        assert_eq!(series_album_name("2026-09", 2), "2026-09 (2)");
        assert_eq!(series_album_name("Trip", 10), "Trip (10)");
    }

    #[tokio::test]
    async fn test_new_album_when_series_is_empty() {
        let backend = FakeBackend::new(&[]);
        let batches = plan_album_batches(&backend, "2026-09", files(3), 5, false)
            .await
            .unwrap();
        assert_eq!(summary(&batches), vec![("2026-09".to_string(), 3, true)]);
        assert_eq!(*backend.created.lock().unwrap(), vec!["2026-09"]);
    }

    #[tokio::test]
    async fn test_fills_existing_album_then_rolls_over() {
        let backend = FakeBackend::new(&[("2026-09", 3)]);
        let batches = plan_album_batches(&backend, "2026-09", files(9), 5, false)
            .await
            .unwrap();
        assert_eq!(
            summary(&batches),
            vec![
                ("2026-09".to_string(), 2, false),
                ("2026-09 (2)".to_string(), 5, true),
                ("2026-09 (3)".to_string(), 2, true),
            ]
        );
        // Files keep their order across the batches.
        assert_eq!(batches[1].files[0], PathBuf::from("2.jpg"));
    }

    #[tokio::test]
    async fn test_skips_full_albums_and_reuses_partial_ones() {
        let backend = FakeBackend::new(&[("Trip", 5), ("Trip (2)", 5), ("Trip (3)", 4)]);
        let batches = plan_album_batches(&backend, "Trip", files(3), 5, false)
            .await
            .unwrap();
        assert_eq!(
            summary(&batches),
            vec![
                ("Trip (3)".to_string(), 1, false),
                ("Trip (4)".to_string(), 2, true),
            ]
        );
    }

    #[tokio::test]
    async fn test_dry_run_creates_nothing() {
        let backend = FakeBackend::new(&[("2026-09", 4)]);
        let batches = plan_album_batches(&backend, "2026-09", files(3), 5, true)
            .await
            .unwrap();
        assert_eq!(
            summary(&batches),
            vec![
                ("2026-09".to_string(), 1, false),
                ("2026-09 (2)".to_string(), 2, true),
            ]
        );
        assert!(batches[1].album.album_key.is_empty());
        assert!(backend.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_no_files_no_batches() {
        let backend = FakeBackend::new(&[]);
        let batches = plan_album_batches(&backend, "2026-09", Vec::new(), 5, false)
            .await
            .unwrap();
        assert!(batches.is_empty());
        assert!(backend.created.lock().unwrap().is_empty());
    }
}
