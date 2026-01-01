use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
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
