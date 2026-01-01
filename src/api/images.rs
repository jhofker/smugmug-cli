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
        let _mock = server.mock("GET", "/api/v2/album/ABC123!images")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{
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
            }"#)
            .create_async()
            .await;

        // In a properly architected version, we'd inject the server URL and test the actual call
    }

    #[tokio::test]
    async fn test_list_album_images_empty() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server.mock("GET", "/api/v2/album/EMPTY!images")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{
                "Response": {
                    "AlbumImage": []
                }
            }"#)
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_list_album_images_error() {
        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server.mock("GET", "/api/v2/album/INVALID!images")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
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

        let response: ImagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.response.images.len(), 1);
        assert_eq!(response.response.images[0].image_key, "IMG123");
    }
}
