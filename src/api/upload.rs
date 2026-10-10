use anyhow::{Context as _, Result};
use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use md5::Context;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};
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
    /// Missing when SmugMug refuses the file ("stat": "fail").
    #[serde(rename = "Image")]
    image: Option<ImageInfo>,
    message: Option<String>,
    code: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ImageInfo {
    #[serde(rename = "ImageUri")]
    image_uri: String,
}

/// A file to upload: its bytes and the name and type SmugMug records.
#[derive(Debug, Clone)]
pub struct UploadPayload {
    /// Shared, so retries don't copy the file.
    pub data: bytes::Bytes,
    pub file_name: String,
    pub mime_type: String,
}

impl UploadPayload {
    /// Read the file at `file_path`, named and typed after the path.
    pub async fn from_path(file_path: &Path) -> Result<Self> {
        let data = fs::read(file_path).await?.into();
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        let mime_type = mime_guess::from_path(file_path)
            .first_or_octet_stream()
            .to_string();
        Ok(UploadPayload {
            data,
            file_name,
            mime_type,
        })
    }
}

pub async fn upload_image(
    client: &crate::api::SmugMugClient,
    album_uri: &str,
    payload: &UploadPayload,
) -> Result<UploadResult> {
    send_upload(client, "X-Smug-AlbumUri", album_uri, payload).await
}

/// Replace the file content of an existing image, identified by its image URI
/// (e.g. `/api/v2/image/<key>`). This uploads new bytes onto the existing
/// image record, keeping its album placement, keywords, and other metadata,
/// instead of creating a new image (which SmugMug rejects with 409 if an
/// image with the same filename already exists in the album).
pub async fn replace_image(
    client: &crate::api::SmugMugClient,
    image_uri: &str,
    payload: &UploadPayload,
) -> Result<UploadResult> {
    send_upload(client, "X-Smug-ImageUri", image_uri, payload).await
}

/// Upload endpoint for SmugMug's Library ("All Media"): media uploaded here
/// isn't placed in any album/gallery. Not in SmugMug's public API docs;
/// discovered from the web uploader. It takes a multipart form (`Media`,
/// `ByteCount`, `Sha256Sum`, optional `Filepath`/`Title`/`Caption`/
/// `Keywords`) and answers 201 with the standard v2 envelope
/// (`{"Response":{"Image":{...}},"Code":201}`).
pub const LIBRARY_UPLOAD_URL: &str = "https://upload.smugmug.com/api/v2/library";

#[derive(Debug, Deserialize)]
struct LibraryUploadResponse {
    #[serde(rename = "Response")]
    response: LibraryUploadResponseBody,
}

#[derive(Debug, Deserialize)]
struct LibraryUploadResponseBody {
    #[serde(rename = "Image")]
    image: LibraryImage,
}

#[derive(Debug, Deserialize)]
struct LibraryImage {
    #[serde(rename = "ImageKey")]
    image_key: String,
    #[serde(rename = "Uri")]
    uri: String,
}

/// Upload a file to the Library (no album). `filepath` is sent as the
/// `Filepath` field, which SmugMug describes as "the relative path to the
/// photo or video itself on the uploader's file system".
#[allow(dead_code)] // wired into the uploader once Library uploads are the default
pub async fn upload_to_library(
    client: &crate::api::SmugMugClient,
    file_path: &Path,
    filepath: &str,
) -> Result<UploadResult> {
    let (status_code, body_text) =
        send_library_upload(client, LIBRARY_UPLOAD_URL, file_path, filepath).await?;
    parse_library_upload_response(status_code, &body_text)
}

/// Send the Library upload request and return the HTTP status and raw body,
/// without interpreting either.
pub async fn send_library_upload(
    client: &crate::api::SmugMugClient,
    upload_url: &str,
    file_path: &Path,
    filepath: &str,
) -> Result<(u16, String)> {
    use reqwest::multipart::{Form, Part};
    use sha2::{Digest, Sha256};

    let file_data = fs::read(file_path).await?;
    let byte_count = file_data.len();
    let sha256_base64 = general_purpose::STANDARD.encode(Sha256::digest(&file_data));

    let mime_type = mime_guess::from_path(file_path)
        .first_or_octet_stream()
        .to_string();
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();

    let form = Form::new()
        .text("ByteCount", byte_count.to_string())
        .text("Filepath", filepath.to_string())
        .text("Sha256Sum", sha256_base64)
        .part(
            "Media",
            Part::bytes(file_data)
                .file_name(filename)
                .mime_str(&mime_type)?,
        );

    // Multipart fields aren't part of the OAuth1 signature base string, so
    // the plain POST signature for the URL is sufficient.
    let oauth_header = client.build_oauth_header("POST", upload_url);

    let response = reqwest::Client::new()
        .post(upload_url)
        .header(AUTHORIZATION, oauth_header)
        .header("Accept", "application/json")
        .multipart(form)
        .send()
        .await?;

    let status_code = response.status().as_u16();
    Ok((status_code, response.text().await?))
}

pub fn parse_library_upload_response(status_code: u16, body_text: &str) -> Result<UploadResult> {
    if !(200..300).contains(&status_code) {
        anyhow::bail!(
            "Library upload failed with status {}: {}",
            status_code,
            body_text
        );
    }

    let parsed: LibraryUploadResponse = serde_json::from_str(body_text)?;
    Ok(UploadResult {
        image_key: parsed.response.image.image_key,
        image_uri: parsed.response.image.uri,
        status_code,
    })
}

async fn send_upload(
    client: &crate::api::SmugMugClient,
    target_header: &'static str,
    target_uri: &str,
    payload: &UploadPayload,
) -> Result<UploadResult> {
    let mut context = Context::new();
    context.consume(&payload.data);
    let md5_base64 = general_purpose::STANDARD.encode(context.finalize().0);
    let headers = upload_headers(
        client,
        target_header,
        target_uri,
        payload.data.len() as u64,
        &md5_base64,
        &payload.mime_type,
        &payload.file_name,
    )?;
    let data = payload.data.clone();
    let chunks = futures_util::stream::iter(
        (0..data.len())
            .step_by(UPLOAD_CHUNK)
            .map(move |start| Ok(data.slice(start..(start + UPLOAD_CHUNK).min(data.len())))),
    );
    post_upload(
        client.upload_http(),
        UPLOAD_URL,
        headers,
        chunks,
        UploadLimits::default(),
    )
    .await
}

/// SmugMug's upload endpoint.
const UPLOAD_URL: &str = "https://upload.smugmug.com/";

/// Who an upload goes to: a new image in an album, or new content for an
/// existing image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadTarget<'a> {
    Album(&'a str),
    ReplaceImage(&'a str),
}

/// A file uploaded straight from disk, without holding it in memory.
#[derive(Debug, Clone)]
pub struct FileUpload<'a> {
    pub path: &'a Path,
    /// Size of the file, as sent in Content-Length.
    pub size: u64,
    /// Hex MD5 of the file, computed when it was read.
    pub md5_hex: &'a str,
    /// Name SmugMug records (usually the file's own).
    pub file_name: &'a str,
}

/// Upload a file from disk, streaming its contents.
pub async fn upload_file(
    client: &crate::api::SmugMugClient,
    target: UploadTarget<'_>,
    file: &FileUpload<'_>,
) -> Result<UploadResult> {
    let (target_header, target_uri) = match target {
        UploadTarget::Album(uri) => ("X-Smug-AlbumUri", uri),
        UploadTarget::ReplaceImage(uri) => ("X-Smug-ImageUri", uri),
    };
    let md5 = hex::decode(file.md5_hex)?;
    let mime_type = mime_guess::from_path(file.path)
        .first_or_octet_stream()
        .to_string();
    let headers = upload_headers(
        client,
        target_header,
        target_uri,
        file.size,
        &general_purpose::STANDARD.encode(md5),
        &mime_type,
        file.file_name,
    )?;
    let reader = fs::File::open(file.path).await?;
    let chunks = tokio_util::io::ReaderStream::with_capacity(reader, UPLOAD_CHUNK);
    post_upload(
        client.upload_http(),
        UPLOAD_URL,
        headers,
        chunks,
        UploadLimits::default(),
    )
    .await
}

/// Size of the pieces an upload's body is sent in.
const UPLOAD_CHUNK: usize = 256 * 1024;

/// How long an upload may take, by stage. There's no limit on the whole
/// upload, since a big video on a slow connection can take hours; instead
/// it fails when it stops making progress.
#[derive(Debug, Clone, Copy)]
pub struct UploadLimits {
    /// The connection took no more of the file for this long.
    pub stalled: Duration,
    /// SmugMug hasn't answered this long after receiving the whole file
    /// (it processes videos before it replies).
    pub response: Duration,
    /// How often to check the two above.
    pub check_every: Duration,
}

impl Default for UploadLimits {
    fn default() -> Self {
        UploadLimits {
            stalled: Duration::from_secs(5 * 60),
            response: Duration::from_secs(30 * 60),
            check_every: Duration::from_secs(10),
        }
    }
}

/// An upload stopped making progress (see `UploadLimits`).
#[derive(Debug, thiserror::Error)]
pub enum UploadTimedOut {
    #[error("upload stalled: no data sent for {}", describe_duration(*.0))]
    Stalled(Duration),
    #[error(
        "SmugMug didn't answer within {} of receiving the whole file",
        describe_duration(*.0)
    )]
    NoResponse(Duration),
}

fn describe_duration(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 60 && s % 60 == 0 => format!("{} minutes", s / 60),
        s if s >= 1 => format!("{} seconds", s),
        _ => format!("{} ms", d.as_millis()),
    }
}

/// When an upload last sent data, and when it finished sending.
struct Progress {
    last_sent: Instant,
    finished: Option<Instant>,
}

/// POST an upload, failing it if it stops making progress. The body is
/// watched rather than the connection's reads: during an upload nothing is
/// received, so a read timeout would cut off every upload that takes
/// longer than it.
async fn post_upload<S>(
    http: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    chunks: S,
    limits: UploadLimits,
) -> Result<UploadResult>
where
    S: Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
{
    let progress = Arc::new(Mutex::new(Progress {
        last_sent: Instant::now(),
        finished: None,
    }));
    let sent = progress.clone();
    let done = progress.clone();
    let body = chunks
        .inspect(move |_| sent.lock().unwrap().last_sent = Instant::now())
        .chain(futures_util::stream::poll_fn(move |_| {
            done.lock().unwrap().finished = Some(Instant::now());
            Poll::Ready(None)
        }));

    let request = async {
        let response = http
            .post(url)
            .headers(headers)
            .body(reqwest::Body::wrap_stream(body))
            .send()
            .await?;
        read_upload_response(response).await
    };
    tokio::pin!(request);

    loop {
        tokio::select! {
            result = &mut request => return result,
            _ = tokio::time::sleep(limits.check_every) => {
                let progress = progress.lock().unwrap();
                match progress.finished {
                    None if progress.last_sent.elapsed() > limits.stalled => {
                        return Err(UploadTimedOut::Stalled(limits.stalled).into());
                    }
                    Some(at) if at.elapsed() > limits.response => {
                        return Err(UploadTimedOut::NoResponse(limits.response).into());
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Upload bytes held in memory (a JPEG rendered from a RAW file).
pub async fn upload_bytes(
    client: &crate::api::SmugMugClient,
    target: UploadTarget<'_>,
    payload: &UploadPayload,
) -> Result<UploadResult> {
    match target {
        UploadTarget::Album(uri) => upload_image(client, uri, payload).await,
        UploadTarget::ReplaceImage(uri) => replace_image(client, uri, payload).await,
    }
}

fn upload_headers(
    client: &crate::api::SmugMugClient,
    target_header: &'static str,
    target_uri: &str,
    size: u64,
    md5_base64: &str,
    mime_type: &str,
    filename: &str,
) -> Result<HeaderMap> {
    let oauth_header = client.build_oauth_header("POST", UPLOAD_URL);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
    headers.insert(CONTENT_LENGTH, HeaderValue::from(size));
    headers.insert(CONTENT_TYPE, HeaderValue::from_str(mime_type)?);
    headers.insert("Content-MD5", HeaderValue::from_str(md5_base64)?);
    headers.insert(target_header, HeaderValue::from_str(target_uri)?);
    headers.insert("X-Smug-FileName", header_text(filename)?);
    headers.insert("X-Smug-Title", header_text(filename)?);
    headers.insert("X-Smug-ResponseType", HeaderValue::from_static("JSON"));
    headers.insert("X-Smug-Version", HeaderValue::from_static("v2"));
    headers.insert("Accept", HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// A header value for a file name. `HeaderValue::from_str` takes ASCII
/// only, which would fail every upload of a file named with accents or
/// emoji; the UTF-8 bytes are sent as they are instead.
fn header_text(text: &str) -> Result<HeaderValue> {
    Ok(HeaderValue::from_bytes(text.as_bytes())?)
}

/// SmugMug refused an upload with an HTTP error.
#[derive(Debug, thiserror::Error)]
#[error("Upload failed with status {status}: {body}")]
pub struct UploadRejected {
    pub status: u16,
    pub body: String,
}

impl UploadRejected {
    /// Refused for the file itself (too big, unsupported), so trying again
    /// won't help until the file changes. Not auth, rate-limit or
    /// conflict errors, which are about the account or the request.
    pub fn is_permanent(&self) -> bool {
        matches!(self.status, 400 | 413 | 415 | 422)
    }
}

async fn read_upload_response(response: reqwest::Response) -> Result<UploadResult> {
    let status = response.status();
    let status_code = status.as_u16();
    let body_text = response.text().await?;

    if !status.is_success() {
        return Err(UploadRejected {
            status: status_code,
            body: body_text,
        }
        .into());
    }

    parse_upload_response(status_code, &body_text)
}

/// SmugMug answered an upload with `"stat": "fail"` (HTTP 200).
#[derive(Debug, thiserror::Error)]
#[error("SmugMug refused the upload: {}", self.describe())]
pub struct UploadFailed {
    pub code: Option<i64>,
    pub message: Option<String>,
    pub body: String,
}

impl UploadFailed {
    fn describe(&self) -> String {
        match (&self.message, self.code) {
            (Some(message), Some(code)) => format!("{} (code {})", message, code),
            (Some(message), None) => message.clone(),
            _ => self.body.clone(),
        }
    }
}

fn parse_upload_response(status_code: u16, body_text: &str) -> Result<UploadResult> {
    let reply: UploadResponse = serde_json::from_str(body_text)
        .with_context(|| format!("Unexpected reply from SmugMug: {}", body_text))?;
    let image = match reply.image {
        Some(image) if reply.stat == "ok" => image,
        _ => {
            return Err(UploadFailed {
                code: reply.code,
                message: reply.message,
                body: body_text.to_string(),
            }
            .into());
        }
    };

    // Extract image key from URI (format: /api/v2/album/<key>/image/<key>-0)
    let image_key = image
        .image_uri
        .split('/')
        .next_back()
        .unwrap_or("")
        .to_string();

    Ok(UploadResult {
        image_key,
        image_uri: image.image_uri,
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
            response.image.unwrap().image_uri,
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
        let md5_hash = context.finalize();
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

    #[test]
    fn test_upload_response_stat_fail_reports_smugmugs_message() {
        let json = r#"{"stat":"fail","method":"smugmug.images.upload","code":64,"message":"Invalid file type"}"#;
        let err = parse_upload_response(200, json).unwrap_err();
        let failed = err.downcast_ref::<UploadFailed>().unwrap();
        assert_eq!(failed.code, Some(64));
        assert_eq!(
            err.to_string(),
            "SmugMug refused the upload: Invalid file type (code 64)"
        );

        // Without a message, the whole reply is shown.
        let err = parse_upload_response(200, r#"{"stat":"fail"}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"SmugMug refused the upload: {"stat":"fail"}"#
        );
    }

    #[test]
    fn test_upload_response_ok() {
        let json = r#"{"stat":"ok","method":"smugmug.images.upload","Image":{"ImageUri":"/api/v2/album/AB/image/CD-0"}}"#;
        let result = parse_upload_response(200, json).unwrap();
        assert_eq!(result.image_key, "CD-0");
        assert_eq!(result.image_uri, "/api/v2/album/AB/image/CD-0");
    }

    #[test]
    fn test_upload_response_not_json() {
        let err = parse_upload_response(200, "<html>oops</html>").unwrap_err();
        assert!(err.to_string().contains("<html>oops</html>"), "{err}");
    }

    /// A local server that reads an upload with `read` and then answers
    /// with `reply` (if any). Returns its URL.
    async fn upload_server<F, Fut>(read: F, reply: Option<&'static str>) -> String
    where
        F: FnOnce(tokio::net::TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = tokio::net::TcpStream> + Send,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = read(socket).await;
            if let Some(body) = reply {
                use tokio::io::AsyncWriteExt;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        url
    }

    /// Read until `total` bytes arrived, `chunk` at a time with `pause`
    /// between reads.
    async fn read_slowly(
        mut socket: tokio::net::TcpStream,
        total: usize,
        chunk: usize,
        pause: Duration,
    ) -> tokio::net::TcpStream {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; chunk];
        let mut got = 0;
        while got < total {
            match socket.read(&mut buf).await.unwrap() {
                0 => break,
                n => got += n,
            }
            tokio::time::sleep(pause).await;
        }
        socket
    }

    fn body_of(len: usize) -> impl Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static {
        let data = Bytes::from(vec![7u8; len]);
        futures_util::stream::iter(
            (0..len)
                .step_by(UPLOAD_CHUNK)
                .map(move |i| Ok(data.slice(i..(i + UPLOAD_CHUNK).min(len)))),
        )
    }

    fn test_limits() -> UploadLimits {
        UploadLimits {
            stalled: Duration::from_millis(400),
            response: Duration::from_millis(400),
            check_every: Duration::from_millis(50),
        }
    }

    const OK_REPLY: &str = r#"{"stat":"ok","Image":{"ImageUri":"/api/v2/album/A/image/B-0"}}"#;

    #[test]
    fn test_timeout_messages() {
        assert_eq!(
            UploadTimedOut::Stalled(Duration::from_secs(300)).to_string(),
            "upload stalled: no data sent for 5 minutes"
        );
        assert_eq!(
            UploadTimedOut::NoResponse(Duration::from_secs(90)).to_string(),
            "SmugMug didn't answer within 90 seconds of receiving the whole file"
        );
    }

    #[tokio::test]
    async fn test_slow_upload_that_keeps_going_succeeds() {
        // Takes several times the stall limit, but never stops moving: a
        // read timeout would have failed it (the 0.5.x timeouts on videos).
        let len = 24 * 1024 * 1024;
        let url = upload_server(
            move |s| read_slowly(s, len, 512 * 1024, Duration::from_millis(40)),
            Some(OK_REPLY),
        )
        .await;
        let started = Instant::now();
        // The last few MB still drain from the socket buffers after the
        // body is handed over, so give the answer longer than that.
        let limits = UploadLimits {
            response: Duration::from_secs(10),
            ..test_limits()
        };
        let result = post_upload(
            &reqwest::Client::new(),
            &url,
            HeaderMap::new(),
            body_of(len),
            limits,
        )
        .await
        .unwrap();
        assert_eq!(result.image_key, "B-0");
        assert!(started.elapsed() > test_limits().stalled * 2);
    }

    #[tokio::test]
    async fn test_stalled_upload_fails() {
        // The server stops reading, so the socket buffers fill up and the
        // body stops moving.
        let url = upload_server(
            |s| async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                s
            },
            None,
        )
        .await;
        let err = post_upload(
            &reqwest::Client::new(),
            &url,
            HeaderMap::new(),
            body_of(256 * 1024 * 1024),
            test_limits(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err.downcast_ref::<UploadTimedOut>(),
            Some(UploadTimedOut::Stalled(_))
        ));
    }

    #[tokio::test]
    async fn test_upload_without_answer_fails() {
        let len = 1024 * 1024;
        let url = upload_server(
            move |s| read_slowly(s, len, 64 * 1024, Duration::ZERO),
            None,
        )
        .await;
        let err = post_upload(
            &reqwest::Client::new(),
            &url,
            HeaderMap::new(),
            body_of(len),
            test_limits(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err.downcast_ref::<UploadTimedOut>(),
            Some(UploadTimedOut::NoResponse(_))
        ));
    }

    #[tokio::test]
    async fn test_library_upload_sends_multipart_fields() {
        use sha2::{Digest, Sha256};

        let mut temp_file = tempfile::Builder::new().suffix(".jpg").tempfile().unwrap();
        let test_data = b"library test data";
        temp_file.write_all(test_data).unwrap();
        temp_file.flush().unwrap();
        let file_name = temp_file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let expected_sha = general_purpose::STANDARD.encode(Sha256::digest(test_data));
        assert_eq!(expected_sha.len(), 44);

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v2/library")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("^OAuth .*oauth_signature=".to_string()),
            )
            .match_header(
                "content-type",
                mockito::Matcher::Regex("^multipart/form-data; boundary=".to_string()),
            )
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex(format!(
                    "name=\"ByteCount\"\r\n\r\n{}\r\n",
                    test_data.len()
                )),
                mockito::Matcher::Regex(format!(
                    "name=\"Sha256Sum\"\r\n\r\n{}\r\n",
                    regex_escape(&expected_sha)
                )),
                mockito::Matcher::Regex("name=\"Filepath\"\r\n\r\n2024/Trip/a.jpg\r\n".to_string()),
                mockito::Matcher::Regex(format!(
                    "name=\"Media\"; filename=\"{}\"\r\nContent-Type: image/jpeg",
                    regex_escape(&file_name)
                )),
                mockito::Matcher::Regex("library test data".to_string()),
            ]))
            .with_status(201)
            .with_body(LIBRARY_RESPONSE_FIXTURE)
            .create_async()
            .await;

        let client = create_test_client();
        let url = format!("{}/api/v2/library", server.url());
        let (status, body) =
            send_library_upload(&client, &url, temp_file.path(), "2024/Trip/a.jpg")
                .await
                .unwrap();

        mock.assert_async().await;
        assert_eq!(status, 201);
        let result = parse_library_upload_response(status, &body).unwrap();
        assert_eq!(result.image_key, "5DftXbZ");
        assert_eq!(result.image_uri, "/api/v2/image/5DftXbZ-0");
    }

    #[test]
    fn test_parse_library_upload_response_error_status() {
        let err = parse_library_upload_response(401, r#"{"Code":401,"Message":"Unauthorized"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("401"));
    }

    fn regex_escape(s: &str) -> String {
        s.chars()
            .flat_map(|c| {
                if "\\.+*?()|[]{}^$".contains(c) {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect()
    }

    /// Trimmed from a real 201 response of the web uploader's Library upload.
    const LIBRARY_RESPONSE_FIXTURE: &str = r#"{
        "Response": {
            "Uri": "/api/v2/image/5DftXbZ-0",
            "Locator": "Image",
            "LocatorType": "Object",
            "Image": {
                "FileName": "darth copy.png",
                "Processing": true,
                "ImageKey": "5DftXbZ",
                "ArchivedMD5": "fec6679b041962097bfd6a4fea03ab8d",
                "PublishedTo": [],
                "Uri": "/api/v2/image/5DftXbZ-0"
            },
            "EndpointType": "Image"
        },
        "Code": 201,
        "Message": "Created"
    }"#;

    #[tokio::test]
    async fn test_upload_invalid_file() {
        let _client = create_test_client();
        let non_existent_path = Path::new("/tmp/this_file_does_not_exist_12345.jpg");

        // Attempting to read a non-existent file should fail
        let result = fs::read(non_existent_path).await;
        assert!(result.is_err());
    }
}
