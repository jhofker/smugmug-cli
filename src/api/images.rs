use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};

use super::SmugMugClient;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AlbumImage {
    #[serde(rename = "ImageKey")]
    pub image_key: String,
    #[serde(rename = "FileName")]
    pub file_name: String,
    #[serde(rename = "ArchivedUri")]
    pub archived_uri: String,
    #[serde(rename = "ArchivedSize")]
    pub file_size: u64,
    #[serde(rename = "Format")]
    pub format: String,
    #[serde(rename = "Uri")]
    pub uri: String,
    #[serde(rename = "Title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "ArchivedMD5", skip_serializing_if = "Option::is_none")]
    pub archived_md5: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImagesResponse {
    #[serde(rename = "Response")]
    response: ImagesResponseData,
}

#[derive(Debug, Deserialize)]
struct ImagesResponseData {
    #[serde(rename = "AlbumImage")]
    images: Vec<AlbumImage>,
}

impl SmugMugClient {
    pub async fn list_album_images(&self, album_key: &str) -> Result<Vec<AlbumImage>> {
        let images_url = format!(
            "https://api.smugmug.com/api/v2/album/{}!images",
            album_key
        );
        let oauth_header = self.build_oauth_header("GET", &images_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client
            .get(&images_url)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to list album images: {} - {}", status, body_text);
        }

        let images_data: ImagesResponse = serde_json::from_str(&body_text)?;
        Ok(images_data.response.images)
    }
}
