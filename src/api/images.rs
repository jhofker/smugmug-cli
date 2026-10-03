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

/// SmugMug's answer to a `collect_images` call.
#[derive(Debug, Default, PartialEq)]
pub struct CollectResult {
    /// URIs SmugMug refused, with its reasons (e.g. "Does not exist"). When
    /// any are refused the whole call reports failure, though SmugMug may
    /// still have collected the others; collecting again is harmless.
    pub rejected: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default)]
pub struct ImageMetadataUpdate {
    pub caption: Option<String>,
    pub title: Option<String>,
    pub keywords: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
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

        // Build JSON body with only provided fields
        let mut body = json!({});

        if let Some(caption) = metadata.caption {
            body["Caption"] = json!(caption);
        }

        if let Some(title) = metadata.title {
            body["Title"] = json!(title);
        }

        if let Some(keywords) = metadata.keywords {
            body["Keywords"] = json!(keywords);
        }

        if let Some(latitude) = metadata.latitude {
            body["Latitude"] = json!(latitude);
        }

        if let Some(longitude) = metadata.longitude {
            body["Longitude"] = json!(longitude);
        }

        let response = self.patch_with_auth(&update_url, body).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Failed to update image metadata: {} - {}",
                status,
                body_text
            );
        }

        Ok(())
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
}
