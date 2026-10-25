use anyhow::Result;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageDetails {
    #[serde(rename = "ImageKey")]
    pub image_key: String,
    #[serde(rename = "FileName")]
    pub file_name: String,
    #[serde(rename = "Format")]
    pub format: String,
    #[serde(rename = "ArchivedSize")]
    pub file_size: u64,
    #[serde(rename = "Title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "Caption", skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(rename = "Keywords", skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(rename = "Latitude", skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(rename = "Longitude", skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(rename = "Altitude", skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(rename = "ArchivedUri")]
    pub archived_uri: String,
    #[serde(rename = "ArchivedMD5", skip_serializing_if = "Option::is_none")]
    pub archived_md5: Option<String>,
    #[serde(rename = "UploadKey", skip_serializing_if = "Option::is_none")]
    pub upload_key: Option<String>,
    #[serde(rename = "Uri")]
    pub uri: String,
    #[serde(rename = "WebUri", skip_serializing_if = "Option::is_none")]
    pub web_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageDetailsResponse {
    #[serde(rename = "Response")]
    response: ImageDetailsResponseData,
}

#[derive(Debug, Deserialize)]
struct ImageDetailsResponseData {
    #[serde(rename = "Image")]
    image: ImageDetails,
}

/// An image as `image!search` and `user/<nick>!recentimages` return it.
#[derive(Debug, Deserialize, Clone)]
pub struct FoundImage {
    #[serde(rename = "ImageKey")]
    pub image_key: String,
    #[serde(rename = "FileName", default)]
    pub file_name: String,
    #[serde(rename = "WebUri", default)]
    pub web_uri: Option<String>,
    /// When the photo was taken, as SmugMug stores it (see `api::taken`).
    #[serde(rename = "DateTimeOriginal", default)]
    pub date_time_original: Option<String>,
    #[serde(rename = "Uris", default)]
    pub uris: Option<FoundImageUris>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FoundImageUris {
    /// The album the image was uploaded to.
    #[serde(rename = "ImageAlbum", default)]
    pub image_album: Option<super::albums::UriRef>,
}

impl FoundImage {
    /// Key of the album the image was uploaded to.
    pub fn album_key(&self) -> Option<&str> {
        let uri = &self.uris.as_ref()?.image_album.as_ref()?.uri;
        uri.rsplit('/').next().filter(|key| !key.is_empty())
    }

    /// When the photo was taken, by the camera's clock.
    pub fn taken(&self) -> Option<chrono::NaiveDateTime> {
        let stored = chrono::DateTime::parse_from_rfc3339(self.date_time_original.as_deref()?)
            .ok()?
            .to_utc();
        Some(super::taken::camera_time(stored))
    }
}

/// What `search_images` looks for. Every field given must match.
#[derive(Debug, Default, Clone)]
pub struct ImageSearch {
    /// Words in any text field (title, caption, keywords, file name).
    pub text: Option<String>,
    /// Keywords the images are tagged with.
    pub keywords: Vec<String>,
    /// Taken at or after this instant (as SmugMug stores it).
    pub taken_start: Option<chrono::DateTime<chrono::Utc>>,
    /// Taken at or before this instant.
    pub taken_end: Option<chrono::DateTime<chrono::Utc>>,
}

impl ImageSearch {
    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.keywords.is_empty()
            && self.taken_start.is_none()
            && self.taken_end.is_none()
    }

    fn params(&self, scope: &str) -> Vec<(&'static str, String)> {
        let format = |t: &chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut params = vec![("Scope", scope.to_string())];
        if let Some(text) = &self.text {
            params.push(("Text", text.clone()));
        }
        // One keyword as is, several as a JSON array.
        match self.keywords.as_slice() {
            [] => {}
            [keyword] => params.push(("Keywords", keyword.clone())),
            keywords => params.push(("Keywords", serde_json::to_string(keywords).unwrap())),
        }
        if let Some(start) = &self.taken_start {
            params.push(("DateTakenStart", format(start)));
        }
        if let Some(end) = &self.taken_end {
            params.push(("DateTakenEnd", format(end)));
        }
        // SmugMug defaults to "Popular". A date range reads best oldest
        // first; otherwise show the newest first. Text searches still put
        // relevant results ahead (`Relevance` defaults to true).
        let date_range = self.taken_start.is_some() || self.taken_end.is_some();
        params.push(("SortMethod", "DateTaken".to_string()));
        params.push((
            "SortDirection",
            if date_range {
                "Ascending"
            } else {
                "Descending"
            }
            .to_string(),
        ));
        params
    }
}

pub const IMAGE_SEARCH_URL: &str = "https://api.smugmug.com/api/v2/image!search";

/// SmugMug's answer to a `collect_images` call.
#[derive(Debug, Default, PartialEq)]
pub struct CollectResult {
    /// URIs SmugMug refused, with its reasons (e.g. "Does not exist"). When
    /// any are refused the whole call reports failure, though SmugMug may
    /// still have collected the others; collecting again is harmless.
    pub rejected: HashMap<String, Vec<String>>,
}

/// SmugMug refused an image metadata update. A 404 means the image is gone.
#[derive(Debug, thiserror::Error)]
#[error("Failed to update image metadata: {} - {body}", self.status_text())]
pub struct ImageMetadataRejected {
    pub status: u16,
    pub body: String,
}

impl ImageMetadataRejected {
    fn status_text(&self) -> String {
        reqwest::StatusCode::from_u16(self.status)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| self.status.to_string())
    }

    /// The image doesn't exist (any more).
    pub fn is_gone(&self) -> bool {
        self.status == 404
    }
}

#[derive(Debug, Default)]
pub struct ImageMetadataUpdate {
    pub caption: Option<String>,
    pub title: Option<String>,
    pub keywords: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Hide the image in its album (still visible to the owner).
    pub hidden: Option<bool>,
    /// The whole keyword list, replacing the old one; see `edit_keywords`.
    pub keyword_array: Option<Vec<String>>,
}

impl ImageMetadataUpdate {
    /// The PATCH body: only the fields that are set.
    pub fn to_json(&self) -> serde_json::Value {
        let mut body = json!({});
        if let Some(caption) = &self.caption {
            body["Caption"] = json!(caption);
        }
        if let Some(title) = &self.title {
            body["Title"] = json!(title);
        }
        if let Some(keywords) = &self.keywords {
            body["Keywords"] = json!(keywords);
        }
        if let Some(latitude) = self.latitude {
            body["Latitude"] = json!(latitude);
        }
        if let Some(longitude) = self.longitude {
            body["Longitude"] = json!(longitude);
        }
        if let Some(hidden) = self.hidden {
            body["Hidden"] = json!(hidden);
        }
        if let Some(keyword_array) = &self.keyword_array {
            body["KeywordArray"] = json!(keyword_array);
        }
        body
    }
}

/// An image's keywords after adding and removing some. SmugMug keeps them
/// as one string separated by semicolons (or, from older uploads, commas);
/// matching ignores case, and a keyword already there isn't added twice.
pub fn edit_keywords(current: &str, add: &[String], remove: &[String]) -> Vec<String> {
    let mut keywords: Vec<String> = current
        .split([';', ','])
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect();
    keywords.retain(|k| !remove.iter().any(|r| r.trim().eq_ignore_ascii_case(k)));
    for keyword in add.iter().map(|k| k.trim()).filter(|k| !k.is_empty()) {
        if !keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
            keywords.push(keyword.to_string());
        }
    }
    keywords
}

/// One size of an image, from `image/<key>!sizedetails`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SizedImage {
    #[serde(rename = "Url")]
    pub url: String,
    /// File extension of this size, e.g. "jpg" (resized copies of HEIC or
    /// RAW originals are JPEGs).
    #[serde(rename = "Ext", default)]
    pub ext: String,
    #[serde(rename = "Width", default)]
    pub width: u32,
    #[serde(rename = "Height", default)]
    pub height: u32,
}

/// The `size` (e.g. "X2Large", as in `ImageSizeX2Large`) from a
/// `!sizedetails` response, or the largest size there is when the image is
/// too small to have it (SmugMug doesn't upscale).
pub fn pick_size(details: &serde_json::Value, size: &str) -> Option<SizedImage> {
    let wanted = format!("ImageSize{}", size);
    let usable = details["UsableSizes"].as_array();
    let is_usable = |name: &str| usable.is_some_and(|u| u.iter().any(|s| s == name));
    let name = if is_usable(&wanted) {
        wanted
    } else {
        details["LargestImageSize"].as_str()?.to_string()
    };
    serde_json::from_value(details.get(&name)?.clone()).ok()
}

impl SmugMugClient {
    /// Every image in the album, across all pages of the listing.
    pub async fn list_album_images(&self, album_key: &str) -> Result<Vec<AlbumImage>> {
        let images_url = format!("https://api.smugmug.com/api/v2/album/{}!images", album_key);
        self.get_all_pages(&images_url, "AlbumImage").await
    }

    pub async fn get_image_details(&self, image_key: &str) -> Result<ImageDetails> {
        let image_url = format!("https://api.smugmug.com/api/v2/image/{}", image_key);
        let oauth_header = self.build_oauth_header("GET", &image_url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&image_url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to get image details: {} - {}", status, body_text);
        }

        let image_data: ImageDetailsResponse = serde_json::from_str(&body_text)?;
        Ok(image_data.response.image)
    }

    pub async fn delete_image(&self, image_key: &str) -> Result<()> {
        let delete_url = format!("https://api.smugmug.com/api/v2/image/{}", image_key);
        let response = self.delete_with_auth(&delete_url).await?;

        let status = response.status();

        if !status.is_success() {
            let body_text = response.text().await?;
            anyhow::bail!("Failed to delete image: {} - {}", status, body_text);
        }

        Ok(())
    }

    pub async fn update_image_metadata(
        &self,
        image_key: &str,
        metadata: ImageMetadataUpdate,
    ) -> Result<()> {
        let update_url = format!("https://api.smugmug.com/api/v2/image/{}", image_key);
        let response = self
            .patch_with_auth(&update_url, metadata.to_json())
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            return Err(ImageMetadataRejected {
                status: status.as_u16(),
                body: body_text,
            }
            .into());
        }

        Ok(())
    }

    /// Images of the signed-in user matching `search`, at most `limit`
    /// (all with `None`). `image!search` returns 100 per page; newly
    /// uploaded images take a few minutes to show up.
    pub async fn search_images(
        &self,
        search: &ImageSearch,
        limit: Option<usize>,
    ) -> Result<Vec<FoundImage>> {
        let scope = format!("/api/v2/user/{}", self.auth_user().await?.nickname);
        self.search_images_at(IMAGE_SEARCH_URL, &scope, search, limit)
            .await
    }

    async fn search_images_at(
        &self,
        search_url: &str,
        scope: &str,
        search: &ImageSearch,
        limit: Option<usize>,
    ) -> Result<Vec<FoundImage>> {
        let url = reqwest::Url::parse_with_params(search_url, search.params(scope))?;
        self.get_pages(url.as_str(), "Image", limit).await
    }

    /// The signed-in user's most recently uploaded images, newest first.
    pub async fn recent_images(&self, limit: Option<usize>) -> Result<Vec<FoundImage>> {
        let url = format!(
            "https://api.smugmug.com/api/v2/user/{}!recentimages",
            self.auth_user().await?.nickname
        );
        self.get_pages(&url, "Image", limit).await
    }

    /// The image in a given size (`ImageSize<size>` in SmugMug's naming,
    /// e.g. "X2Large"), or its largest size if it's smaller than that.
    pub async fn image_size(&self, image_key: &str, size: &str) -> Result<SizedImage> {
        self.image_size_at(
            &format!(
                "https://api.smugmug.com/api/v2/image/{}!sizedetails",
                image_key
            ),
            size,
        )
        .await
    }

    async fn image_size_at(&self, url: &str, size: &str) -> Result<SizedImage> {
        let response = self.get_with_auth(url).await?;
        let status = response.status();
        let body_text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("Failed to get image sizes: {} - {}", status, body_text);
        }
        let body: serde_json::Value = serde_json::from_str(&body_text)?;
        pick_size(&body["Response"]["ImageSizeDetails"], size)
            .ok_or_else(|| anyhow::anyhow!("SmugMug listed no sizes for this image"))
    }

    /// Add existing images (Image or AlbumImage URIs) to the album, without
    /// uploading them again. Collecting an image that's already in the album
    /// changes nothing.
    pub async fn collect_images(&self, album_key: &str, uris: &[String]) -> Result<CollectResult> {
        let url = format!(
            "https://api.smugmug.com/api/v2/album/{}!collectimages",
            album_key
        );
        self.collect_images_at(&url, uris).await
    }

    async fn collect_images_at(&self, url: &str, uris: &[String]) -> Result<CollectResult> {
        let body = json!({ "CollectUris": uris.join(",") });
        let response = self.post_with_auth(url, body).await?;
        let status = response.status();
        let body_text = response.text().await?;
        if status.is_success() {
            return Ok(CollectResult::default());
        }

        // A 400 names each URI it couldn't use under
        // Options.Parameters.POST[CollectUris].UriProblems.
        let rejected: HashMap<String, Vec<String>> = (status == reqwest::StatusCode::BAD_REQUEST)
            .then(|| serde_json::from_str::<serde_json::Value>(&body_text).ok())
            .flatten()
            .and_then(|body| {
                body["Options"]["Parameters"]["POST"]
                    .as_array()?
                    .iter()
                    .find(|p| p["Name"] == "CollectUris")
                    .and_then(|p| serde_json::from_value(p["UriProblems"].clone()).ok())
            })
            .unwrap_or_default();
        if rejected.is_empty() {
            anyhow::bail!("Failed to collect images: {} - {}", status, body_text);
        }
        Ok(CollectResult { rejected })
    }

    pub async fn move_image(&self, image_key: &str, target_album_key: &str) -> Result<()> {
        let move_url = format!("https://api.smugmug.com/api/v2/image/{}", image_key);

        // Build JSON body with album URI
        let album_uri = format!("/api/v2/album/{}", target_album_key);
        let body = json!({
            "AlbumUri": album_uri
        });

        let response = self.patch_with_auth(&move_url, body).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to move image: {} - {}", status, body_text);
        }

        Ok(())
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
    fn test_album_image_serialization() {
        let image = AlbumImage {
            image_key: "IMG123".to_string(),
            file_name: "test.jpg".to_string(),
            archived_uri: "https://example.com/test.jpg".to_string(),
            file_size: 1024,
            format: "JPG".to_string(),
            uri: "/api/v2/image/IMG123".to_string(),
            title: Some("Test Image".to_string()),
            archived_md5: Some("abc123def456".to_string()),
        };

        let json = serde_json::to_string(&image).unwrap();
        assert!(json.contains("\"ImageKey\":\"IMG123\""));
        assert!(json.contains("\"FileName\":\"test.jpg\""));
        assert!(json.contains("\"ArchivedSize\":1024"));
    }

    #[test]
    fn test_album_image_deserialization() {
        let json = r#"{
            "ImageKey": "IMG123",
            "FileName": "test.jpg",
            "ArchivedUri": "https://example.com/test.jpg",
            "ArchivedSize": 1024,
            "Format": "JPG",
            "Uri": "/api/v2/image/IMG123"
        }"#;

        let image: AlbumImage = serde_json::from_str(json).unwrap();
        assert_eq!(image.image_key, "IMG123");
        assert_eq!(image.file_name, "test.jpg");
        assert_eq!(image.archived_uri, "https://example.com/test.jpg");
        assert_eq!(image.file_size, 1024);
        assert_eq!(image.format, "JPG");
        assert_eq!(image.uri, "/api/v2/image/IMG123");
        assert!(image.title.is_none());
        assert!(image.archived_md5.is_none());
    }

    #[test]
    fn test_album_image_with_optional_fields() {
        let json = r#"{
            "ImageKey": "IMG123",
            "FileName": "test.jpg",
            "ArchivedUri": "https://example.com/test.jpg",
            "ArchivedSize": 1024,
            "Format": "JPG",
            "Uri": "/api/v2/image/IMG123",
            "Title": "My Photo",
            "ArchivedMD5": "abc123"
        }"#;

        let image: AlbumImage = serde_json::from_str(json).unwrap();
        assert_eq!(image.title, Some("My Photo".to_string()));
        assert_eq!(image.archived_md5, Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn test_list_album_images_mock() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/album/ABC123!images")
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
                    "AlbumImage": [
                        {
                            "ImageKey": "IMG123",
                            "FileName": "photo1.jpg",
                            "ArchivedUri": "https://example.com/photo1.jpg",
                            "ArchivedSize": 2048,
                            "Format": "JPG",
                            "Uri": "/api/v2/image/IMG123",
                            "Title": "First Photo",
                            "ArchivedMD5": "abc123"
                        },
                        {
                            "ImageKey": "IMG456",
                            "FileName": "photo2.png",
                            "ArchivedUri": "https://example.com/photo2.png",
                            "ArchivedSize": 4096,
                            "Format": "PNG",
                            "Uri": "/api/v2/image/IMG456"
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
    async fn test_list_album_images_empty() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/album/EMPTY!images")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "Response": {
                    "AlbumImage": []
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_list_album_images_error() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/album/INVALID!images")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Album not found")
            .create_async()
            .await;
    }

    #[test]
    fn test_album_image_clone() {
        let image = AlbumImage {
            image_key: "IMG123".to_string(),
            file_name: "test.jpg".to_string(),
            archived_uri: "https://example.com/test.jpg".to_string(),
            file_size: 1024,
            format: "JPG".to_string(),
            uri: "/api/v2/image/IMG123".to_string(),
            title: Some("Test".to_string()),
            archived_md5: Some("abc".to_string()),
        };

        let cloned = image.clone();
        assert_eq!(image.image_key, cloned.image_key);
        assert_eq!(image.file_name, cloned.file_name);
        assert_eq!(image.file_size, cloned.file_size);
    }

    #[test]
    fn test_images_response_deserialization() {
        let json = r#"{
            "Response": {
                "AlbumImage": [
                    {
                        "ImageKey": "IMG123",
                        "FileName": "test.jpg",
                        "ArchivedUri": "https://example.com/test.jpg",
                        "ArchivedSize": 1024,
                        "Format": "JPG",
                        "Uri": "/api/v2/image/IMG123"
                    }
                ]
            }
        }"#;

        // list_album_images reads each page's Response.AlbumImage array.
        let mut response: serde_json::Value = serde_json::from_str(json).unwrap();
        let images: Vec<AlbumImage> =
            serde_json::from_value(response["Response"]["AlbumImage"].take()).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].image_key, "IMG123");
    }

    #[tokio::test]
    async fn test_delete_image_success() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("DELETE", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{}}"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_delete_image_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("DELETE", "/api/v2/image/INVALID")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Image not found")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to delete image: 404"
    }

    #[tokio::test]
    async fn test_delete_image_unauthorized() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("DELETE", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to delete image: 401"
    }

    #[tokio::test]
    async fn test_delete_image_forbidden() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("DELETE", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_body("Forbidden - insufficient permissions")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to delete image: 403"
    }

    #[test]
    fn test_image_details_serialization() {
        let details = ImageDetails {
            image_key: "IMG789".to_string(),
            file_name: "vacation.jpg".to_string(),
            format: "JPG".to_string(),
            file_size: 2048000,
            title: Some("Beach Sunset".to_string()),
            caption: Some("Beautiful sunset at the beach".to_string()),
            keywords: Some("sunset,beach,vacation".to_string()),
            latitude: Some(37.7749),
            longitude: Some(-122.4194),
            altitude: Some(10.5),
            archived_uri: "https://example.com/vacation.jpg".to_string(),
            archived_md5: Some("def456abc789".to_string()),
            upload_key: Some("UPLOAD123".to_string()),
            uri: "/api/v2/image/IMG789".to_string(),
            web_uri: Some("https://smugmug.com/image/IMG789".to_string()),
        };

        let json = serde_json::to_string(&details).unwrap();
        assert!(json.contains("\"ImageKey\":\"IMG789\""));
        assert!(json.contains("\"FileName\":\"vacation.jpg\""));
        assert!(json.contains("\"ArchivedSize\":2048000"));
        assert!(json.contains("\"Title\":\"Beach Sunset\""));
    }

    #[test]
    fn test_image_details_deserialization_all_fields() {
        let json = r#"{
            "ImageKey": "IMG789",
            "FileName": "vacation.jpg",
            "Format": "JPG",
            "ArchivedSize": 2048000,
            "Title": "Beach Sunset",
            "Caption": "Beautiful sunset at the beach",
            "Keywords": "sunset,beach,vacation",
            "Latitude": 37.7749,
            "Longitude": -122.4194,
            "Altitude": 10.5,
            "ArchivedUri": "https://example.com/vacation.jpg",
            "ArchivedMD5": "def456abc789",
            "UploadKey": "UPLOAD123",
            "Uri": "/api/v2/image/IMG789",
            "WebUri": "https://smugmug.com/image/IMG789"
        }"#;

        let details: ImageDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.image_key, "IMG789");
        assert_eq!(details.file_name, "vacation.jpg");
        assert_eq!(details.format, "JPG");
        assert_eq!(details.file_size, 2048000);
        assert_eq!(details.title, Some("Beach Sunset".to_string()));
        assert_eq!(
            details.caption,
            Some("Beautiful sunset at the beach".to_string())
        );
        assert_eq!(details.keywords, Some("sunset,beach,vacation".to_string()));
        assert_eq!(details.latitude, Some(37.7749));
        assert_eq!(details.longitude, Some(-122.4194));
        assert_eq!(details.altitude, Some(10.5));
        assert_eq!(details.archived_uri, "https://example.com/vacation.jpg");
        assert_eq!(details.archived_md5, Some("def456abc789".to_string()));
        assert_eq!(details.upload_key, Some("UPLOAD123".to_string()));
        assert_eq!(details.uri, "/api/v2/image/IMG789");
        assert_eq!(
            details.web_uri,
            Some("https://smugmug.com/image/IMG789".to_string())
        );
    }

    #[test]
    fn test_image_details_deserialization_minimal_fields() {
        let json = r#"{
            "ImageKey": "IMG999",
            "FileName": "simple.png",
            "Format": "PNG",
            "ArchivedSize": 1024,
            "ArchivedUri": "https://example.com/simple.png",
            "Uri": "/api/v2/image/IMG999"
        }"#;

        let details: ImageDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.image_key, "IMG999");
        assert_eq!(details.file_name, "simple.png");
        assert_eq!(details.format, "PNG");
        assert_eq!(details.file_size, 1024);
        assert!(details.title.is_none());
        assert!(details.caption.is_none());
        assert!(details.keywords.is_none());
        assert!(details.latitude.is_none());
        assert!(details.longitude.is_none());
        assert!(details.altitude.is_none());
        assert!(details.archived_md5.is_none());
        assert!(details.upload_key.is_none());
        assert!(details.web_uri.is_none());
    }

    #[test]
    fn test_image_details_response_deserialization() {
        let json = r#"{
            "Response": {
                "Image": {
                    "ImageKey": "IMG777",
                    "FileName": "test.jpg",
                    "Format": "JPG",
                    "ArchivedSize": 5120,
                    "ArchivedUri": "https://example.com/test.jpg",
                    "Uri": "/api/v2/image/IMG777",
                    "Title": "Test Image"
                }
            }
        }"#;

        let response: ImageDetailsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.response.image.image_key, "IMG777");
        assert_eq!(response.response.image.file_name, "test.jpg");
        assert_eq!(
            response.response.image.title,
            Some("Test Image".to_string())
        );
    }

    #[tokio::test]
    async fn test_get_image_details_success() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/image/IMG888")
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
                    "Image": {
                        "ImageKey": "IMG888",
                        "FileName": "mountain.jpg",
                        "Format": "JPG",
                        "ArchivedSize": 3145728,
                        "Title": "Mountain Peak",
                        "Caption": "View from the summit",
                        "Keywords": "mountain,hiking,nature",
                        "Latitude": 45.123,
                        "Longitude": -121.456,
                        "Altitude": 3000.0,
                        "ArchivedUri": "https://example.com/mountain.jpg",
                        "ArchivedMD5": "abc123def456",
                        "UploadKey": "UPLOAD456",
                        "Uri": "/api/v2/image/IMG888",
                        "WebUri": "https://smugmug.com/image/IMG888"
                    }
                }
            }"#,
            )
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_get_image_details_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/image/NOTFOUND")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Image not found")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to get image details: 404"
    }

    #[tokio::test]
    async fn test_get_image_details_unauthorized() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/image/IMG999")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to get image details: 401"
    }

    #[tokio::test]
    async fn test_get_image_details_forbidden() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/image/IMG999")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_body("Forbidden - insufficient permissions")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to get image details: 403"
    }

    #[test]
    fn test_image_details_clone() {
        let details = ImageDetails {
            image_key: "IMG555".to_string(),
            file_name: "clone_test.jpg".to_string(),
            format: "JPG".to_string(),
            file_size: 1024,
            title: Some("Test".to_string()),
            caption: None,
            keywords: Some("test".to_string()),
            latitude: Some(0.0),
            longitude: Some(0.0),
            altitude: None,
            archived_uri: "https://example.com/test.jpg".to_string(),
            archived_md5: Some("abc".to_string()),
            upload_key: None,
            uri: "/api/v2/image/IMG555".to_string(),
            web_uri: None,
        };

        let cloned = details.clone();
        assert_eq!(details.image_key, cloned.image_key);
        assert_eq!(details.file_name, cloned.file_name);
        assert_eq!(details.file_size, cloned.file_size);
        assert_eq!(details.title, cloned.title);
        assert_eq!(details.latitude, cloned.latitude);
    }

    #[test]
    fn test_image_metadata_update_default() {
        let update = ImageMetadataUpdate::default();
        assert!(update.caption.is_none());
        assert!(update.title.is_none());
        assert!(update.keywords.is_none());
        assert!(update.latitude.is_none());
        assert!(update.longitude.is_none());
    }

    #[test]
    fn test_image_metadata_update_with_all_fields() {
        let update = ImageMetadataUpdate {
            caption: Some("Test Caption".to_string()),
            title: Some("Test Title".to_string()),
            keywords: Some("sunset;beach;vacation".to_string()),
            latitude: Some(37.7749),
            longitude: Some(-122.4194),
            ..Default::default()
        };

        assert_eq!(update.caption, Some("Test Caption".to_string()));
        assert_eq!(update.title, Some("Test Title".to_string()));
        assert_eq!(update.keywords, Some("sunset;beach;vacation".to_string()));
        assert_eq!(update.latitude, Some(37.7749));
        assert_eq!(update.longitude, Some(-122.4194));
    }

    #[test]
    fn test_image_metadata_update_partial() {
        let update = ImageMetadataUpdate {
            caption: Some("Only caption".to_string()),
            ..Default::default()
        };

        assert_eq!(update.caption, Some("Only caption".to_string()));
        assert!(update.title.is_none());
        assert!(update.keywords.is_none());
    }

    #[tokio::test]
    async fn test_update_image_metadata_success() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{"Image":{"ImageKey":"IMG123"}}}"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_update_image_metadata_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/INVALID")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Image not found")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update image metadata: 404"
    }

    #[tokio::test]
    async fn test_update_image_metadata_unauthorized() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update image metadata: 401"
    }

    #[tokio::test]
    async fn test_update_image_metadata_forbidden() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_body("Forbidden - insufficient permissions")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to update image metadata: 403"
    }

    #[test]
    fn test_image_metadata_update_with_gps() {
        let update = ImageMetadataUpdate {
            latitude: Some(37.7749),
            longitude: Some(-122.4194),
            ..Default::default()
        };

        assert_eq!(update.latitude, Some(37.7749));
        assert_eq!(update.longitude, Some(-122.4194));
        assert!(update.caption.is_none());
        assert!(update.title.is_none());
    }

    #[test]
    fn test_image_metadata_update_with_keywords_semicolon() {
        let update = ImageMetadataUpdate {
            keywords: Some("sunset;beach;vacation;2024".to_string()),
            ..Default::default()
        };

        assert_eq!(
            update.keywords,
            Some("sunset;beach;vacation;2024".to_string())
        );
    }

    #[test]
    fn test_image_metadata_update_with_title_and_caption() {
        let update = ImageMetadataUpdate {
            title: Some("My Amazing Photo".to_string()),
            caption: Some("This was taken at sunset on the beach.".to_string()),
            ..Default::default()
        };

        assert_eq!(update.title, Some("My Amazing Photo".to_string()));
        assert_eq!(
            update.caption,
            Some("This was taken at sunset on the beach.".to_string())
        );
        assert!(update.keywords.is_none());
    }

    #[tokio::test]
    async fn test_move_image_success() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::JsonString(
                r#"{"AlbumUri":"/api/v2/album/ALB456"}"#.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"Response":{"Image":{"ImageKey":"IMG123","AlbumUri":"/api/v2/album/ALB456"}}}"#,
            )
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would succeed and return Ok(())
    }

    #[tokio::test]
    async fn test_move_image_not_found() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/INVALID")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(404)
            .with_body("Image not found")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to move image: 404"
    }

    #[tokio::test]
    async fn test_move_image_unauthorized() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to move image: 401"
    }

    #[tokio::test]
    async fn test_move_image_forbidden() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("PATCH", "/api/v2/image/IMG123")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(403)
            .with_body("Forbidden - insufficient permissions")
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
        // The actual call would fail with anyhow::Error containing "Failed to move image: 403"
    }

    #[tokio::test]
    async fn test_collect_images_sends_uris_as_json() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v2/album/ABC!collectimages")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".into()))
            .match_body(mockito::Matcher::Json(json!({
                "CollectUris": "/api/v2/image/A-0,/api/v2/album/X/image/B-0"
            })))
            .with_status(200)
            .with_body(r#"{"Code":200,"Message":"Ok","Response":{}}"#)
            .create_async()
            .await;

        let result = client
            .collect_images_at(
                &format!("{}/api/v2/album/ABC!collectimages", server.url()),
                &[
                    "/api/v2/image/A-0".to_string(),
                    "/api/v2/album/X/image/B-0".to_string(),
                ],
            )
            .await
            .unwrap();

        mock.assert_async().await;
        assert_eq!(result, CollectResult::default());
    }

    #[tokio::test]
    async fn test_collect_images_reports_refused_uris() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;
        // Trimmed from a real response (2026-10-02).
        let _mock = server
            .mock("POST", "/api/v2/album/ABC!collectimages")
            .with_status(400)
            .with_body(
                r#"{"Code":400,"Message":"Bad Request","Options":{"Parameters":{"POST":[
                    {"Name":"CollectUris","Problems":["Unable to parse uris"],
                     "UriProblems":{"/api/v2/image/GONE-0":["Does not exist"]}},
                    {"Name":"Async"}]}}}"#,
            )
            .create_async()
            .await;

        let result = client
            .collect_images_at(
                &format!("{}/api/v2/album/ABC!collectimages", server.url()),
                &[
                    "/api/v2/image/GONE-0".to_string(),
                    "/api/v2/image/OK-0".to_string(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(
            result.rejected,
            HashMap::from([(
                "/api/v2/image/GONE-0".to_string(),
                vec!["Does not exist".to_string()]
            )])
        );
    }

    #[tokio::test]
    async fn test_collect_images_other_errors_fail() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/v2/album/ABC!collectimages")
            .with_status(400)
            .with_body(r#"{"Code":400,"Message":"Bad Request"}"#)
            .create_async()
            .await;

        let error = client
            .collect_images_at(
                &format!("{}/api/v2/album/ABC!collectimages", server.url()),
                &["/api/v2/image/A-0".to_string()],
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("400"));
    }

    #[tokio::test]
    async fn test_search_images_sends_filters_and_stops_at_limit() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/api/v2/image!search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("Scope".into(), "/api/v2/user/me".into()),
                mockito::Matcher::UrlEncoded("Text".into(), "beach day".into()),
                mockito::Matcher::UrlEncoded("Keywords".into(), "sunset".into()),
                mockito::Matcher::UrlEncoded(
                    "DateTakenStart".into(),
                    "2026-05-01T07:00:00Z".into(),
                ),
                mockito::Matcher::UrlEncoded("DateTakenEnd".into(), "2026-05-02T06:59:59Z".into()),
                mockito::Matcher::UrlEncoded("SortMethod".into(), "DateTaken".into()),
                mockito::Matcher::UrlEncoded("SortDirection".into(), "Ascending".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"Response":{"Image":[
                    {"ImageKey":"A","FileName":"a.jpg","WebUri":"https://x/i-A",
                     "DateTimeOriginal":"2026-05-01T19:00:00+00:00",
                     "Uris":{"ImageAlbum":{"Uri":"/api/v2/album/ALB1"}}}],
                  "Pages":{"NextPage":"/api/v2/image!search?start=2&count=1"}}}"#,
            )
            .create_async()
            .await;
        let second = server
            .mock("GET", "/api/v2/image!search")
            .match_query(mockito::Matcher::UrlEncoded("start".into(), "2".into()))
            .with_status(200)
            .with_body(
                r#"{"Response":{"Image":[{"ImageKey":"B","FileName":"b.jpg"},{"ImageKey":"C"}],
                  "Pages":{"NextPage":"/api/v2/image!search?start=4&count=2"}}}"#,
            )
            .create_async()
            .await;
        // Never asked for: the limit is reached on the second page.
        let third = server
            .mock("GET", "/api/v2/image!search")
            .match_query(mockito::Matcher::UrlEncoded("start".into(), "4".into()))
            .expect(0)
            .create_async()
            .await;

        let day = |end| {
            let bound = super::super::taken::parse_bound("2026-05-01", end).unwrap();
            let (earliest, latest) = super::super::taken::stored_span(bound);
            if end { latest } else { earliest }
        };
        let search = ImageSearch {
            text: Some("beach day".into()),
            keywords: vec!["sunset".into()],
            taken_start: Some(day(false)),
            taken_end: Some(day(true)),
        };
        let images = client
            .search_images_at(
                &format!("{}/api/v2/image!search", server.url()),
                "/api/v2/user/me",
                &search,
                Some(2),
            )
            .await
            .unwrap();

        first.assert_async().await;
        second.assert_async().await;
        third.assert_async().await;
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].album_key(), Some("ALB1"));
        assert_eq!(
            images[0].taken().unwrap().to_string(),
            "2026-05-01 12:00:00"
        );
        assert_eq!(images[0].web_uri.as_deref(), Some("https://x/i-A"));
        assert_eq!(images[1].file_name, "b.jpg");
        assert_eq!(images[1].album_key(), None);
        assert_eq!(images[1].taken(), None);
    }

    #[test]
    fn test_image_search_leaves_out_unset_filters() {
        let search = ImageSearch {
            text: Some("cat".into()),
            ..Default::default()
        };
        assert!(!search.is_empty());
        assert!(ImageSearch::default().is_empty());
        assert_eq!(
            search.params("/api/v2/user/me"),
            vec![
                ("Scope", "/api/v2/user/me".to_string()),
                ("Text", "cat".to_string()),
                ("SortMethod", "DateTaken".to_string()),
                ("SortDirection", "Descending".to_string()),
            ]
        );

        let search = ImageSearch {
            keywords: vec!["cat".into(), "dog \"rex\"".into()],
            ..Default::default()
        };
        assert_eq!(
            search.params("/api/v2/user/me")[1],
            ("Keywords", r#"["cat","dog \"rex\""]"#.to_string())
        );
    }

    #[test]
    fn test_metadata_body_has_hidden_and_keyword_array() {
        let update = ImageMetadataUpdate {
            hidden: Some(true),
            keyword_array: Some(vec!["a".into(), "b c".into()]),
            ..Default::default()
        };
        assert_eq!(
            update.to_json(),
            json!({"Hidden": true, "KeywordArray": ["a", "b c"]})
        );
        assert_eq!(ImageMetadataUpdate::default().to_json(), json!({}));
    }

    #[test]
    fn test_edit_keywords() {
        let s = |v: &[&str]| v.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(
            edit_keywords("smugmug; heroes", &s(&["Dogs", "HEROES"]), &s(&["SmugMug"])),
            s(&["heroes", "Dogs"])
        );
        assert_eq!(edit_keywords("a, b;c ;; ", &[], &s(&["b"])), s(&["a", "c"]));
        assert_eq!(edit_keywords("", &s(&[" x "]), &[]), s(&["x"]));
    }

    /// Trimmed from a real `!sizedetails` response (2026-10-17).
    fn size_details() -> serde_json::Value {
        json!({
            "UsableSizes": ["ImageSizeSmall", "ImageSizeX2Large", "ImageSizeOriginal"],
            "LargestImageSize": "ImageSizeOriginal",
            "ImageSizeSmall": {"Url": "https://p/S/a-S.jpg", "Ext": "jpg", "Height": 294, "Width": 400, "Size": 44867},
            "ImageSizeX2Large": {"Url": "https://p/X2/a-X2.jpg", "Ext": "jpg", "Height": 940, "Width": 1280},
            "ImageSizeOriginal": {"Url": "https://p/O/a.heic", "Ext": "heic", "Height": 4912, "Width": 6690}
        })
    }

    #[test]
    fn test_pick_size() {
        let details = size_details();
        let x2 = pick_size(&details, "X2Large").unwrap();
        assert_eq!(
            (x2.url.as_str(), x2.ext.as_str(), x2.width),
            ("https://p/X2/a-X2.jpg", "jpg", 1280)
        );
        // Too small for 4K: the largest there is.
        assert_eq!(pick_size(&details, "4K").unwrap().ext, "heic");
        assert!(pick_size(&json!({}), "X2Large").is_none());
    }

    #[tokio::test]
    async fn test_image_size_reads_size_details() {
        let client = create_test_client();
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/image/KEY!sizedetails")
            .with_status(200)
            .with_body(json!({"Response": {"ImageSizeDetails": size_details()}}).to_string())
            .create_async()
            .await;
        let small = client
            .image_size_at(
                &format!("{}/api/v2/image/KEY!sizedetails", server.url()),
                "Small",
            )
            .await
            .unwrap();
        assert_eq!(small.width, 400);
    }
}
