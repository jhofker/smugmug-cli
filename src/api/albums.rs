use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};

use super::SmugMugClient;

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
    #[serde(rename = "NodeID")]
    node_id: String,
    #[serde(rename = "Uri")]
    uri: String,
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

impl SmugMugClient {
    pub async fn list_albums(&self) -> Result<Vec<Album>> {
        // First get the authenticated user info
        let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
        let oauth_header = self.build_oauth_header("GET", auth_user_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client
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

        // Now get the albums for this user using the !albums expansion
        let albums_url = format!("https://api.smugmug.com{}!albums", user_uri);
        let oauth_header = self.build_oauth_header("GET", &albums_url);

        headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client
            .get(&albums_url)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to list albums: {} - {}", status, body_text);
        }

        let albums_data: AlbumsResponse = serde_json::from_str(&body_text)?;
        Ok(albums_data.response.albums)
    }

    pub async fn create_album(&self, name: &str, parent_node_uri: Option<&str>) -> Result<Album> {
        // Get the parent node URI and user nickname (default to user's root node)
        let (parent_uri, user_nickname) = if let Some(uri) = parent_node_uri {
            // If parent URI is provided, we still need to get the nickname
            let auth_user_url = "https://api.smugmug.com/api/v2!authuser";
            let oauth_header = self.build_oauth_header("GET", auth_user_url);

            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));

            let response = self.client
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

            let response = self.client
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
            (user_data.response.user.uris.node.uri, user_data.response.user.nickname)
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
            "Privacy": "Public",
            "SortMethod": "DateAdded",
            "SortDirection": "Ascending"
        });

        let response = self.client
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

        let response = self.client
            .get(&album_url)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get album details: {} - {}", status, body_text);
        }

        let album_response: AlbumResponse = serde_json::from_str(&body_text)?;
        let mut album = album_response.response.album;

        // Construct the web URL
        album.web_uri = Some(format!("https://{}.smugmug.com/{}", user_nickname, album.url_name));

        Ok(album)
    }

    pub async fn get_or_create_album(&self, name: &str) -> Result<Album> {
        // Try to find an existing album with this name
        let albums = self.list_albums().await?;

        for album in albums {
            if album.name == name {
                return Ok(album);
            }
        }

        // Album not found, create it
        self.create_album(name, None).await
    }
}
