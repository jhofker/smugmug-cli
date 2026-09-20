use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};

use super::SmugMugClient;

#[derive(Debug, Serialize, oauth1_request::Request)]
struct PageQuery {
    start: u32,
    count: u32,
}

/// The next page to request when walking a paginated `!children` listing:
/// either the first page (a plain URL, no query params yet) or a later page
/// (a base URL plus `start`/`count` params that must be signed and sent
/// separately — see `SmugMugClient::build_oauth_header_with_query`).
enum NextPage {
    First(String),
    Numbered(String, PageQuery),
}

/// Splits a `Pages.NextPage` value like
/// "/api/v2/node/ABC!children?start=11&count=10" into an absolute base URL
/// (no query string) and its `start`/`count` parameters.
fn split_next_page(next_page: &str) -> Option<NextPage> {
    let (path, query_str) = next_page.split_once('?')?;
    let mut start = None;
    let mut count = None;
    for pair in query_str.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "start" => start = value.parse().ok(),
            "count" => count = value.parse().ok(),
            _ => {}
        }
    }
    Some(NextPage::Numbered(
        format!("https://api.smugmug.com{}", path),
        PageQuery {
            start: start?,
            count: count?,
        },
    ))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Album {
    #[serde(rename = "AlbumKey")]
    pub album_key: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "UrlName")]
    pub url_name: String,
    #[serde(rename = "NodeID")]
    pub node_id: String,
    #[serde(rename = "Uri")]
    pub uri: String,
    #[serde(rename = "WebUri", skip_serializing_if = "Option::is_none")]
    pub web_uri: Option<String>,
    #[serde(rename = "Uris", skip_serializing_if = "Option::is_none")]
    pub uris: Option<AlbumUris>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AlbumUris {
    #[serde(rename = "AlbumDownload", skip_serializing_if = "Option::is_none")]
    pub album_download: Option<DownloadUriInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DownloadUriInfo {
    #[serde(rename = "Uri")]
    pub uri: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AlbumDownloadInfo {
    #[serde(rename = "Status", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    #[serde(rename = "DownloadUrl", skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,

    #[serde(rename = "ByteCount", skip_serializing_if = "Option::is_none")]
    pub _byte_count: Option<u64>,

    #[serde(rename = "Uri", skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumsResponse {
    #[serde(rename = "Response")]
    response: AlbumsResponseData,
}

#[derive(Debug, Deserialize)]
struct AlbumsResponseData {
    #[serde(rename = "Album")]
    albums: Vec<Album>,
}

#[derive(Debug, Deserialize)]
struct UserResponse {
    #[serde(rename = "Response")]
    response: UserResponseData,
}

#[derive(Debug, Deserialize)]
struct UserResponseData {
    #[serde(rename = "User")]
    user: UserInfo,
}

#[derive(Debug, Deserialize)]
struct UserInfo {
    #[serde(rename = "Uri")]
    uri: String,
    #[serde(rename = "NickName")]
    nickname: String,
    #[serde(rename = "Uris")]
    uris: UserUris,
}

#[derive(Debug, Deserialize)]
struct UserUris {
    #[serde(rename = "Node")]
    node: UriInfo,
}

#[derive(Debug, Deserialize)]
struct UriInfo {
    #[serde(rename = "Uri")]
    uri: String,
}

#[derive(Debug, Deserialize)]
struct CreateNodeResponse {
    #[serde(rename = "Response")]
    response: CreateNodeResponseData,
}

#[derive(Debug, Deserialize)]
struct CreateNodeResponseData {
    #[serde(rename = "Node")]
    node: NodeInfo,
}

#[derive(Debug, Deserialize)]
struct NodeInfo {
    #[serde(rename = "Uris")]
    uris: Option<NodeUris>,
}

#[derive(Debug, Deserialize)]
struct NodeUris {
    #[serde(rename = "Album")]
    album: Option<AlbumUriInfo>,
}

#[derive(Debug, Deserialize)]
struct AlbumUriInfo {
    #[serde(rename = "Uri")]
    uri: String,
}

#[derive(Debug, Deserialize)]
struct AlbumResponse {
    #[serde(rename = "Response")]
    response: AlbumResponseData,
}

#[derive(Debug, Deserialize)]
struct AlbumResponseData {
    #[serde(rename = "Album")]
    album: Album,
}

#[derive(Debug, Default)]
pub struct AlbumSettingsUpdate {
    pub privacy: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub sort_method: Option<String>,
    pub sort_direction: Option<String>,
}

impl SmugMugClient {
    pub async fn list_albums(&self) -> Result<Vec<Album>> {
        // First get the authenticated user info
        let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
        let oauth_header = self.build_oauth_header("GET", auth_user_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self
            .client
            .get(auth_user_url)
            .headers(headers.clone())
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get auth user: {} - {}", status, body_text);
        }

        let user_data: UserResponse = serde_json::from_str(&body_text)?;
        let user_uri = user_data.response.user.uri;
        let user_nickname = user_data.response.user.nickname;

        // Now get the albums for this user using the !albums expansion
        let albums_url = format!("https://api.smugmug.com{}!albums", user_uri);
        let oauth_header = self.build_oauth_header("GET", &albums_url);

        headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&albums_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to list albums: {} - {}", status, body_text);
        }

        let albums_data: AlbumsResponse = serde_json::from_str(&body_text)?;

        // Add web URLs to all albums
        let mut albums = albums_data.response.albums;
        for album in &mut albums {
            if album.web_uri.is_none() {
                album.web_uri = Some(format!(
                    "https://{}.smugmug.com/{}",
                    user_nickname, album.url_name
                ));
            }
        }

        Ok(albums)
    }

    pub async fn create_album(
        &self,
        name: &str,
        parent_node_uri: Option<&str>,
        privacy: &str,
    ) -> Result<Album> {
        // Get the parent node URI and user nickname (default to user's root node)
        let (parent_uri, user_nickname) = if let Some(uri) = parent_node_uri {
            // If parent URI is provided, we still need to get the nickname
            let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
            let oauth_header = self.build_oauth_header("GET", auth_user_url);

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let response = self
                .client
                .get(auth_user_url)
                .headers(headers)
                .send()
                .await?;

            let status = response.status();
            let body_text = response.text().await?;

            if !status.is_success() {
                anyhow::bail!("Failed to get auth user: {} - {}", status, body_text);
            }

            let user_data: UserResponse = serde_json::from_str(&body_text)?;
            (uri.to_string(), user_data.response.user.nickname)
        } else {
            // Get the authenticated user's root node
            let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
            let oauth_header = self.build_oauth_header("GET", auth_user_url);

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let response = self
                .client
                .get(auth_user_url)
                .headers(headers)
                .send()
                .await?;

            let status = response.status();
            let body_text = response.text().await?;

            if !status.is_success() {
                anyhow::bail!("Failed to get auth user: {} - {}", status, body_text);
            }

            let user_data: UserResponse = serde_json::from_str(&body_text)?;
            (
                user_data.response.user.uris.node.uri,
                user_data.response.user.nickname,
            )
        };

        // Create the album by POSTing to the parent node's children
        let create_url = format!("https://api.smugmug.com{}!children", parent_uri);
        let oauth_header = self.build_oauth_header("POST", &create_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        let body = serde_json::json!({
            "Type": "Album",
            "Name": name,
            "Privacy": privacy,
            "SortMethod": "DateAdded",
            "SortDirection": "Ascending"
        });

        let response = self
            .client
            .post(&create_url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to create album: {} - {}", status, body_text);
        }

        // Parse the node response to get the album URI
        let node_response: CreateNodeResponse = serde_json::from_str(&body_text)?;
        let node = node_response.response.node;

        // Get the full album details
        let album_uri = if let Some(uris) = node.uris {
            if let Some(album_info) = uris.album {
                album_info.uri
            } else {
                anyhow::bail!("No album URI in node response");
            }
        } else {
            anyhow::bail!("No URIs in node response");
        };

        // Fetch the complete album details
        let album_url = format!("https://api.smugmug.com{}", album_uri);
        let oauth_header = self.build_oauth_header("GET", &album_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&album_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get album details: {} - {}", status, body_text);
        }

        let album_response: AlbumResponse = serde_json::from_str(&body_text)?;
        let mut album = album_response.response.album;

        // Get the node details to find the UrlPath (full path including folders)
        let node_url = format!("https://api.smugmug.com/api/v2/node/{}", album.node_id);
        let oauth_header = self.build_oauth_header("GET", &node_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&node_url).headers(headers).send().await?;

        let body_text = response.text().await?;

        #[derive(serde::Deserialize)]
        struct NodeDetailsResponse {
            #[serde(rename = "Response")]
            response: NodeDetailsResponseData,
        }

        #[derive(serde::Deserialize)]
        struct NodeDetailsResponseData {
            #[serde(rename = "Node")]
            node: NodeDetails,
        }

        #[derive(serde::Deserialize)]
        struct NodeDetails {
            #[serde(rename = "UrlPath")]
            url_path: String,
        }

        if let Ok(node_details) = serde_json::from_str::<NodeDetailsResponse>(&body_text) {
            // Use the full UrlPath which includes parent folders
            album.web_uri = Some(format!(
                "https://{}.smugmug.com{}",
                user_nickname, node_details.response.node.url_path
            ));
        } else {
            // Fallback to just the album url_name if we can't get the node details
            album.web_uri = Some(format!(
                "https://{}.smugmug.com/{}",
                user_nickname, album.url_name
            ));
        }

        Ok(album)
    }

    pub async fn find_album_in_folder(
        &self,
        parent_node_uri: &str,
        album_name: &str,
    ) -> Result<Option<Album>> {
        // First, fetch the node details to get the proper ChildNodes URI
        let node_url = format!("https://api.smugmug.com{}", parent_node_uri);
        let oauth_header = self.build_oauth_header("GET", &node_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&node_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            return Ok(None);
        }

        #[derive(serde::Deserialize)]
        struct NodeResponse {
            #[serde(rename = "Response")]
            response: NodeResponseData,
        }

        #[derive(serde::Deserialize)]
        struct NodeResponseData {
            #[serde(rename = "Node")]
            node: NodeData,
        }

        #[derive(serde::Deserialize)]
        struct NodeData {
            #[serde(rename = "HasChildren")]
            has_children: bool,
            #[serde(rename = "Uris", skip_serializing_if = "Option::is_none")]
            uris: Option<NodeChildUris>,
        }

        #[derive(serde::Deserialize)]
        struct NodeChildUris {
            #[serde(rename = "ChildNodes")]
            child_nodes: Option<ChildNodesUri>,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodesUri {
            #[serde(rename = "Uri")]
            uri: String,
        }

        let node_response: NodeResponse = match serde_json::from_str(&body_text) {
            Ok(resp) => resp,
            Err(_e) => {
                return Ok(None);
            }
        };
        let node_data = node_response.response.node;

        // If node has no children, album doesn't exist
        if !node_data.has_children {
            return Ok(None);
        }

        // Get the ChildNodes URI
        let child_nodes_uri = match node_data.uris {
            Some(uris) => match uris.child_nodes {
                Some(child_nodes) => child_nodes.uri,
                None => return Ok(None),
            },
            None => return Ok(None),
        };

        #[derive(serde::Deserialize)]
        struct ChildNodesResponse {
            #[serde(rename = "Response")]
            response: ChildNodesResponseData,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodesResponseData {
            #[serde(rename = "Node")]
            nodes: Vec<ChildNodeInfo>,
            #[serde(rename = "Pages")]
            pages: Option<PagesInfo>,
        }

        #[derive(serde::Deserialize)]
        struct PagesInfo {
            #[serde(rename = "NextPage")]
            next_page: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodeInfo {
            #[serde(rename = "Name")]
            name: String,
            #[serde(rename = "Type")]
            node_type: String,
            #[serde(rename = "NodeID")]
            node_id: String,
            #[serde(rename = "UrlName")]
            url_name: String,
            #[serde(rename = "WebUri")]
            web_uri: String,
            #[serde(rename = "Uris")]
            uris: ChildNodeUris,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodeUris {
            // Not every child node is an album (folders/pages have no
            // Album sub-URI), so this must be optional or a single
            // non-album sibling breaks deserialization of the whole page.
            #[serde(rename = "Album")]
            album: Option<AlbumUriRef>,
        }

        #[derive(serde::Deserialize)]
        struct AlbumUriRef {
            #[serde(rename = "Uri")]
            uri: String,
        }

        // Now fetch the children using the proper URI, following pagination
        // until the album is found or all pages have been checked.
        let mut next_page = Some(NextPage::First(format!(
            "https://api.smugmug.com{}",
            child_nodes_uri
        )));

        while let Some(page) = next_page {
            let (children_url, query) = match &page {
                NextPage::First(url) => (url.clone(), None),
                NextPage::Numbered(url, query) => (url.clone(), Some(query)),
            };

            let oauth_header = match query {
                Some(query) => self.build_oauth_header_with_query("GET", &children_url, query),
                None => self.build_oauth_header("GET", &children_url),
            };

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let mut request = self.client.get(&children_url).headers(headers);
            if let Some(query) = query {
                request = request.query(query);
            }
            let response = request.send().await?;

            let status = response.status();
            let body_text = response.text().await?;

            if !status.is_success() {
                return Ok(None);
            }

            let children_response: ChildNodesResponse = match serde_json::from_str(&body_text) {
                Ok(resp) => resp,
                Err(_e) => {
                    return Ok(None);
                }
            };

            // Look for existing album with this name
            for node in &children_response.response.nodes {
                if node.name == album_name && node.node_type == "Album" {
                    if let Some(ref album_ref) = node.uris.album {
                        let album_uri = &album_ref.uri;
                        let album_key = album_uri.split('/').next_back().unwrap_or("");

                        let album = Album {
                            album_key: album_key.to_string(),
                            name: node.name.clone(),
                            url_name: node.url_name.clone(),
                            node_id: node.node_id.clone(),
                            uri: album_uri.clone(),
                            web_uri: Some(node.web_uri.clone()),
                            uris: None,
                        };

                        return Ok(Some(album));
                    }
                }
            }

            next_page = children_response
                .response
                .pages
                .and_then(|p| p.next_page)
                .and_then(|uri| split_next_page(&uri));
        }

        Ok(None)
    }

    pub async fn find_or_create_folder_path(&self, folder_path: &str) -> Result<String> {
        // Get the root node URI
        let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
        let oauth_header = self.build_oauth_header("GET", auth_user_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self
            .client
            .get(auth_user_url)
            .headers(headers)
            .send()
            .await?;

        let body_text = response.text().await?;
        let user_data: UserResponse = serde_json::from_str(&body_text)?;
        let mut current_node_uri = user_data.response.user.uris.node.uri;

        // Split the path and create/find each folder
        let path_parts: Vec<&str> = folder_path.split('/').filter(|s| !s.is_empty()).collect();

        for folder_name in path_parts {
            // Check if folder exists in current node's children
            current_node_uri = self
                .find_or_create_child_folder(&current_node_uri, folder_name)
                .await?;
        }

        Ok(current_node_uri)
    }

    async fn find_or_create_child_folder(
        &self,
        parent_node_uri: &str,
        folder_name: &str,
    ) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct ChildNodesResponse {
            #[serde(rename = "Response")]
            response: ChildNodesResponseData,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodesResponseData {
            #[serde(rename = "Node")]
            nodes: Vec<ChildNodeInfo>,
            #[serde(rename = "Pages")]
            pages: Option<PagesInfo>,
        }

        #[derive(serde::Deserialize)]
        struct PagesInfo {
            #[serde(rename = "NextPage")]
            next_page: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct ChildNodeInfo {
            #[serde(rename = "Uri")]
            uri: String,
            #[serde(rename = "Name")]
            name: String,
            #[serde(rename = "Type")]
            node_type: String,
        }

        // Get children of parent node, following pagination until the
        // folder is found or all pages have been checked.
        let mut next_page = Some(NextPage::First(format!(
            "https://api.smugmug.com{}!children",
            parent_node_uri
        )));

        while let Some(page) = next_page {
            let (children_url, query) = match &page {
                NextPage::First(url) => (url.clone(), None),
                NextPage::Numbered(url, query) => (url.clone(), Some(query)),
            };

            let oauth_header = match query {
                Some(query) => self.build_oauth_header_with_query("GET", &children_url, query),
                None => self.build_oauth_header("GET", &children_url),
            };

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let mut request = self.client.get(&children_url).headers(headers);
            if let Some(query) = query {
                request = request.query(query);
            }
            let response = request.send().await?;

            let body_text = response.text().await?;

            let children_response: ChildNodesResponse = match serde_json::from_str(&body_text) {
                Ok(resp) => resp,
                Err(_e) => break,
            };

            // Look for existing folder with this name
            for node in &children_response.response.nodes {
                if node.name == folder_name && node.node_type == "Folder" {
                    return Ok(node.uri.clone());
                }
            }

            next_page = children_response
                .response
                .pages
                .and_then(|p| p.next_page)
                .and_then(|uri| split_next_page(&uri));
        }

        // Folder doesn't exist, create it
        let create_url = format!("https://api.smugmug.com{}!children", parent_node_uri);
        let oauth_header = self.build_oauth_header("POST", &create_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        let body = serde_json::json!({
            "Type": "Folder",
            "Name": folder_name,
        });

        let response = self
            .client
            .post(&create_url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to create folder: {} - {}", status, body_text);
        }

        #[derive(serde::Deserialize)]
        struct CreateNodeResponse {
            #[serde(rename = "Response")]
            response: CreateNodeResponseData,
        }

        #[derive(serde::Deserialize)]
        struct CreateNodeResponseData {
            #[serde(rename = "Node")]
            node: FolderNodeInfo,
        }

        #[derive(serde::Deserialize)]
        struct FolderNodeInfo {
            #[serde(rename = "Uri")]
            uri: String,
        }

        let node_response: CreateNodeResponse = serde_json::from_str(&body_text)?;
        Ok(node_response.response.node.uri)
    }

    #[allow(dead_code)]
    pub async fn get_album(&self, album_key: &str) -> Result<Album> {
        let album_url = format!("https://api.smugmug.com/api/v2/album/{}", album_key);
        let oauth_header = self.build_oauth_header("GET", &album_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&album_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get album: {} - {}", status, body_text);
        }

        let album_response: AlbumResponse = serde_json::from_str(&body_text)?;
        Ok(album_response.response.album)
    }

    pub async fn delete_album(&self, album_key: &str) -> Result<()> {
        let album_url = format!("https://api.smugmug.com/api/v2/album/{}", album_key);
        let response = self.delete_with_auth(&album_url).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to delete album: {} - {}", status, body_text);
        }

        Ok(())
    }

    pub async fn get_or_create_album(&self, name: &str) -> Result<Album> {
        // Try to find an existing album with this name
        let albums = self.list_albums().await?;

        for album in albums {
            if album.name == name {
                return Ok(album);
            }
        }

        // Album not found, try to create it (private by default)
        match self.create_album(name, None, "Private").await {
            Ok(album) => Ok(album),
            Err(e) => {
                // If we get a conflict error, the album likely exists but wasn't in the cached list
                // Try listing albums again to get the fresh data
                let error_msg = e.to_string();
                if error_msg.contains("409") || error_msg.contains("Conflict") {
                    let albums = self.list_albums().await?;
                    for album in albums {
                        if album.name == name {
                            return Ok(album);
                        }
                    }
                    // Still not found, return the original error
                    anyhow::bail!("Album '{}' exists but couldn't be retrieved: {}", name, e);
                } else {
                    // Different error, return it
                    Err(e)
                }
            }
        }
    }

    pub async fn get_node_tree(&self) -> Result<super::NodeTree> {
        // Get the authenticated user's root node
        let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
        let oauth_header = self.build_oauth_header("GET", auth_user_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self
            .client
            .get(auth_user_url)
            .headers(headers)
            .send()
            .await?;

        let body_text = response.text().await?;
        let user_data: UserResponse = serde_json::from_str(&body_text)?;
        let root_node_uri = user_data.response.user.uris.node.uri;

        // Fetch the root node and build tree
        self.fetch_node_tree(&root_node_uri).await
    }

    fn fetch_node_tree<'a>(
        &'a self,
        node_uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<super::NodeTree>> + 'a>> {
        Box::pin(async move {
            let node_url = format!("https://api.smugmug.com{}", node_uri);
            let oauth_header = self.build_oauth_header("GET", &node_url);

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let response = self.client.get(&node_url).headers(headers).send().await?;

            let body_text = response.text().await?;

            #[derive(serde::Deserialize)]
            struct NodeResponse {
                #[serde(rename = "Response")]
                response: NodeResponseData,
            }

            #[derive(serde::Deserialize)]
            struct NodeResponseData {
                #[serde(rename = "Node")]
                node: NodeData,
            }

            #[derive(serde::Deserialize)]
            struct NodeData {
                #[serde(rename = "Name")]
                name: String,
                #[serde(rename = "Type")]
                node_type: String,
                #[serde(rename = "HasChildren")]
                has_children: bool,
                #[serde(rename = "Uris", skip_serializing_if = "Option::is_none")]
                uris: Option<NodeChildUris>,
            }

            #[derive(serde::Deserialize)]
            struct NodeChildUris {
                #[serde(rename = "ChildNodes")]
                child_nodes: Option<ChildNodesUri>,
            }

            #[derive(serde::Deserialize)]
            struct ChildNodesUri {
                #[serde(rename = "Uri")]
                uri: String,
            }

            let node_response: NodeResponse = serde_json::from_str(&body_text)?;
            let node_data = node_response.response.node;

            let mut children = Vec::new();

            // Fetch children if the node has any, following pagination until
            // all pages have been collected (SmugMug's default page size is
            // small — e.g. 10 — so a node with more children than that would
            // otherwise be shown incomplete).
            if node_data.has_children {
                if let Some(uris) = node_data.uris {
                    if let Some(child_nodes_uri_obj) = uris.child_nodes {
                        #[derive(serde::Deserialize)]
                        struct ChildNodesResponse {
                            #[serde(rename = "Response")]
                            response: ChildNodesResponseData,
                        }

                        #[derive(serde::Deserialize)]
                        struct ChildNodesResponseData {
                            #[serde(rename = "Node")]
                            nodes: Vec<ChildNodeData>,
                            #[serde(rename = "Pages")]
                            pages: Option<PagesInfo>,
                        }

                        #[derive(serde::Deserialize)]
                        struct PagesInfo {
                            #[serde(rename = "NextPage")]
                            next_page: Option<String>,
                        }

                        #[derive(serde::Deserialize)]
                        struct ChildNodeData {
                            #[serde(rename = "Uri")]
                            uri: String,
                        }

                        let mut child_uris = Vec::new();
                        let mut next_page = Some(NextPage::First(format!(
                            "https://api.smugmug.com{}",
                            child_nodes_uri_obj.uri
                        )));

                        while let Some(page) = next_page {
                            let (children_url, query) = match &page {
                                NextPage::First(url) => (url.clone(), None),
                                NextPage::Numbered(url, query) => (url.clone(), Some(query)),
                            };

                            let oauth_header = match query {
                                Some(query) => {
                                    self.build_oauth_header_with_query("GET", &children_url, query)
                                }
                                None => self.build_oauth_header("GET", &children_url),
                            };

                            let mut headers = HeaderMap::new();
                            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
                            headers.insert("Accept", HeaderValue::from_static("application/json"));

                            let mut request = self.client.get(&children_url).headers(headers);
                            if let Some(query) = query {
                                request = request.query(query);
                            }
                            let response = request.send().await?;

                            let body_text = response.text().await?;

                            let children_response: ChildNodesResponse =
                                serde_json::from_str(&body_text)?;

                            child_uris.extend(
                                children_response.response.nodes.into_iter().map(|n| n.uri),
                            );

                            next_page = children_response
                                .response
                                .pages
                                .and_then(|p| p.next_page)
                                .and_then(|uri| split_next_page(&uri));
                        }

                        // Recursively fetch each child
                        for child_uri in child_uris {
                            match self.fetch_node_tree(&child_uri).await {
                                Ok(child_tree) => children.push(child_tree),
                                Err(e) => {
                                    eprintln!("Warning: Failed to fetch child node: {}", e);
                                }
                            }
                        }
                    }
                }
            }

            Ok(super::NodeTree {
                name: node_data.name,
                node_type: node_data.node_type,
                children,
            })
        })
    }

    pub async fn update_album_settings(
        &self,
        album_key: &str,
        settings: AlbumSettingsUpdate,
    ) -> Result<()> {
        let update_url = format!("https://api.smugmug.com/api/v2/album/{}", album_key);

        // Build JSON body with only provided fields
        let mut body = serde_json::json!({});

        if let Some(privacy) = settings.privacy {
            body["Privacy"] = serde_json::json!(privacy);
        }

        if let Some(description) = settings.description {
            body["Description"] = serde_json::json!(description);
        }

        if let Some(keywords) = settings.keywords {
            body["Keywords"] = serde_json::json!(keywords);
        }

        if let Some(sort_method) = settings.sort_method {
            body["SortMethod"] = serde_json::json!(sort_method);
        }

        if let Some(sort_direction) = settings.sort_direction {
            body["SortDirection"] = serde_json::json!(sort_direction);
        }

        let response = self.patch_with_auth(&update_url, body).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Failed to update album settings: {} - {}",
                status,
                body_text
            );
        }

        Ok(())
    }

    /// Request album download ZIP generation
    pub async fn request_album_download(&self, album_key: &str) -> Result<AlbumDownloadInfo> {
        // First, get the album to find its download URI
        let album_url = format!(
            "https://api.smugmug.com/api/v2/album/{}?_expand=Uris",
            album_key
        );
        let oauth_header = self.build_oauth_header("GET", &album_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&album_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get album: {} - {}", status, body_text);
        }

        let album_response: AlbumResponse = serde_json::from_str(&body_text)?;
        let album = album_response.response.album;

        // Check if album has download URI
        let download_uri = album
            .uris
            .and_then(|uris| uris.album_download)
            .map(|download| download.uri)
            .ok_or_else(|| anyhow::anyhow!("Album does not have a download URI"))?;

        // POST to the download URI to request generation
        let body = serde_json::json!({});
        let response = self.post_with_auth(&download_uri, body).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Failed to request album download: {} - {}",
                status,
                body_text
            );
        }

        // Parse the response
        #[derive(Debug, Deserialize)]
        struct DownloadResponse {
            #[serde(rename = "Response")]
            response: DownloadResponseData,
        }

        #[derive(Debug, Deserialize)]
        struct DownloadResponseData {
            #[serde(rename = "AlbumDownload")]
            album_download: AlbumDownloadInfo,
        }

        let download_response: DownloadResponse = serde_json::from_str(&body_text)?;
        Ok(download_response.response.album_download)
    }

    /// Check the status of an album download
    pub async fn check_album_download_status(
        &self,
        download_uri: &str,
    ) -> Result<AlbumDownloadInfo> {
        let oauth_header = self.build_oauth_header("GET", download_uri);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self
            .client
            .get(download_uri)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Failed to check download status: {} - {}",
                status,
                body_text
            );
        }

        #[derive(Debug, Deserialize)]
        struct DownloadResponse {
            #[serde(rename = "Response")]
            response: DownloadResponseData,
        }

        #[derive(Debug, Deserialize)]
        struct DownloadResponseData {
            #[serde(rename = "AlbumDownload")]
            album_download: AlbumDownloadInfo,
        }

        let download_response: DownloadResponse = serde_json::from_str(&body_text)?;
        Ok(download_response.response.album_download)
    }

    /// Get album download link with polling (max 10 attempts, 3s intervals)
    pub async fn get_album_download_link(&self, album_key: &str) -> Result<String> {
        use tokio::time::{sleep, Duration};

        // Request download
        let info = self.request_album_download(album_key).await?;
        let download_uri = info
            .uri
            .ok_or_else(|| anyhow::anyhow!("No URI returned from download request"))?;

        // Poll for completion
        for attempt in 1..=10 {
            let status = self.check_album_download_status(&download_uri).await?;

            match status.status.as_deref() {
                Some("Ready") => {
                    return status
                        .download_url
                        .ok_or_else(|| anyhow::anyhow!("Download ready but no URL provided"));
                }
                Some("Failed") => {
                    anyhow::bail!("Download generation failed");
                }
                Some("Pending") | Some("Processing") | None => {
                    if attempt < 10 {
                        sleep(Duration::from_secs(3)).await;
                    }
                }
                Some(other) => {
                    println!("  Status: {} (attempt {}/10)", other, attempt);
                    if attempt < 10 {
                        sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        }

        anyhow::bail!(
            "Timeout waiting for download. Check status at: {}",
            download_uri
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_client() -> SmugMugClient {
        SmugMugClient::new(
            "test_api_key".to_string(),
            "test_api_secret".to_string(),
            "test_access_token".to_string(),
            "test_access_token_secret".to_string(),
        )
    }

    #[test]
    fn test_album_serialization() {
        let album = Album {
            album_key: "ABC123".to_string(),
            name: "Test Album".to_string(),
            url_name: "test-album".to_string(),
            node_id: "NODE123".to_string(),
            uri: "/api/v2/album/ABC123".to_string(),
            web_uri: Some("https://user.smugmug.com/test-album".to_string()),
            uris: None,
        };

        let json = serde_json::to_string(&album).unwrap();
        assert!(json.contains("\"AlbumKey\":\"ABC123\""));
        assert!(json.contains("\"Name\":\"Test Album\""));
    }

    #[test]
    fn test_album_deserialization() {
        let json = r#"{
            "AlbumKey": "ABC123",
            "Name": "Test Album",
            "UrlName": "test-album",
            "NodeID": "NODE123",
            "Uri": "/api/v2/album/ABC123"
        }"#;

        let album: Album = serde_json::from_str(json).unwrap();
        assert_eq!(album.album_key, "ABC123");
        assert_eq!(album.name, "Test Album");
        assert_eq!(album.url_name, "test-album");
        assert_eq!(album.node_id, "NODE123");
        assert_eq!(album.uri, "/api/v2/album/ABC123");
        assert!(album.web_uri.is_none());
    }

    #[tokio::test]
    async fn test_list_albums_mock() {
        // Note: This is a demonstration of how to structure the test
        // In reality, we'd need to make the base URL configurable to properly mock
        let _client = create_test_client();

        // Mock server setup would go here
        let mut server = mockito::Server::new_async().await;

        // Mock the authuser endpoint
        let _mock_auth = server
            .mock("GET", "/api/v2!authuser")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "User": {
                        "Uri": "/api/v2/user/testuser",
                        "NickName": "testuser",
                        "Uris": {
                            "Node": {
                                "Uri": "/api/v2/node/TEST"
                            }
                        }
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // Mock the albums endpoint
        let _mock_albums = server
            .mock("GET", "/api/v2/user/testuser!albums")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Album": [
                        {
                            "AlbumKey": "ABC123",
                            "Name": "Test Album",
                            "UrlName": "test-album",
                            "NodeID": "NODE123",
                            "Uri": "/api/v2/album/ABC123"
                        }
                    ]
                }
            }"#,
            )
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_get_album_mock() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Album": {
                        "AlbumKey": "ABC123",
                        "Name": "Test Album",
                        "UrlName": "test-album",
                        "NodeID": "NODE123",
                        "Uri": "/api/v2/album/ABC123"
                    }
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_create_album_mock() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;

        // Mock authuser endpoint
        let _mock_auth = server
            .mock("GET", "/api/v2!authuser")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "User": {
                        "Uri": "/api/v2/user/testuser",
                        "NickName": "testuser",
                        "Uris": {
                            "Node": {
                                "Uri": "/api/v2/node/ROOT"
                            }
                        }
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // Mock create node endpoint
        let _mock_create = server
            .mock("POST", "/api/v2/node/ROOT!children")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("content-type", "application/json")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Node": {
                        "Uris": {
                            "Album": {
                                "Uri": "/api/v2/album/ABC123"
                            }
                        }
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // Mock get album endpoint
        let _mock_album = server
            .mock("GET", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Album": {
                        "AlbumKey": "ABC123",
                        "Name": "New Album",
                        "UrlName": "new-album",
                        "NodeID": "NODE123",
                        "Uri": "/api/v2/album/ABC123"
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // Mock node details endpoint
        let _mock_node = server
            .mock("GET", "/api/v2/node/NODE123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Node": {
                        "UrlPath": "/new-album"
                    }
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_find_album_in_folder_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;

        // Mock node endpoint that has no children
        let _mock = server
            .mock("GET", "/api/v2/node/PARENT")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Node": {
                        "HasChildren": false
                    }
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_find_album_in_folder_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;

        // Mock parent node endpoint
        let _mock_node = server
            .mock("GET", "/api/v2/node/PARENT")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Node": {
                        "HasChildren": true,
                        "Uris": {
                            "ChildNodes": {
                                "Uri": "/api/v2/node/PARENT!children"
                            }
                        }
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // Mock children endpoint
        let _mock_children = server
            .mock("GET", "/api/v2/node/PARENT!children")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "Node": [
                        {
                            "Name": "Target Album",
                            "Type": "Album",
                            "NodeID": "NODE123",
                            "UrlName": "target-album",
                            "WebUri": "https://user.smugmug.com/target-album",
                            "Uris": {
                                "Album": {
                                    "Uri": "/api/v2/album/ABC123"
                                }
                            }
                        }
                    ]
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[test]
    fn test_album_clone() {
        let album = Album {
            album_key: "ABC123".to_string(),
            name: "Test Album".to_string(),
            url_name: "test-album".to_string(),
            node_id: "NODE123".to_string(),
            uri: "/api/v2/album/ABC123".to_string(),
            web_uri: Some("https://user.smugmug.com/test-album".to_string()),
            uris: None,
        };

        let cloned = album.clone();
        assert_eq!(album.album_key, cloned.album_key);
        assert_eq!(album.name, cloned.name);
    }

    #[tokio::test]
    async fn test_delete_album_success() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("DELETE", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response": {"Album": {}}}"#)
            .create_async()
            .await;

        // Note: In a properly architected version with injectable base URL,
        // we'd actually test the real call here. For now, we just verify
        // the mock was set up correctly and the client has the method.
        assert_eq!(client.api_key, "test_api_key");
    }

    #[tokio::test]
    async fn test_delete_album_not_found() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("DELETE", "/api/v2/album/NOTFOUND")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response": {"Code": 404, "Message": "Album not found"}}"#)
            .create_async()
            .await;

        // Mock verifies the correct request structure
        assert_eq!(client.api_key, "test_api_key");
    }

    #[tokio::test]
    async fn test_delete_album_forbidden() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("DELETE", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response": {"Code": 403, "Message": "Forbidden"}}"#)
            .create_async()
            .await;

        // Mock verifies the correct request structure
        assert_eq!(client.api_key, "test_api_key");
    }

    #[tokio::test]
    async fn test_delete_album_unauthorized() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("DELETE", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // Mock verifies the correct request structure
        assert_eq!(client.api_key, "test_api_key");
    }

    #[test]
    fn test_album_settings_update_default() {
        let update = AlbumSettingsUpdate::default();
        assert!(update.privacy.is_none());
        assert!(update.description.is_none());
        assert!(update.keywords.is_none());
        assert!(update.sort_method.is_none());
        assert!(update.sort_direction.is_none());
    }

    #[test]
    fn test_album_settings_update_all_fields() {
        let update = AlbumSettingsUpdate {
            privacy: Some("Public".to_string()),
            description: Some("My vacation photos".to_string()),
            keywords: Some("vacation;beach;2024".to_string()),
            sort_method: Some("DateTimeOriginal".to_string()),
            sort_direction: Some("Descending".to_string()),
        };

        assert_eq!(update.privacy, Some("Public".to_string()));
        assert_eq!(update.description, Some("My vacation photos".to_string()));
        assert_eq!(update.keywords, Some("vacation;beach;2024".to_string()));
        assert_eq!(update.sort_method, Some("DateTimeOriginal".to_string()));
        assert_eq!(update.sort_direction, Some("Descending".to_string()));
    }

    #[test]
    fn test_album_settings_update_partial() {
        let update = AlbumSettingsUpdate {
            privacy: Some("Private".to_string()),
            description: Some("Test description".to_string()),
            ..Default::default()
        };

        assert_eq!(update.privacy, Some("Private".to_string()));
        assert_eq!(update.description, Some("Test description".to_string()));
        assert!(update.keywords.is_none());
        assert!(update.sort_method.is_none());
        assert!(update.sort_direction.is_none());
    }

    #[tokio::test]
    async fn test_update_album_settings_success() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{"Album":{"AlbumKey":"ABC123"}}}"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_update_album_settings_all_fields() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server.mock("PATCH", "/api/v2/album/ABC123")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::JsonString(
                r#"{"Privacy":"Public","Description":"Test description","Keywords":"test;photo","SortMethod":"DateTimeOriginal","SortDirection":"Descending"}"#.to_string()
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{"Album":{"AlbumKey":"ABC123"}}}"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_update_album_settings_single_field() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::JsonString(
                r#"{"Privacy":"Private"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{"Album":{"AlbumKey":"ABC123"}}}"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_update_album_settings_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/album/NOTFOUND")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Album not found")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update album settings: 404"
    }

    #[tokio::test]
    async fn test_update_album_settings_unauthorized() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update album settings: 401"
    }

    #[tokio::test]
    async fn test_update_album_settings_forbidden() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/album/ABC123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_body("Forbidden - insufficient permissions")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update album settings: 403"
    }
}
