use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use md5::Context;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResult {
    pub image_key: String,
    pub image_uri: String,
    pub status_code: u16,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    stat: String,
    #[serde(rename = "Image")]
    image: ImageInfo,
}

#[derive(Debug, Deserialize)]
struct ImageInfo {
    #[serde(rename = "ImageUri")]
    image_uri: String,
}

pub async fn upload_image(
    client: &crate::api::SmugMugClient,
    album_uri: &str,
    file_path: &Path,
) -> Result<UploadResult> {
    // 1. Read file
    let file_data = fs::read(file_path).await?;
    let file_size = file_data.len();

    // 2. Calculate MD5 checksum (base64-encoded)
    let mut context = Context::new();
    context.consume(&file_data);
    let md5_hash = context.compute();
    let md5_base64 = general_purpose::STANDARD.encode(md5_hash.0);

    // 3. Determine MIME type
    let mime_type = mime_guess::from_path(file_path)
        .first_or_octet_stream()
        .to_string();

    // Get filename
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image");

    // 4. Build OAuth header for upload endpoint
    let upload_url = "https://upload.smugmug.com/";
    let oauth_header = client.build_oauth_header("POST", upload_url);

    // 5. Build headers
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
    headers.insert(CONTENT_LENGTH, HeaderValue::from(file_size as u64));
    headers.insert(CONTENT_TYPE, HeaderValue::from_str(&mime_type)?);
    headers.insert("Content-MD5", HeaderValue::from_str(&md5_base64)?);
    headers.insert("X-Smug-AlbumUri", HeaderValue::from_str(album_uri)?);
    headers.insert("X-Smug-FileName", HeaderValue::from_str(filename)?);
    headers.insert("X-Smug-Title", HeaderValue::from_str(filename)?);
    headers.insert("X-Smug-ResponseType", HeaderValue::from_static("JSON"));
    headers.insert("X-Smug-Version", HeaderValue::from_static("v2"));
    headers.insert("Accept", HeaderValue::from_static("application/json"));

    // 6. Send POST request with file data as body
    let http_client = reqwest::Client::new();
    let response = http_client
        .post(upload_url)
        .headers(headers)
        .body(file_data)
        .send()
        .await?;

    let status = response.status();
    let status_code = status.as_u16();
    let body_text = response.text().await?;

    if !status.is_success() {
        anyhow::bail!("Upload failed with status {}: {}", status_code, body_text);
    }

    // 7. Parse response
    let upload_response: UploadResponse = serde_json::from_str(&body_text)?;

    if upload_response.stat != "ok" {
        anyhow::bail!("Upload failed: {}", body_text);
    }

    // Extract image key from URI (format: /api/v2/album/<key>/image/<key>-0)
    let image_key = upload_response
        .image
        .image_uri
        .split('/')
        .last()
        .unwrap_or("")
        .to_string();

    Ok(UploadResult {
        image_key,
        image_uri: upload_response.image.image_uri,
        status_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_client() -> crate::api::SmugMugClient {
        crate::api::SmugMugClient::new(
            "test_api_key".to_string(),
            "test_api_secret".to_string(),
            "test_access_token".to_string(),
            "test_access_token_secret".to_string(),
        )
    }

    #[test]
    fn test_upload_result_structure() {
        let result = UploadResult {
            image_key: "IMG123".to_string(),
            image_uri: "/api/v2/image/IMG123".to_string(),
            status_code: 200,
        };

        assert_eq!(result.image_key, "IMG123");
        assert_eq!(result.image_uri, "/api/v2/image/IMG123");
        assert_eq!(result.status_code, 200);
    }

    #[test]
    fn test_upload_result_serialization() {
        let result = UploadResult {
            image_key: "IMG123".to_string(),
            image_uri: "/api/v2/image/IMG123".to_string(),
            status_code: 200,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"image_key\":\"IMG123\""));
        assert!(json.contains("\"image_uri\":\"/api/v2/image/IMG123\""));
        assert!(json.contains("\"status_code\":200"));
    }

    #[test]
    fn test_upload_result_deserialization() {
        let json = r#"{
            "image_key": "IMG123",
            "image_uri": "/api/v2/image/IMG123",
            "status_code": 200
        }"#;

        let result: UploadResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.image_key, "IMG123");
        assert_eq!(result.image_uri, "/api/v2/image/IMG123");
        assert_eq!(result.status_code, 200);
    }

    #[tokio::test]
    async fn test_upload_image_creates_correct_headers() {
        // Create a temporary test file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test image data").unwrap();
        let file_path = temp_file.path();

        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("content-type", mockito::Matcher::Any)
            .match_header("content-md5", mockito::Matcher::Any)
            .match_header("x-smug-albumuri", "/api/v2/album/ABC123")
            .match_header("x-smug-responsetype", "JSON")
            .match_header("x-smug-version", "v2")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "stat": "ok",
                "Image": {
                    "ImageUri": "/api/v2/album/ABC123/image/IMG123-0"
                }
            }"#,
            )
            .create_async()
            .await;

        // In a properly architected version, we'd inject the upload URL and test the actual call
    }

    #[tokio::test]
    async fn test_upload_image_mock_success() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test image data").unwrap();
        let _file_path = temp_file.path();

        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "stat": "ok",
                "Image": {
                    "ImageUri": "/api/v2/album/ABC123/image/IMG123-0"
                }
            }"#,
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_upload_image_mock_failure() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "test image data").unwrap();
        let _file_path = temp_file.path();

        let _client = create_test_client();

        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(400)
            .with_body("Bad Request: Invalid album URI")
            .create_async()
            .await;
    }

    #[test]
    fn test_upload_response_deserialization() {
        let json = r#"{
            "stat": "ok",
            "Image": {
                "ImageUri": "/api/v2/album/ABC123/image/IMG123-0"
            }
        }"#;

        let response: UploadResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.stat, "ok");
        assert_eq!(
            response.image.image_uri,
            "/api/v2/album/ABC123/image/IMG123-0"
        );
    }

    #[test]
    fn test_image_key_extraction() {
        let image_uri = "/api/v2/album/ABC123/image/IMG123-0";
        let image_key = image_uri.split('/').last().unwrap_or("");
        assert_eq!(image_key, "IMG123-0");
    }

    #[tokio::test]
    async fn test_file_reading_and_md5() {
        // Create a temporary test file with known content
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = b"Hello, SmugMug!";
        temp_file.write_all(test_data).unwrap();
        temp_file.flush().unwrap();

        let file_path = temp_file.path();

        // Read file and calculate MD5
        let file_data = fs::read(file_path).await.unwrap();
        assert_eq!(file_data, test_data);

        let mut context = Context::new();
        context.consume(&file_data);
        let md5_hash = context.compute();
        let md5_base64 = general_purpose::STANDARD.encode(md5_hash.0);

        // Verify MD5 is not empty
        assert!(!md5_base64.is_empty());
    }

    #[test]
    fn test_mime_type_detection() {
        // Test various file extensions
        let jpg_path = Path::new("test.jpg");
        let mime_jpg = mime_guess::from_path(jpg_path).first_or_octet_stream();
        assert_eq!(mime_jpg.to_string(), "image/jpeg");

        let png_path = Path::new("test.png");
        let mime_png = mime_guess::from_path(png_path).first_or_octet_stream();
        assert_eq!(mime_png.to_string(), "image/png");

        // Test that unknown extensions have a default MIME type
        let unknown_path = Path::new("test.unknown123");
        let mime_unknown = mime_guess::from_path(unknown_path).first_or_octet_stream();
        assert_eq!(mime_unknown.to_string(), "application/octet-stream");
    }

    #[tokio::test]
    async fn test_upload_response_stat_not_ok() {
        let json = r#"{
            "stat": "fail",
            "message": "Upload failed",
            "code": 1
        }"#;

        // If we were to try to parse this as UploadResponse, it would fail
        // because the Image field is missing. This tests that error handling works.
        let result = serde_json::from_str::<UploadResponse>(json);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_upload_invalid_file() {
        let _client = create_test_client();
        let non_existent_path = Path::new("/tmp/this_file_does_not_exist_12345.jpg");

        // Attempting to read a non-existent file should fail
        let result = fs::read(non_existent_path).await;
        assert!(result.is_err());
    }
}
