//! `sm folders rename|move|delete` and `sm albums move`: reorganizing the
//! folder tree. Folders and albums are named by paths of names from the top
//! of the account, as `sort` takes them (see `nodes::find`).
//!
//! SmugMug calls: `PATCH node/<id>` with `Name` (and `UrlName`) to rename,
//! `POST node/<folder>!movenodes` with `MoveUris` to move, `DELETE node/<id>`
//! to delete (which deletes everything inside a folder; `main` asks first).

use anyhow::{Result, bail};

use crate::api::SmugMugClient;
use crate::api::albums::ChildNode;
use crate::nodes::{self, NodeLookup};

/// The SmugMug calls reorganizing needs.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait OrganizeBackend: NodeLookup {
    async fn rename(&self, node_uri: &str, name: &str, url_name: Option<&str>) -> Result<()>;
    async fn move_into(&self, folder_uri: &str, node_uris: &[String]) -> Result<()>;
}

impl OrganizeBackend for SmugMugClient {
    async fn rename(&self, node_uri: &str, name: &str, url_name: Option<&str>) -> Result<()> {
        self.rename_node(node_uri, name, url_name).await
    }
    async fn move_into(&self, folder_uri: &str, node_uris: &[String]) -> Result<()> {
        self.move_nodes(folder_uri, node_uris).await
    }
}

/// What a command may act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Album,
    Either,
}

/// A folder or album found by its path.
#[derive(Debug, Clone)]
pub struct Target {
    pub node: ChildNode,
    /// The path as matched, e.g. "Backup/2014".
    pub path: String,
}

impl Target {
    pub fn is_folder(&self) -> bool {
        self.node.node_type == "Folder"
    }
}

/// The folder or album at `path`, which must be of `kind`.
pub async fn find<B: NodeLookup>(backend: &B, path: &str, kind: Kind) -> Result<Target> {
    let (node, path) = nodes::find(backend, path).await?;
    match (kind, node.node_type.as_str()) {
        (Kind::Folder, "Album") => bail!(
            "'{}' is an album, not a folder (use `sm albums` for albums)",
            path
        ),
        (Kind::Album, "Folder") => bail!(
            "'{}' is a folder, not an album (use `sm folders` for folders)",
            path
        ),
        _ => Ok(Target { node, path }),
    }
}

/// The node URI of the folder at `path`, the root for "" or "/", and how to
/// show it.
async fn folder_uri<B: NodeLookup>(backend: &B, path: &str) -> Result<(String, String)> {
    if nodes::parts(path).is_empty() {
        return Ok((backend.root().await?, "the top of your account".to_string()));
    }
    let target = find(backend, path, Kind::Folder).await?;
    Ok((target.node.uri, format!("'{}'", target.path)))
}

/// The path of the folder holding `path` ("" for the top).
fn parent_path(path: &str) -> String {
    let parts = nodes::parts(path);
    parts[..parts.len().saturating_sub(1)].join("/")
}

/// Check a new name: SmugMug allows 1 to 254 characters, and a `/` would
/// make the folder impossible to name in a path.
fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("The new name is empty");
    }
    if name.contains('/') {
        bail!("Names can't contain '/', which separates folders in paths");
    }
    if name.chars().count() > 254 {
        bail!("Names can be at most 254 characters");
    }
    Ok(())
}

/// Check a web address part: SmugMug wants 1 to 60 characters starting
/// with a capital letter; letters, digits and hyphens are safe in a URL.
fn check_url_name(url_name: &str) -> Result<()> {
    let valid = url_name.chars().count() <= 60
        && url_name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && url_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        bail!(
            "The URL name must start with a capital letter and use only letters, digits and hyphens (at most 60), e.g. Summer-2026"
        );
    }
    Ok(())
}

/// Rename the folder or album at `path` to `new_name` (and its web address
/// part to `url_name`, if given). Returns the old path.
pub async fn rename<B: OrganizeBackend>(
    backend: &B,
    path: &str,
    new_name: &str,
    url_name: Option<&str>,
) -> Result<String> {
    check_name(new_name)?;
    if let Some(url_name) = url_name {
        check_url_name(url_name)?;
    }
    let target = find(backend, path, Kind::Either).await?;
    if target.node.name == new_name && url_name.is_none() {
        bail!("'{}' already has that name", target.path);
    }

    // A clash would make both unreachable by path.
    let (parent_uri, _) = folder_uri(backend, &parent_path(&target.path)).await?;
    if backend
        .children(&parent_uri)
        .await?
        .iter()
        .any(|c| c.name == new_name && c.uri != target.node.uri)
    {
        bail!(
            "There's already a folder or album named '{}' next to '{}'",
            new_name,
            target.path
        );
    }

    backend.rename(&target.node.uri, new_name, url_name).await?;
    Ok(target.path)
}

/// Move the folder or album at `path` (of `kind`) into the folder at
/// `destination` ("" or "/" for the top of the account). Returns the moved
/// path and how to show the destination.
pub async fn move_to<B: OrganizeBackend>(
    backend: &B,
    path: &str,
    destination: &str,
    kind: Kind,
) -> Result<(String, String)> {
    let target = find(backend, path, kind).await?;
    let destination_parts = nodes::parts(destination);
    let target_parts = nodes::parts(&target.path);
    if target.is_folder() && destination_parts.starts_with(&target_parts) {
        bail!("Can't move '{}' into itself", target.path);
    }

    let (destination_uri, shown) = folder_uri(backend, destination).await?;
    let siblings = backend.children(&destination_uri).await?;
    if siblings.iter().any(|c| c.uri == target.node.uri) {
        bail!("'{}' is already in {}", target.path, shown);
    }
    if siblings.iter().any(|c| c.name == target.node.name) {
        bail!(
            "{} already has a folder or album named '{}'",
            shown,
            target.node.name
        );
    }

    backend
        .move_into(&destination_uri, std::slice::from_ref(&target.node.uri))
        .await?;
    Ok((target.path, shown))
}

/// What's directly inside a folder, for the delete confirmation.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub folders: usize,
    pub albums: usize,
    pub pages: usize,
}

/// The folder at `path` and what's directly inside it.
pub async fn inspect_folder<B: NodeLookup>(backend: &B, path: &str) -> Result<(Target, Contents)> {
    let target = find(backend, path, Kind::Folder).await?;
    let mut contents = Contents::default();
    for child in backend.children(&target.node.uri).await? {
        match child.node_type.as_str() {
            "Folder" => contents.folders += 1,
            "Album" => contents.albums += 1,
            _ => contents.pages += 1,
        }
    }
    Ok((target, contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An in-memory folder tree: children by parent node URI.
    #[derive(Default)]
    struct Fake {
        children: Mutex<HashMap<String, Vec<ChildNode>>>,
        calls: Mutex<Vec<String>>,
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

        fn add(&self, parent: &str, name: &str, node_type: &str) -> String {
            let uri = {
                let mut n = self.next.lock().unwrap();
                *n += 1;
                format!("/node/n{}", n)
            };
            let mut children = self.children.lock().unwrap();
            children.get_mut(parent).unwrap().push(ChildNode {
                name: name.into(),
                node_type: node_type.into(),
                uri: uri.clone(),
                node_id: String::new(),
                url_name: String::new(),
                web_uri: None,
                uris: None,
                sort_method: None,
                sort_direction: None,
            });
            if node_type == "Folder" {
                children.insert(uri.clone(), Vec::new());
            }
            uri
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl NodeLookup for Fake {
        async fn root(&self) -> Result<String> {
            Ok("/node/root".into())
        }
        async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
            Ok(self.children.lock().unwrap()[node_uri].clone())
        }
    }

    impl OrganizeBackend for Fake {
        async fn rename(&self, uri: &str, name: &str, url_name: Option<&str>) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("rename {} {} {:?}", uri, name, url_name));
            Ok(())
        }
        async fn move_into(&self, folder: &str, uris: &[String]) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("move {} -> {}", uris.join(","), folder));
            Ok(())
        }
    }

    /// root: Trips/{Paris (album), 2026/}, Inbox (album), Archive/
    fn tree() -> (Fake, HashMap<&'static str, String>) {
        let fake = Fake::new();
        let mut ids = HashMap::new();
        let trips = fake.add("/node/root", "Trips", "Folder");
        ids.insert("Paris", fake.add(&trips, "Paris", "Album"));
        ids.insert("2026", fake.add(&trips, "2026", "Folder"));
        ids.insert("Inbox", fake.add("/node/root", "Inbox", "Album"));
        ids.insert("Archive", fake.add("/node/root", "Archive", "Folder"));
        ids.insert("Trips", trips);
        (fake, ids)
    }

    #[tokio::test]
    async fn renames_folders_and_albums() {
        let (fake, ids) = tree();
        assert_eq!(
            rename(&fake, "Trips/2026", "Twenty", None).await.unwrap(),
            "Trips/2026"
        );
        rename(&fake, "Inbox", "Unsorted", Some("Unsorted"))
            .await
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                format!("rename {} Twenty None", ids["2026"]),
                format!("rename {} Unsorted Some(\"Unsorted\")", ids["Inbox"]),
            ]
        );
    }

    #[tokio::test]
    async fn rename_refuses_clashes_and_bad_names() {
        let (fake, _) = tree();
        let err = |e: anyhow::Error| e.to_string();
        assert!(
            err(rename(&fake, "Trips", "Archive", None).await.unwrap_err())
                .contains("already a folder or album named 'Archive'")
        );
        assert!(
            err(rename(&fake, "Trips", "a/b", None).await.unwrap_err()).contains("can't contain")
        );
        assert!(err(rename(&fake, "Trips", " ", None).await.unwrap_err()).contains("empty"));
        assert!(
            err(rename(&fake, "Trips", "Journeys", Some("journeys"))
                .await
                .unwrap_err())
            .contains("capital letter")
        );
        assert!(
            err(rename(&fake, "Nowhere", "X", None).await.unwrap_err())
                .contains("No folder or album named 'Nowhere'")
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn moves_albums_and_folders() {
        let (fake, ids) = tree();
        let (moved, to) = move_to(&fake, "Trips/Paris", "Archive", Kind::Album)
            .await
            .unwrap();
        assert_eq!((moved.as_str(), to.as_str()), ("Trips/Paris", "'Archive'"));
        let (_, to) = move_to(&fake, "Trips/2026", "/", Kind::Folder)
            .await
            .unwrap();
        assert_eq!(to, "the top of your account");
        assert_eq!(
            fake.calls(),
            vec![
                format!("move {} -> {}", ids["Paris"], ids["Archive"]),
                format!("move {} -> /node/root", ids["2026"]),
            ]
        );
    }

    #[tokio::test]
    async fn move_refuses_what_would_go_wrong() {
        let (fake, ids) = tree();
        let err = |e: anyhow::Error| e.to_string();
        assert!(
            err(move_to(&fake, "Trips", "Trips/2026", Kind::Folder)
                .await
                .unwrap_err())
            .contains("into itself")
        );
        assert!(
            err(move_to(&fake, "Trips", "Trips", Kind::Folder)
                .await
                .unwrap_err())
            .contains("into itself")
        );
        assert!(
            err(move_to(&fake, "Inbox", "", Kind::Album).await.unwrap_err())
                .contains("already in the top")
        );
        assert!(
            err(move_to(&fake, "Trips", "Archive", Kind::Album)
                .await
                .unwrap_err())
            .contains("is a folder, not an album")
        );
        assert!(
            err(move_to(&fake, "Inbox", "Trips", Kind::Folder)
                .await
                .unwrap_err())
            .contains("is an album, not a folder")
        );
        assert!(
            err(move_to(&fake, "Trips/Paris", "Inbox", Kind::Album)
                .await
                .unwrap_err())
            .contains("is an album, not a folder")
        );
        fake.add(&ids["Archive"], "Paris", "Album");
        assert!(
            err(move_to(&fake, "Trips/Paris", "Archive", Kind::Album)
                .await
                .unwrap_err())
            .contains("already has a folder or album named 'Paris'")
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn inspect_counts_what_a_delete_would_take() {
        let (fake, _) = tree();
        let (target, contents) = inspect_folder(&fake, "Trips").await.unwrap();
        assert_eq!(target.path, "Trips");
        assert_eq!(
            contents,
            Contents {
                folders: 1,
                albums: 1,
                pages: 0
            }
        );
        assert!(
            inspect_folder(&fake, "Inbox")
                .await
                .unwrap_err()
                .to_string()
                .contains("is an album")
        );
    }

    #[test]
    fn parent_paths() {
        assert_eq!(parent_path("Trips/2026"), "Trips");
        assert_eq!(parent_path("Trips"), "");
        assert_eq!(parent_path("/A/B/C/"), "A/B");
    }
}
