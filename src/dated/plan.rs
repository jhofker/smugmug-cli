//! Where a dated file goes: `<root>/YYYY/MM/YYYY-MM-DD`, each day an album
//! series (`2014-07-12`, `2014-07-12 (2)`, ...) so no album passes
//! SmugMug's limit. Year and month folders and day albums are created only
//! when a file needs uploading into them, and each folder is listed at most
//! once per run.
//!
//! Each day also keeps the file names its albums hold, so a file is never
//! uploaded over a different photo of the same name (two cameras both
//! producing `IMG_0001.JPG` on one day): the newcomer is renamed
//! `IMG_0001~<md5 prefix>.JPG` instead. A file whose name and content are
//! already there is linked to that image rather than uploaded again.

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

use crate::api::SmugMugClient;
use crate::api::albums::{Album, ChildNode};
use crate::api::images::AlbumImage;
use crate::uploader::album_series::{AlbumSeries, AlbumSeriesBackend, MAX_ALBUM_IMAGES};

/// The SmugMug calls placing files by date needs.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait Smug {
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>>;
    /// Create a private folder; returns its node URI.
    async fn create_folder(&self, parent_node_uri: &str, name: &str) -> Result<String>;
    /// Create a private album.
    async fn create_album(&self, parent_node_uri: &str, name: &str) -> Result<Album>;
    async fn image_count(&self, album: &Album) -> Result<u64>;
    async fn album_images(&self, album_key: &str) -> Result<Vec<AlbumImage>>;
}

impl Smug for SmugMugClient {
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
        self.list_children(node_uri).await
    }
    async fn create_folder(&self, parent_node_uri: &str, name: &str) -> Result<String> {
        SmugMugClient::create_folder(self, parent_node_uri, name, Some("Private")).await
    }
    async fn create_album(&self, parent_node_uri: &str, name: &str) -> Result<Album> {
        SmugMugClient::create_album(self, name, Some(parent_node_uri), "Private").await
    }
    async fn image_count(&self, album: &Album) -> Result<u64> {
        Ok(self
            .get_album(&album.album_key)
            .await?
            .image_count
            .unwrap_or(0))
    }
    async fn album_images(&self, album_key: &str) -> Result<Vec<AlbumImage>> {
        self.list_album_images(album_key).await
    }
}

/// Name of the album for `date` (the first of its series).
pub fn day_album_name(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn month_path(date: NaiveDate) -> [String; 2] {
    [
        format!("{:04}", date.year()),
        format!("{:02}", date.month()),
    ]
}

/// Folders found or created so far, and folder listings.
#[derive(Default)]
struct Folders {
    /// "2014" or "2014/07" → node URI, or `None` when known not to exist.
    known: HashMap<String, Option<String>>,
    listings: HashMap<String, Arc<Vec<ChildNode>>>,
}

struct Inner<T> {
    smug: Arc<T>,
    /// Node URI of the root folder; `None` if it doesn't exist (dry run).
    root: Option<String>,
    dry_run: bool,
    folders: Mutex<Folders>,
}

impl<T: Smug> Inner<T> {
    async fn listing(&self, folders: &mut Folders, node_uri: &str) -> Result<Arc<Vec<ChildNode>>> {
        if let Some(listing) = folders.listings.get(node_uri) {
            return Ok(listing.clone());
        }
        let listing = Arc::new(
            self.smug
                .children(node_uri)
                .await
                .with_context(|| format!("Failed to list folder {}", node_uri))?,
        );
        folders
            .listings
            .insert(node_uri.to_string(), listing.clone());
        Ok(listing)
    }

    /// Node URI of the folder at `path` under the root, creating missing
    /// folders if `create` (and this isn't a dry run).
    async fn folder(&self, path: &[String], create: bool) -> Result<Option<String>> {
        let Some(root) = &self.root else {
            return Ok(None);
        };
        let mut folders = self.folders.lock().await;
        let mut parent = root.clone();
        let mut key = String::new();
        for part in path {
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(part);

            let known = folders.known.get(&key).cloned();
            let found = match known {
                Some(Some(uri)) => Some(uri),
                Some(None) if !create => return Ok(None),
                _ => self
                    .listing(&mut folders, &parent)
                    .await?
                    .iter()
                    .find(|c| c.node_type == "Folder" && c.name == *part)
                    .map(|c| c.uri.clone()),
            };
            let uri = match found {
                Some(uri) => uri,
                None if create && !self.dry_run => {
                    let uri = self
                        .smug
                        .create_folder(&parent, part)
                        .await
                        .with_context(|| format!("Failed to create folder {}", key))?;
                    folders.listings.insert(uri.clone(), Arc::new(Vec::new()));
                    uri
                }
                None => {
                    folders.known.insert(key, None);
                    return Ok(None);
                }
            };
            folders.known.insert(key.clone(), Some(uri.clone()));
            parent = uri;
        }
        Ok(Some(parent))
    }

    async fn albums_in(&self, folder_uri: &str) -> Result<Vec<Album>> {
        let mut folders = self.folders.lock().await;
        Ok(self
            .listing(&mut folders, folder_uri)
            .await?
            .iter()
            .filter_map(ChildNode::album)
            .collect())
    }
}

/// The albums of one day's series, as `AlbumSeries` sees them.
pub struct DayBackend<T> {
    inner: Arc<Inner<T>>,
    date: NaiveDate,
}

impl<T: Smug> AlbumSeriesBackend for DayBackend<T> {
    async fn find_album(&self, name: &str) -> Result<Option<Album>> {
        let Some(month) = self.inner.folder(&month_path(self.date), false).await? else {
            return Ok(None);
        };
        Ok(self
            .inner
            .albums_in(&month)
            .await?
            .into_iter()
            .find(|a| a.name == name))
    }

    async fn image_count(&self, album: &Album) -> Result<u64> {
        self.inner.smug.image_count(album).await
    }

    async fn create_album(&self, name: &str) -> Result<Album> {
        let month = self
            .inner
            .folder(&month_path(self.date), true)
            .await?
            .context("The day's folder couldn't be created")?;
        self.inner.smug.create_album(&month, name).await
    }
}

/// An image already in a day's albums, by file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedImage {
    /// Lowercase hex MD5 of its content, if known.
    pub md5: Option<String>,
    /// Album image URI; empty while an upload of this name is in progress.
    pub uri: String,
    pub album_key: String,
}

/// What to call a file in its day's albums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameCheck {
    /// Upload it under this name (its own, or renamed to avoid a different
    /// photo with its name), now reserved for it.
    Upload(String),
    /// The same content is already there under this name.
    Present(NamedImage),
}

/// One day's album series and the file names in it.
pub struct Day<T> {
    pub series: AlbumSeries<DayBackend<T>>,
    smug: Arc<T>,
    names: OnceCell<std::sync::Mutex<HashMap<String, NamedImage>>>,
}

/// `IMG_0001.JPG` → `IMG_0001~1a2b3c4d.JPG`.
pub fn renamed(file_name: &str, md5: &str) -> String {
    let tag = &md5[..md5.len().min(8)];
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{}~{}.{}", stem, tag, ext),
        _ => format!("{}~{}", file_name, tag),
    }
}

impl<T: Smug> Day<T> {
    async fn names(&self) -> Result<&std::sync::Mutex<HashMap<String, NamedImage>>> {
        self.names
            .get_or_try_init(|| async {
                let mut names = HashMap::new();
                for existing in self.series.existing_albums().await {
                    let key = existing.album.album_key;
                    for image in
                        self.smug.album_images(&key).await.with_context(|| {
                            format!("Failed to list album {}", existing.album.name)
                        })?
                    {
                        names.insert(
                            image.file_name,
                            NamedImage {
                                md5: image.archived_md5.map(|m| m.to_lowercase()),
                                uri: image.uri,
                                album_key: key.clone(),
                            },
                        );
                    }
                }
                Ok::<_, anyhow::Error>(std::sync::Mutex::new(names))
            })
            .await
    }

    /// Decide the name for a file called `file_name` with content `md5`,
    /// reserving it (see `NameCheck`).
    pub async fn check_name(&self, file_name: &str, md5: &str) -> Result<NameCheck> {
        let names = self.names().await?;
        let mut names = names.lock().unwrap();
        let alternative = renamed(file_name, md5);
        for candidate in [file_name, alternative.as_str()] {
            match names.get(candidate) {
                None => {
                    names.insert(
                        candidate.to_string(),
                        NamedImage {
                            md5: Some(md5.to_string()),
                            uri: String::new(),
                            album_key: String::new(),
                        },
                    );
                    return Ok(NameCheck::Upload(candidate.to_string()));
                }
                Some(existing)
                    if existing.md5.as_deref() == Some(md5) && !existing.uri.is_empty() =>
                {
                    return Ok(NameCheck::Present(existing.clone()));
                }
                Some(_) => {}
            }
        }
        // Both taken by other content: extremely unlikely, but stay safe.
        let mut n = 2;
        loop {
            let candidate = renamed(file_name, &format!("{}-{}", md5, n));
            if !names.contains_key(&candidate) {
                names.insert(
                    candidate.clone(),
                    NamedImage {
                        md5: Some(md5.to_string()),
                        uri: String::new(),
                        album_key: String::new(),
                    },
                );
                return Ok(NameCheck::Upload(candidate));
            }
            n += 1;
        }
    }

    /// Record the image a reserved name was uploaded as.
    pub async fn name_uploaded(&self, name: &str, uri: &str, album_key: &str) {
        if let Some(names) = self.names.get()
            && let Some(entry) = names.lock().unwrap().get_mut(name)
        {
            entry.uri = uri.to_string();
            entry.album_key = album_key.to_string();
        }
    }

    /// Give back a name whose upload failed.
    pub async fn release_name(&self, name: &str) {
        if let Some(names) = self.names.get() {
            let mut names = names.lock().unwrap();
            if names.get(name).is_some_and(|e| e.uri.is_empty()) {
                names.remove(name);
            }
        }
    }

    /// Keys of the day's albums (existing and created by this run).
    pub async fn album_keys(&self) -> HashSet<String> {
        self.series
            .albums()
            .await
            .into_iter()
            .map(|a| a.album.album_key)
            .filter(|k| !k.is_empty())
            .collect()
    }
}

/// A day's series, looked up by the first file that needs it.
type DayCell<T> = Arc<OnceCell<Arc<Day<T>>>>;

/// Day album series under one root folder, loaded as files need them.
pub struct DayAlbums<T> {
    inner: Arc<Inner<T>>,
    days: std::sync::Mutex<HashMap<NaiveDate, DayCell<T>>>,
}

impl<T: Smug> DayAlbums<T> {
    /// `root` is the root folder's node URI, or `None` if it doesn't exist
    /// (only on a dry run).
    pub fn new(smug: Arc<T>, root: Option<String>, dry_run: bool) -> Self {
        DayAlbums {
            inner: Arc::new(Inner {
                smug,
                root,
                dry_run,
                folders: Mutex::new(Folders::default()),
            }),
            days: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The album series for `date`, looking up its existing albums the
    /// first time.
    pub async fn day(&self, date: NaiveDate) -> Result<Arc<Day<T>>> {
        let cell = self
            .days
            .lock()
            .unwrap()
            .entry(date)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        cell.get_or_try_init(|| async {
            let backend = DayBackend {
                inner: self.inner.clone(),
                date,
            };
            let series = AlbumSeries::load(
                backend,
                &day_album_name(date),
                MAX_ALBUM_IMAGES,
                self.inner.dry_run,
            )
            .await
            .with_context(|| format!("Failed to look up the albums for {}", date))?;
            Ok::<_, anyhow::Error>(Arc::new(Day {
                series,
                smug: self.inner.smug.clone(),
                names: OnceCell::new(),
            }))
        })
        .await
        .cloned()
    }

    /// Every day loaded so far.
    pub fn loaded_days(&self) -> Vec<Arc<Day<T>>> {
        self.days
            .lock()
            .unwrap()
            .values()
            .filter_map(|cell| cell.get().cloned())
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// In-memory SmugMug folder tree.
    #[derive(Default)]
    pub struct FakeSmug {
        /// node URI → children
        pub nodes: StdMutex<HashMap<String, Vec<ChildNode>>>,
        /// album key → images
        pub images: StdMutex<HashMap<String, Vec<AlbumImage>>>,
        pub calls: StdMutex<Vec<String>>,
        /// File names whose upload SmugMug refuses (413).
        pub refuse: StdMutex<HashSet<String>>,
        next: StdMutex<u32>,
    }

    impl FakeSmug {
        pub fn id(&self) -> u32 {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        }

        pub fn with_root() -> (Arc<Self>, String) {
            let fake = Arc::new(FakeSmug::default());
            fake.nodes
                .lock()
                .unwrap()
                .insert("/node/root".into(), Vec::new());
            (fake, "/node/root".into())
        }

        pub fn add_folder(&self, parent: &str, name: &str) -> String {
            let uri = format!("/node/f{}", self.id());
            self.nodes
                .lock()
                .unwrap()
                .get_mut(parent)
                .unwrap()
                .push(ChildNode {
                    name: name.into(),
                    node_type: "Folder".into(),
                    uri: uri.clone(),
                    node_id: String::new(),
                    url_name: String::new(),
                    web_uri: None,
                    uris: None,
                });
            self.nodes.lock().unwrap().insert(uri.clone(), Vec::new());
            uri
        }

        pub fn add_album(&self, parent: &str, name: &str, images: &[(&str, &str)]) -> Album {
            let id = self.id();
            let key = format!("A{}", id);
            let album_uri = format!("/api/v2/album/{}", key);
            self.nodes
                .lock()
                .unwrap()
                .get_mut(parent)
                .unwrap()
                .push(ChildNode {
                    name: name.into(),
                    node_type: "Album".into(),
                    uri: format!("/node/a{}", id),
                    node_id: format!("a{}", id),
                    url_name: name.into(),
                    web_uri: None,
                    uris: Some(crate::api::albums::ChildNodeUris {
                        album: Some(crate::api::albums::UriRef {
                            uri: album_uri.clone(),
                        }),
                    }),
                });
            let images = images
                .iter()
                .map(|(file, md5)| AlbumImage {
                    image_key: format!("{}-{}", key, file),
                    file_name: file.to_string(),
                    archived_uri: String::new(),
                    file_size: 1,
                    format: "JPG".into(),
                    uri: format!("{}/image/{}", album_uri, file),
                    title: None,
                    archived_md5: Some(md5.to_string()),
                })
                .collect();
            self.images.lock().unwrap().insert(key.clone(), images);
            Album {
                album_key: key,
                name: name.into(),
                url_name: name.into(),
                node_id: format!("a{}", id),
                uri: album_uri,
                web_uri: None,
                uris: None,
                image_count: None,
            }
        }

        pub fn calls(&self, prefix: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .count()
        }
    }

    impl Smug for FakeSmug {
        async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("children {}", node_uri));
            self.nodes
                .lock()
                .unwrap()
                .get(node_uri)
                .cloned()
                .context("no such node")
        }
        async fn create_folder(&self, parent: &str, name: &str) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_folder {}", name));
            Ok(self.add_folder(parent, name))
        }
        async fn create_album(&self, parent: &str, name: &str) -> Result<Album> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_album {}", name));
            Ok(self.add_album(parent, name, &[]))
        }
        async fn image_count(&self, album: &Album) -> Result<u64> {
            Ok(self
                .images
                .lock()
                .unwrap()
                .get(&album.album_key)
                .map_or(0, |i| i.len() as u64))
        }
        async fn album_images(&self, album_key: &str) -> Result<Vec<AlbumImage>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("album_images {}", album_key));
            Ok(self
                .images
                .lock()
                .unwrap()
                .get(album_key)
                .cloned()
                .unwrap_or_default())
        }
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[tokio::test]
    async fn finds_existing_day_albums_without_creating_anything() {
        let (fake, root) = FakeSmug::with_root();
        let year = fake.add_folder(&root, "2014");
        let month = fake.add_folder(&year, "07");
        let album = fake.add_album(&month, "2014-07-12", &[("a.jpg", "aa")]);

        let days = DayAlbums::new(fake.clone(), Some(root), false);
        let day = days.day(d(2014, 7, 12)).await.unwrap();
        let existing = day.series.existing_albums().await;
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].album.album_key, album.album_key);
        assert_eq!(fake.calls("create"), 0);

        // A second day in the same month reuses the folder listings.
        let before = fake.calls("children");
        days.day(d(2014, 7, 13)).await.unwrap();
        assert_eq!(fake.calls("children"), before);
    }

    #[tokio::test]
    async fn creates_year_month_and_day_only_when_claimed() {
        let (fake, root) = FakeSmug::with_root();
        let days = DayAlbums::new(fake.clone(), Some(root.clone()), false);
        let day = days.day(d(2020, 1, 5)).await.unwrap();
        assert_eq!(fake.calls("create"), 0);

        let album = day.series.claim().await.unwrap();
        assert_eq!(album.name, "2020-01-05");
        assert_eq!(fake.calls("create_folder"), 2);
        assert_eq!(fake.calls("create_album"), 1);

        // Another day that month reuses the new folders.
        let other = days.day(d(2020, 1, 6)).await.unwrap();
        other.series.claim().await.unwrap();
        assert_eq!(fake.calls("create_folder"), 2);

        let nodes = fake.nodes.lock().unwrap();
        let year = nodes[&root].iter().find(|c| c.name == "2020").unwrap();
        let month = nodes[&year.uri].iter().find(|c| c.name == "01").unwrap();
        let names: Vec<_> = nodes[&month.uri].iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["2020-01-05", "2020-01-06"]);
    }

    #[tokio::test]
    async fn dry_run_without_root_uses_placeholders() {
        let (fake, _) = FakeSmug::with_root();
        let days = DayAlbums::new(fake.clone(), None, true);
        let day = days.day(d(2020, 1, 5)).await.unwrap();
        let album = day.series.claim().await.unwrap();
        assert_eq!(album.name, "2020-01-05");
        assert!(album.album_key.is_empty());
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dry_run_creates_nothing() {
        let (fake, root) = FakeSmug::with_root();
        let days = DayAlbums::new(fake.clone(), Some(root), true);
        let day = days.day(d(2020, 1, 5)).await.unwrap();
        day.series.claim().await.unwrap();
        assert_eq!(fake.calls("create"), 0);
    }

    #[tokio::test]
    async fn names_link_same_content_and_rename_different_content() {
        let (fake, root) = FakeSmug::with_root();
        let year = fake.add_folder(&root, "2014");
        let month = fake.add_folder(&year, "07");
        fake.add_album(&month, "2014-07-12", &[("IMG_0001.JPG", "aaaa1111")]);

        let days = DayAlbums::new(fake.clone(), Some(root), false);
        let day = days.day(d(2014, 7, 12)).await.unwrap();

        match day.check_name("IMG_0001.JPG", "aaaa1111").await.unwrap() {
            NameCheck::Present(image) => assert!(image.uri.ends_with("IMG_0001.JPG")),
            other => panic!("{:?}", other),
        }
        assert_eq!(
            day.check_name("IMG_0001.JPG", "bbbb2222cccc")
                .await
                .unwrap(),
            NameCheck::Upload("IMG_0001~bbbb2222.JPG".into())
        );
        assert_eq!(
            day.check_name("IMG_0002.JPG", "dddd").await.unwrap(),
            NameCheck::Upload("IMG_0002.JPG".into())
        );
        // Reserved: another file with that name but other content is renamed.
        assert_eq!(
            day.check_name("IMG_0002.JPG", "eeee").await.unwrap(),
            NameCheck::Upload("IMG_0002~eeee.JPG".into())
        );
        // A failed upload frees its name.
        day.release_name("IMG_0002.JPG").await;
        assert_eq!(
            day.check_name("IMG_0002.JPG", "ffff").await.unwrap(),
            NameCheck::Upload("IMG_0002.JPG".into())
        );
        // An uploaded name is recognized afterwards.
        day.name_uploaded("IMG_0002.JPG", "/img/2", "A9").await;
        assert_eq!(
            day.check_name("IMG_0002.JPG", "ffff").await.unwrap(),
            NameCheck::Present(NamedImage {
                md5: Some("ffff".into()),
                uri: "/img/2".into(),
                album_key: "A9".into()
            })
        );
        assert_eq!(fake.calls("album_images"), 1);
    }

    #[test]
    fn renaming() {
        assert_eq!(
            renamed("IMG_0001.JPG", "0123456789"),
            "IMG_0001~01234567.JPG"
        );
        assert_eq!(renamed("noext", "0123456789"), "noext~01234567");
        assert_eq!(renamed(".hidden", "ab"), ".hidden~ab");
    }
}
