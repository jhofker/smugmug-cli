//! `smugmug-cli sort`: how folders list their contents and how albums list
//! their photos, for one folder or album or, with `--recursive`, everything
//! under it.
//!
//! These are different settings with different values, so they're given
//! separately (`--folders-by`, `--albums-by`) and a run only touches the kind
//! you named: `--recursive --folders-by name` never changes an album. A
//! node that already has the setting is left alone, so a run can be repeated
//! (SmugMug's default for a new folder is by date modified, newest first, so
//! folders made by older versions of this tool need fixing; later runs only
//! find the ones made since).

use anyhow::{Result, bail};
use clap::ValueEnum;
use futures_util::stream::{self, StreamExt};

use crate::api::SmugMugClient;
use crate::api::albums::{AlbumSettingsUpdate, ChildNode};

/// How a folder lists its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FolderSort {
    Name,
    DateAdded,
    DateModified,
    /// The order set by hand in SmugMug's organizer
    Manual,
}

impl FolderSort {
    fn api_name(self) -> &'static str {
        match self {
            FolderSort::Name => "Name",
            FolderSort::DateAdded => "DateAdded",
            FolderSort::DateModified => "DateModified",
            FolderSort::Manual => "SortIndex",
        }
    }
}

/// How an album lists its photos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AlbumSort {
    /// The order set by hand in SmugMug's organizer
    Manual,
    Caption,
    Filename,
    DateUploaded,
    DateModified,
    DateTaken,
}

impl AlbumSort {
    fn api_name(self) -> &'static str {
        match self {
            AlbumSort::Manual => "Position",
            AlbumSort::Caption => "Caption",
            AlbumSort::Filename => "Filename",
            AlbumSort::DateUploaded => "Date Uploaded",
            AlbumSort::DateModified => "Date Modified",
            AlbumSort::DateTaken => "Date Taken",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Direction {
    #[value(alias = "ascending")]
    Asc,
    #[value(alias = "descending")]
    Desc,
}

impl Direction {
    fn api_name(self) -> &'static str {
        match self {
            Direction::Asc => "Ascending",
            Direction::Desc => "Descending",
        }
    }
}

pub struct SortOptions {
    /// Folder or album, as a path of names from the top of the account
    /// ("Backup", "Backup/2014/07", "Backup/2014/07/2014-07-12").
    pub path: String,
    pub folders: Option<FolderSort>,
    pub albums: Option<AlbumSort>,
    pub direction: Direction,
    /// Everything under `path` too.
    pub recursive: bool,
    /// Report what would change without changing it.
    pub dry_run: bool,
}

/// What a run did (or, on a dry run, would do).
#[derive(Debug, Default)]
pub struct SortSummary {
    pub folders_changed: usize,
    pub folders_already: usize,
    pub albums_changed: usize,
    pub albums_already: usize,
    /// Paths of the first few folders and albums changed.
    pub examples: Vec<String>,
    pub failed: Vec<String>,
}

const EXAMPLES: usize = 10;

/// The SmugMug calls sorting needs.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait SortBackend {
    /// The account's root folder node.
    async fn root(&self) -> Result<String>;
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>>;
    /// An album's photo sort as SmugMug names it: (method, direction).
    async fn album_sort(&self, album_key: &str) -> Result<(String, String)>;
    async fn set_folder_sort(&self, node_uri: &str, method: &str, direction: &str) -> Result<()>;
    async fn set_album_sort(&self, album_key: &str, method: &str, direction: &str) -> Result<()>;
}

impl SortBackend for SmugMugClient {
    async fn root(&self) -> Result<String> {
        Ok(self.auth_user().await?.root_node_uri)
    }
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
        self.list_children(node_uri).await
    }
    async fn album_sort(&self, album_key: &str) -> Result<(String, String)> {
        self.get_album_sort(album_key).await
    }
    async fn set_folder_sort(&self, node_uri: &str, method: &str, direction: &str) -> Result<()> {
        SmugMugClient::set_folder_sort(self, node_uri, method, direction).await
    }
    async fn set_album_sort(&self, album_key: &str, method: &str, direction: &str) -> Result<()> {
        self.update_album_settings(
            album_key,
            AlbumSettingsUpdate {
                privacy: None,
                description: None,
                keywords: None,
                sort_method: Some(method.to_string()),
                sort_direction: Some(direction.to_string()),
            },
        )
        .await
    }
}

/// Something to set the sort order of.
enum Item {
    Folder {
        path: String,
        uri: String,
        current: (String, String),
    },
    Album {
        path: String,
        key: String,
    },
}

/// The folder or album at `path`.
async fn find<B: SortBackend>(backend: &B, path: &str) -> Result<(ChildNode, String)> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        bail!("Give the folder or album to sort, e.g. Backup or Backup/2014/07");
    }
    let mut uri = backend.root().await?;
    let mut shown = String::new();
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        let matches: Vec<ChildNode> = backend
            .children(&uri)
            .await?
            .into_iter()
            .filter(|c| {
                c.name == *part && (c.node_type == "Folder" || (last && c.node_type == "Album"))
            })
            .collect();
        if !shown.is_empty() {
            shown.push('/');
        }
        shown.push_str(part);
        match matches.len() {
            1 => {
                let node = matches.into_iter().next().unwrap();
                if last {
                    return Ok((node, shown));
                }
                uri = node.uri;
            }
            0 => bail!(
                "No folder{} named '{}' at '{}'",
                if last { " or album" } else { "" },
                part,
                shown
            ),
            n => bail!(
                "{} folders and albums are named '{}' at '{}'; rename one first",
                n,
                part,
                shown
            ),
        }
    }
    unreachable!("the loop returns on its last part")
}

fn folder_item(node: &ChildNode, path: String) -> Item {
    Item::Folder {
        path,
        uri: node.uri.clone(),
        current: (
            node.sort_method.clone().unwrap_or_default(),
            node.sort_direction.clone().unwrap_or_default(),
        ),
    }
}

fn album_item(node: &ChildNode, path: String) -> Option<Item> {
    node.album().map(|album| Item::Album {
        path,
        key: album.album_key,
    })
}

/// The folders and albums to change, per `options`.
async fn collect<B: SortBackend>(
    backend: &B,
    options: &SortOptions,
) -> Result<(Vec<Item>, Vec<String>)> {
    let (target, shown) = find(backend, &options.path).await?;
    let is_folder = target.node_type == "Folder";

    if is_folder && !options.recursive && options.folders.is_none() {
        bail!(
            "'{}' is a folder: give --folders-by to set how it lists its contents (or --recursive to reach the albums inside)",
            shown
        );
    }
    if !is_folder && options.albums.is_none() {
        bail!(
            "'{}' is an album: give --albums-by to set how it lists its photos",
            shown
        );
    }

    let mut items = Vec::new();
    let mut failed = Vec::new();
    if is_folder {
        if options.folders.is_some() {
            items.push(folder_item(&target, shown.clone()));
        }
        if options.recursive {
            let mut pending = vec![(target.uri.clone(), shown)];
            while let Some((uri, path)) = pending.pop() {
                // A folder that can't be listed is reported, and the rest
                // of the tree is still done.
                let children = match backend.children(&uri).await {
                    Ok(children) => children,
                    Err(e) => {
                        failed.push(format!("{}: couldn't list what's in it: {:#}", path, e));
                        continue;
                    }
                };
                for child in children {
                    let child_path = format!("{}/{}", path, child.name);
                    match child.node_type.as_str() {
                        "Folder" => {
                            if options.folders.is_some() {
                                items.push(folder_item(&child, child_path.clone()));
                            }
                            pending.push((child.uri.clone(), child_path));
                        }
                        "Album" if options.albums.is_some() => {
                            items.extend(album_item(&child, child_path));
                        }
                        _ => {}
                    }
                }
            }
        }
    } else if let Some(item) = album_item(&target, shown) {
        items.push(item);
    }
    Ok((items, failed))
}

enum Outcome {
    Changed(String),
    Already,
    Failed(String),
}

/// Albums and folders handled at once.
const CONCURRENCY: usize = 8;

async fn apply<B: SortBackend>(backend: &B, item: Item, options: &SortOptions) -> (bool, Outcome) {
    let direction = options.direction.api_name();
    match item {
        Item::Folder { path, uri, current } => {
            let method = options
                .folders
                .expect("folders are only collected with --folders-by")
                .api_name();
            if current.0 == method && current.1 == direction {
                return (true, Outcome::Already);
            }
            if options.dry_run {
                return (true, Outcome::Changed(path));
            }
            match backend.set_folder_sort(&uri, method, direction).await {
                Ok(()) => (true, Outcome::Changed(path)),
                Err(e) => (true, Outcome::Failed(format!("{}: {:#}", path, e))),
            }
        }
        Item::Album { path, key } => {
            let method = options
                .albums
                .expect("albums are only collected with --albums-by")
                .api_name();
            let current = match backend.album_sort(&key).await {
                Ok(current) => current,
                Err(e) => return (false, Outcome::Failed(format!("{}: {:#}", path, e))),
            };
            if current.0 == method && current.1 == direction {
                return (false, Outcome::Already);
            }
            if options.dry_run {
                return (false, Outcome::Changed(path));
            }
            match backend.set_album_sort(&key, method, direction).await {
                Ok(()) => (false, Outcome::Changed(path)),
                Err(e) => (false, Outcome::Failed(format!("{}: {:#}", path, e))),
            }
        }
    }
}

/// Set the sort order of `options.path` (and, if recursive, everything
/// under it). Failures on single items are collected in the summary and
/// don't stop the rest.
pub async fn run<B: SortBackend>(backend: &B, options: &SortOptions) -> Result<SortSummary> {
    if options.folders.is_none() && options.albums.is_none() {
        bail!("Say what to sort: --folders-by and/or --albums-by");
    }
    let (items, listing_failures) = collect(backend, options).await?;
    let total = items.len();
    let mut summary = SortSummary {
        failed: listing_failures,
        ..SortSummary::default()
    };
    let mut done = 0;

    let mut results = stream::iter(items)
        .map(|item| apply(backend, item, options))
        .buffer_unordered(CONCURRENCY);
    while let Some((is_folder, outcome)) = results.next().await {
        done += 1;
        match outcome {
            Outcome::Changed(path) => {
                if is_folder {
                    summary.folders_changed += 1;
                } else {
                    summary.albums_changed += 1;
                }
                if summary.examples.len() < EXAMPLES {
                    summary.examples.push(path);
                }
            }
            Outcome::Already if is_folder => summary.folders_already += 1,
            Outcome::Already => summary.albums_already += 1,
            Outcome::Failed(message) => summary.failed.push(message),
        }
        if !options.dry_run && done % 100 == 0 && done < total {
            println!("  {}/{} done...", done, total);
        }
    }
    Ok(summary)
}

/// Tell the user what a run did, or would do.
pub fn print_summary(summary: &SortSummary, options: &SortOptions) {
    let verb = if options.dry_run {
        "Would change"
    } else {
        "Changed"
    };
    let mut parts = Vec::new();
    if options.folders.is_some() {
        parts.push(format!(
            "{} {} folder{} ({} already right)",
            verb,
            summary.folders_changed,
            if summary.folders_changed == 1 {
                ""
            } else {
                "s"
            },
            summary.folders_already
        ));
    }
    if options.albums.is_some() {
        parts.push(format!(
            "{} {} album{} ({} already right)",
            verb,
            summary.albums_changed,
            if summary.albums_changed == 1 { "" } else { "s" },
            summary.albums_already
        ));
    }
    println!();
    for part in parts {
        println!("{}", part);
    }
    if options.dry_run && !summary.examples.is_empty() {
        let shown = summary.examples.len();
        let total = summary.folders_changed + summary.albums_changed;
        println!("\nFor example:");
        for path in &summary.examples {
            println!("  {}", path);
        }
        if total > shown {
            println!("  ... and {} more", total - shown);
        }
    }
    if !summary.failed.is_empty() {
        println!(
            "\n{} couldn't be updated (run again to retry):",
            summary.failed.len()
        );
        for failure in summary.failed.iter().take(20) {
            println!("  {}", failure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::albums::{ChildNodeUris, UriRef};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory account: nodes by URI, albums' sorts by key.
    #[derive(Default)]
    struct Fake {
        children: Mutex<HashMap<String, Vec<ChildNode>>>,
        albums: Mutex<HashMap<String, (String, String)>>,
        /// Node URIs and album keys whose update fails.
        fail: Mutex<Vec<String>>,
        /// Folders whose listing fails.
        fail_listing: Mutex<Vec<String>>,
        sets: Mutex<Vec<String>>,
        next: Mutex<u32>,
    }

    impl Fake {
        fn new() -> Self {
            let fake = Fake::default();
            fake.children
                .lock()
                .unwrap()
                .insert("/node/root".into(), Vec::new());
            fake
        }

        fn id(&self) -> u32 {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        }

        fn folder(&self, parent: &str, name: &str, sort: (&str, &str)) -> String {
            let uri = format!("/node/f{}", self.id());
            self.children
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
                    sort_method: Some(sort.0.into()),
                    sort_direction: Some(sort.1.into()),
                });
            self.children
                .lock()
                .unwrap()
                .insert(uri.clone(), Vec::new());
            uri
        }

        fn album(&self, parent: &str, name: &str, sort: (&str, &str)) -> String {
            let key = format!("A{}", self.id());
            self.children
                .lock()
                .unwrap()
                .get_mut(parent)
                .unwrap()
                .push(ChildNode {
                    name: name.into(),
                    node_type: "Album".into(),
                    uri: format!("/node/{}", key),
                    node_id: String::new(),
                    url_name: String::new(),
                    web_uri: None,
                    uris: Some(ChildNodeUris {
                        album: Some(UriRef {
                            uri: format!("/api/v2/album/{}", key),
                        }),
                    }),
                    // A folder-style sort on an album node means nothing here
                    sort_method: Some("Name".into()),
                    sort_direction: Some("Ascending".into()),
                });
            self.albums
                .lock()
                .unwrap()
                .insert(key.clone(), (sort.0.into(), sort.1.into()));
            key
        }

        fn folder_sort(&self, uri: &str) -> (String, String) {
            for list in self.children.lock().unwrap().values() {
                if let Some(c) = list.iter().find(|c| c.uri == uri) {
                    return (
                        c.sort_method.clone().unwrap(),
                        c.sort_direction.clone().unwrap(),
                    );
                }
            }
            panic!("no node {}", uri);
        }

        fn album_sort_of(&self, key: &str) -> (String, String) {
            self.albums.lock().unwrap()[key].clone()
        }

        fn set_count(&self) -> usize {
            self.sets.lock().unwrap().len()
        }

        fn fails_for(&self, id: &str) {
            self.fail.lock().unwrap().push(id.into());
        }
    }

    impl SortBackend for Fake {
        async fn root(&self) -> Result<String> {
            Ok("/node/root".into())
        }
        async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
            if self
                .fail_listing
                .lock()
                .unwrap()
                .iter()
                .any(|f| f == node_uri)
            {
                bail!("503");
            }
            Ok(self.children.lock().unwrap()[node_uri].clone())
        }
        async fn album_sort(&self, key: &str) -> Result<(String, String)> {
            Ok(self.album_sort_of(key))
        }
        async fn set_folder_sort(&self, uri: &str, method: &str, direction: &str) -> Result<()> {
            if self.fail.lock().unwrap().iter().any(|f| f == uri) {
                bail!("503");
            }
            self.sets.lock().unwrap().push(uri.into());
            for list in self.children.lock().unwrap().values_mut() {
                if let Some(c) = list.iter_mut().find(|c| c.uri == uri) {
                    c.sort_method = Some(method.into());
                    c.sort_direction = Some(direction.into());
                }
            }
            Ok(())
        }
        async fn set_album_sort(&self, key: &str, method: &str, direction: &str) -> Result<()> {
            if self.fail.lock().unwrap().iter().any(|f| f == key) {
                bail!("503");
            }
            self.sets.lock().unwrap().push(key.into());
            self.albums
                .lock()
                .unwrap()
                .insert(key.into(), (method.into(), direction.into()));
            Ok(())
        }
    }

    const DEFAULT: (&str, &str) = ("DateModified", "Descending");
    const NAME_ASC: (&str, &str) = ("Name", "Ascending");

    fn options(path: &str) -> SortOptions {
        SortOptions {
            path: path.into(),
            folders: None,
            albums: None,
            direction: Direction::Asc,
            recursive: false,
            dry_run: false,
        }
    }

    /// Backup/{2014/{07/{2014-07-12 (album)}, 08}, 2015}, plus an unrelated
    /// Other/ folder. Returns the fake and the interesting URIs/keys.
    fn account() -> (Fake, Tree) {
        let fake = Fake::new();
        let backup = fake.folder("/node/root", "Backup", NAME_ASC);
        let y2014 = fake.folder(&backup, "2014", DEFAULT);
        let m07 = fake.folder(&y2014, "07", DEFAULT);
        let m08 = fake.folder(&y2014, "08", DEFAULT);
        let y2015 = fake.folder(&backup, "2015", DEFAULT);
        let day = fake.album(&m07, "2014-07-12", ("Date Taken", "Ascending"));
        let other = fake.folder("/node/root", "Other", DEFAULT);
        (
            fake,
            Tree {
                backup,
                y2014,
                m07,
                m08,
                y2015,
                day,
                other,
            },
        )
    }

    struct Tree {
        backup: String,
        y2014: String,
        m07: String,
        m08: String,
        y2015: String,
        day: String,
        other: String,
    }

    #[tokio::test]
    async fn recursive_folder_sort_changes_folders_below_and_nothing_else() {
        let (fake, t) = account();
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.recursive = true;
        let summary = run(&fake, &o).await.unwrap();

        // Backup was already right; the four below were not.
        assert_eq!((summary.folders_changed, summary.folders_already), (4, 1));
        for uri in [&t.y2014, &t.m07, &t.m08, &t.y2015] {
            assert_eq!(fake.folder_sort(uri), ("Name".into(), "Ascending".into()));
        }
        // Not under Backup, and albums only change when asked
        assert_eq!(
            fake.folder_sort(&t.other),
            (DEFAULT.0.into(), DEFAULT.1.into())
        );
        assert_eq!(
            fake.album_sort_of(&t.day),
            ("Date Taken".into(), "Ascending".into())
        );
        assert_eq!(summary.albums_changed + summary.albums_already, 0);
        assert!(summary.failed.is_empty());
    }

    #[tokio::test]
    async fn repeating_a_run_changes_nothing() {
        let (fake, _) = account();
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.recursive = true;
        run(&fake, &o).await.unwrap();
        let sets = fake.set_count();

        let again = run(&fake, &o).await.unwrap();
        assert_eq!((again.folders_changed, again.folders_already), (0, 5));
        assert_eq!(fake.set_count(), sets);
    }

    #[tokio::test]
    async fn without_recursive_only_the_named_folder_changes() {
        let (fake, t) = account();
        let mut o = options("Backup/2014");
        o.folders = Some(FolderSort::DateAdded);
        o.direction = Direction::Desc;
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!(summary.folders_changed, 1);
        assert_eq!(
            fake.folder_sort(&t.y2014),
            ("DateAdded".into(), "Descending".into())
        );
        assert_eq!(
            fake.folder_sort(&t.m07),
            (DEFAULT.0.into(), DEFAULT.1.into())
        );
    }

    #[tokio::test]
    async fn albums_are_sorted_by_their_own_values() {
        let (fake, t) = account();
        let mut o = options("Backup/2014/07/2014-07-12");
        o.albums = Some(AlbumSort::Filename);
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!(summary.albums_changed, 1);
        assert_eq!(
            fake.album_sort_of(&t.day),
            ("Filename".into(), "Ascending".into())
        );

        // Recursive from above, albums only: no folder changes
        let mut o = options("Backup");
        o.albums = Some(AlbumSort::DateTaken);
        o.recursive = true;
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!((summary.albums_changed, summary.folders_changed), (1, 0));
        assert_eq!(
            fake.album_sort_of(&t.day),
            ("Date Taken".into(), "Ascending".into())
        );
        assert_eq!(
            fake.folder_sort(&t.m07),
            (DEFAULT.0.into(), DEFAULT.1.into())
        );
    }

    #[tokio::test]
    async fn folders_and_albums_can_be_sorted_in_one_run() {
        let (fake, t) = account();
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.albums = Some(AlbumSort::Filename);
        o.direction = Direction::Desc;
        o.recursive = true;
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!((summary.folders_changed, summary.albums_changed), (5, 1));
        assert_eq!(
            fake.folder_sort(&t.backup),
            ("Name".into(), "Descending".into())
        );
        assert_eq!(
            fake.album_sort_of(&t.day),
            ("Filename".into(), "Descending".into())
        );
    }

    #[tokio::test]
    async fn dry_run_reports_without_changing() {
        let (fake, t) = account();
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.recursive = true;
        o.dry_run = true;
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!(summary.folders_changed, 4);
        assert_eq!(fake.set_count(), 0);
        assert_eq!(
            fake.folder_sort(&t.y2014),
            (DEFAULT.0.into(), DEFAULT.1.into())
        );
        assert!(summary.examples.contains(&"Backup/2014/07".to_string()));
    }

    #[tokio::test]
    async fn one_failure_is_reported_and_the_rest_still_change() {
        let (fake, t) = account();
        fake.fails_for(&t.m07);
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.recursive = true;
        let summary = run(&fake, &o).await.unwrap();
        assert_eq!(summary.folders_changed, 3);
        assert_eq!(summary.failed.len(), 1);
        assert!(
            summary.failed[0].starts_with("Backup/2014/07:"),
            "{:?}",
            summary.failed
        );
        assert_eq!(
            fake.folder_sort(&t.y2015),
            ("Name".into(), "Ascending".into())
        );
    }

    /// The error `run` gives for `options` against the sample account.
    async fn error_for(options: SortOptions) -> String {
        let (fake, _) = account();
        run(&fake, &options).await.unwrap_err().to_string()
    }

    fn folders_by_name(path: &str) -> SortOptions {
        let mut o = options(path);
        o.folders = Some(FolderSort::Name);
        o
    }

    #[tokio::test]
    async fn a_folder_that_cant_be_listed_is_reported_and_the_rest_still_done() {
        let (fake, t) = account();
        fake.fail_listing.lock().unwrap().push(t.y2014.clone());
        let mut o = options("Backup");
        o.folders = Some(FolderSort::Name);
        o.recursive = true;
        let summary = run(&fake, &o).await.unwrap();

        // 2014 itself and 2015 changed; what's inside 2014 couldn't be reached
        assert_eq!(summary.folders_changed, 2);
        assert_eq!(
            fake.folder_sort(&t.y2014),
            ("Name".into(), "Ascending".into())
        );
        assert_eq!(
            fake.folder_sort(&t.y2015),
            ("Name".into(), "Ascending".into())
        );
        assert_eq!(
            fake.folder_sort(&t.m07),
            (DEFAULT.0.into(), DEFAULT.1.into())
        );
        assert_eq!(summary.failed.len(), 1);
        assert!(
            summary.failed[0].starts_with("Backup/2014: couldn't list"),
            "{:?}",
            summary.failed
        );
    }

    #[tokio::test]
    async fn asks_for_what_it_needs() {
        // Nothing to sort by
        assert!(error_for(options("Backup")).await.contains("--folders-by"));

        // A folder, but only an album sort given and no --recursive
        let mut o = options("Backup");
        o.albums = Some(AlbumSort::DateTaken);
        assert!(error_for(o).await.contains("is a folder"));

        // An album, but only a folder sort given
        let err = error_for(folders_by_name("Backup/2014/07/2014-07-12")).await;
        assert!(err.contains("is an album"), "{}", err);

        // No path, a missing one, and an album in the middle of a path
        let err = error_for(folders_by_name("")).await;
        assert!(err.contains("Give the folder or album"), "{}", err);
        let err = error_for(folders_by_name("Backup/1999")).await;
        assert!(err.contains("No folder or album named '1999'"), "{}", err);
        let err = error_for(folders_by_name("Backup/2014/07/2014-07-12/x")).await;
        assert!(err.contains("No folder named '2014-07-12'"), "{}", err);
    }

    #[tokio::test]
    async fn two_nodes_with_one_name_are_not_guessed_between() {
        let (fake, t) = account();
        fake.folder(&t.backup, "2014", DEFAULT);
        let mut o = options("Backup/2014");
        o.folders = Some(FolderSort::Name);
        let err = run(&fake, &o).await.unwrap_err().to_string();
        assert!(
            err.contains("2 folders and albums are named '2014'"),
            "{}",
            err
        );
    }

    #[test]
    fn api_names() {
        assert_eq!(FolderSort::Manual.api_name(), "SortIndex");
        assert_eq!(FolderSort::DateModified.api_name(), "DateModified");
        assert_eq!(AlbumSort::Manual.api_name(), "Position");
        assert_eq!(AlbumSort::DateTaken.api_name(), "Date Taken");
        assert_eq!(Direction::Desc.api_name(), "Descending");
    }
}
