//! Spreading an upload across a series of albums so no album exceeds
//! SmugMug's per-gallery limit. The series is `Name`, `Name (2)`,
//! `Name (3)`, ...; each upload claims a slot in the first album with room,
//! and a new album is created only when a file actually needs one. Files that
//! are skipped (duplicates, RAW without Source) never use up space or cause
//! an album to be created.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::api::SmugMugClient;
use crate::api::albums::Album;

/// SmugMug's documented maximum number of photos and videos per gallery.
pub const MAX_ALBUM_IMAGES: u64 = 5000;

/// Safety stop for the series walk; far beyond any real upload.
const MAX_SERIES_LEN: usize = 1000;

/// `n`-th album name in a series, counting from 1.
pub fn series_album_name(base: &str, n: usize) -> String {
    if n <= 1 {
        base.to_string()
    } else {
        format!("{} ({})", base, n)
    }
}

/// Where the albums of a series live and how to look them up or create them.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait AlbumSeriesBackend {
    async fn find_album(&self, name: &str) -> Result<Option<Album>>;
    async fn image_count(&self, album: &Album) -> Result<u64>;
    async fn create_album(&self, name: &str) -> Result<Album>;
}

/// One album of a series and how it's being used by this run.
#[derive(Debug, Clone)]
pub struct SeriesAlbum {
    pub album: Album,
    /// Images in the album, counting slots claimed by this run.
    pub count: u64,
    /// Slots claimed by this run (uploads in progress or done).
    pub claimed: u64,
    /// Created by this run (on a dry run: would be created, and
    /// `album.album_key` is empty).
    pub created: bool,
}

struct SeriesState {
    albums: Vec<SeriesAlbum>,
    /// Index of the first album that may still have room.
    current: usize,
}

/// A series of albums shared by concurrent upload workers.
pub struct AlbumSeries<B> {
    backend: B,
    base: String,
    capacity: u64,
    dry_run: bool,
    state: Mutex<SeriesState>,
}

impl<B: AlbumSeriesBackend> AlbumSeries<B> {
    /// Look up the albums of the series that already exist (`base`,
    /// `base (2)`, ... until the first missing one) and their image counts.
    /// Creates nothing.
    pub async fn load(backend: B, base: &str, capacity: u64, dry_run: bool) -> Result<Self> {
        let mut albums = Vec::new();
        for n in 1..=MAX_SERIES_LEN {
            let Some(album) = backend.find_album(&series_album_name(base, n)).await? else {
                break;
            };
            let count = backend.image_count(&album).await?;
            albums.push(SeriesAlbum {
                album,
                count,
                claimed: 0,
                created: false,
            });
        }
        Ok(AlbumSeries {
            backend,
            base: base.to_string(),
            capacity,
            dry_run,
            state: Mutex::new(SeriesState { albums, current: 0 }),
        })
    }

    /// Claim room for one new image and return the album it should go in:
    /// the first album with room, creating the next album of the series if
    /// all are full (on a dry run, a placeholder with an empty key).
    pub async fn claim(&self) -> Result<Album> {
        let mut state = self.state.lock().await;
        let mut i = state.current;
        loop {
            if i >= MAX_SERIES_LEN {
                anyhow::bail!(
                    "Gave up after {} albums named '{}'",
                    MAX_SERIES_LEN,
                    self.base
                );
            }
            if i == state.albums.len() {
                let name = series_album_name(&self.base, i + 1);
                let album = if self.dry_run {
                    placeholder_album(&name)
                } else {
                    self.backend.create_album(&name).await?
                };
                state.albums.push(SeriesAlbum {
                    album,
                    count: 0,
                    claimed: 0,
                    created: true,
                });
            }
            let entry = &mut state.albums[i];
            if entry.count < self.capacity {
                entry.count += 1;
                entry.claimed += 1;
                let album = entry.album.clone();
                state.current = i;
                return Ok(album);
            }
            i += 1;
        }
    }

    /// Give back a slot claimed for `album` whose upload failed.
    pub async fn release(&self, album: &Album) {
        let mut state = self.state.lock().await;
        if let Some(i) = state.albums.iter().position(|a| a.album.name == album.name) {
            let entry = &mut state.albums[i];
            entry.count = entry.count.saturating_sub(1);
            entry.claimed = entry.claimed.saturating_sub(1);
            state.current = state.current.min(i);
        }
    }

    /// Albums of the series that existed before this run.
    pub async fn existing_albums(&self) -> Vec<SeriesAlbum> {
        let state = self.state.lock().await;
        state
            .albums
            .iter()
            .filter(|a| !a.created)
            .cloned()
            .collect()
    }

    /// Every album of the series as it stands now.
    pub async fn albums(&self) -> Vec<SeriesAlbum> {
        self.state.lock().await.albums.clone()
    }
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
    use std::sync::Mutex as StdMutex;

    /// In-memory albums: name -> image count.
    struct FakeBackend {
        albums: StdMutex<HashMap<String, u64>>,
        created: Arc<StdMutex<Vec<String>>>,
    }

    impl FakeBackend {
        fn new(existing: &[(&str, u64)]) -> (Self, Arc<StdMutex<Vec<String>>>) {
            let created = Arc::new(StdMutex::new(Vec::new()));
            let backend = FakeBackend {
                albums: StdMutex::new(
                    existing
                        .iter()
                        .map(|(name, count)| (name.to_string(), *count))
                        .collect(),
                ),
                created: created.clone(),
            };
            (backend, created)
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

    async fn claim_names<B: AlbumSeriesBackend>(series: &AlbumSeries<B>, n: usize) -> Vec<String> {
        let mut names = Vec::new();
        for _ in 0..n {
            names.push(series.claim().await.unwrap().name);
        }
        names
    }

    #[test]
    fn test_series_album_name() {
        assert_eq!(series_album_name("2026-09", 1), "2026-09");
        assert_eq!(series_album_name("2026-09", 2), "2026-09 (2)");
        assert_eq!(series_album_name("Trip", 10), "Trip (10)");
    }

    #[tokio::test]
    async fn test_load_creates_nothing() {
        let (backend, created) = FakeBackend::new(&[]);
        let series = AlbumSeries::load(backend, "2026-09", 5, false)
            .await
            .unwrap();
        assert!(series.albums().await.is_empty());
        assert!(created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_first_claim_creates_album() {
        let (backend, created) = FakeBackend::new(&[]);
        let series = AlbumSeries::load(backend, "2026-09", 5, false)
            .await
            .unwrap();
        assert_eq!(claim_names(&series, 3).await, vec!["2026-09"; 3]);
        assert_eq!(*created.lock().unwrap(), vec!["2026-09"]);
    }

    #[tokio::test]
    async fn test_fills_existing_album_then_rolls_over() {
        let (backend, created) = FakeBackend::new(&[("2026-09", 3)]);
        let series = AlbumSeries::load(backend, "2026-09", 5, false)
            .await
            .unwrap();
        assert_eq!(
            claim_names(&series, 4).await,
            vec!["2026-09", "2026-09", "2026-09 (2)", "2026-09 (2)"]
        );
        assert_eq!(*created.lock().unwrap(), vec!["2026-09 (2)"]);

        let albums = series.albums().await;
        assert_eq!(
            (albums[0].count, albums[0].claimed, albums[0].created),
            (5, 2, false)
        );
        assert_eq!(
            (albums[1].count, albums[1].claimed, albums[1].created),
            (2, 2, true)
        );
    }

    #[tokio::test]
    async fn test_skips_full_albums_and_reuses_partial_ones() {
        let (backend, created) = FakeBackend::new(&[("Trip", 5), ("Trip (2)", 5), ("Trip (3)", 4)]);
        let series = AlbumSeries::load(backend, "Trip", 5, false).await.unwrap();
        assert_eq!(series.existing_albums().await.len(), 3);
        assert_eq!(
            claim_names(&series, 3).await,
            vec!["Trip (3)", "Trip (4)", "Trip (4)"]
        );
        assert_eq!(*created.lock().unwrap(), vec!["Trip (4)"]);
    }

    #[tokio::test]
    async fn test_release_frees_room_for_the_next_claim() {
        let (backend, created) = FakeBackend::new(&[("2026-09", 4)]);
        let series = AlbumSeries::load(backend, "2026-09", 5, false)
            .await
            .unwrap();
        let album = series.claim().await.unwrap();
        assert_eq!(album.name, "2026-09");
        // The upload failed: its slot goes back, so no new album is needed.
        series.release(&album).await;
        assert_eq!(claim_names(&series, 1).await, vec!["2026-09"]);
        assert!(created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dry_run_creates_nothing() {
        let (backend, created) = FakeBackend::new(&[("2026-09", 4)]);
        let series = AlbumSeries::load(backend, "2026-09", 5, true)
            .await
            .unwrap();
        assert_eq!(
            claim_names(&series, 3).await,
            vec!["2026-09", "2026-09 (2)", "2026-09 (2)"]
        );
        let albums = series.albums().await;
        assert!(albums[1].created);
        assert!(albums[1].album.album_key.is_empty());
        assert!(created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_concurrent_claims_never_overfill() {
        let (backend, _created) = FakeBackend::new(&[("2026-09", 2)]);
        let series = Arc::new(
            AlbumSeries::load(backend, "2026-09", 5, false)
                .await
                .unwrap(),
        );
        let mut handles = Vec::new();
        for _ in 0..8 {
            let series = series.clone();
            handles.push(tokio::spawn(async move { series.claim().await.unwrap() }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let albums = series.albums().await;
        assert_eq!(albums[0].count, 5);
        assert_eq!(albums[1].count, 5);
        assert_eq!(albums.len(), 2);
    }
}
