//! Finding folders and albums by a path of names from the top of the
//! account ("Backup", "Backup/2014/07", "Backup/2014/07/2014-07-12"), as
//! `sort` and the folder commands take them.

use anyhow::{Result, bail};

use crate::api::SmugMugClient;
use crate::api::albums::ChildNode;

/// Listing the account's folder tree.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait NodeLookup {
    /// The account's root folder node.
    async fn root(&self) -> Result<String>;
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>>;
}

impl NodeLookup for SmugMugClient {
    async fn root(&self) -> Result<String> {
        Ok(self.auth_user().await?.root_node_uri)
    }
    async fn children(&self, node_uri: &str) -> Result<Vec<ChildNode>> {
        self.list_children(node_uri).await
    }
}

/// The parts of a path of names, without empty ones ("/Backup//2014/" is
/// Backup, 2014).
pub fn parts(path: &str) -> Vec<&str> {
    path.split('/').filter(|p| !p.is_empty()).collect()
}

/// The folder or album at `path`, and the path as it was matched. Every
/// part but the last must be a folder. A name shared by two nodes in one
/// folder is an error, since the path can't tell them apart.
pub async fn find<N: NodeLookup>(lookup: &N, path: &str) -> Result<(ChildNode, String)> {
    let parts = parts(path);
    if parts.is_empty() {
        bail!("Give a folder or album as a path of names, e.g. Backup or Backup/2014/07");
    }
    let mut uri = lookup.root().await?;
    let mut shown = String::new();
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        let matches: Vec<ChildNode> = lookup
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
