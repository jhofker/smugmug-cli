use anyhow::Result;
use oauth1_request as oauth;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

pub mod albums;
pub mod comments;
pub mod images;
pub mod oauth_flow;
pub mod upload;

#[derive(Debug)]
pub struct NodeTree {
    pub name: String,
    pub node_type: String,
    pub children: Vec<NodeTree>,
}

/// Time allowed to connect to SmugMug.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Time a connection may go without any data before the request fails, so
/// a stalled connection can't hang an unattended backup. There's no limit
/// on a whole request: a big video may take a long time to upload.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Tries per API request (see `send_retrying`).
const MAX_ATTEMPTS: u32 = 5;

/// 1s, 2s, 4s, 8s, ... plus up to half a second of jitter so concurrent
/// workers don't retry in lockstep.
fn backoff(attempt: u32) -> std::time::Duration {
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() % 500)
        .unwrap_or(0);
    std::time::Duration::from_millis(
        1000 * 2u64.pow(attempt.saturating_sub(1).min(6)) + jitter as u64,
    )
}

/// The wait a 429/503 response asks for, in seconds (capped at 10 minutes).
fn retry_after(response: &reqwest::Response) -> Option<std::time::Duration> {
    let secs: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(std::time::Duration::from_secs(secs.min(600)))
}

pub struct SmugMugClient {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    access_token: String,
    access_token_secret: String,
    /// The authenticated user's nickname and root node URI, fetched once.
    auth_user: tokio::sync::OnceCell<AuthUser>,
}

/// Who the client is signed in as.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub nickname: String,
    /// URI of the root folder node.
    pub root_node_uri: String,
}

impl SmugMugClient {
    pub fn new(
        api_key: String,
        api_secret: String,
        access_token: String,
        access_token_secret: String,
    ) -> Self {
        SmugMugClient {
            client: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                .build()
                // Fails only if TLS can't be initialized, where
                // reqwest::Client::new() would panic too.
                .expect("Failed to set up the HTTP client (TLS initialization failed)"),
            api_key,
            api_secret,
            access_token,
            access_token_secret,
            auth_user: tokio::sync::OnceCell::new(),
        }
    }

    /// The HTTP client, so uploads reuse its connections.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn build_oauth_header(&self, method: &str, url: &str) -> String {
        self.build_oauth_header_with_query(method, url, &())
    }

    /// Same as `build_oauth_header`, but for a request that carries query
    /// parameters (e.g. pagination's `start`/`count`). OAuth1 signing
    /// requires those parameters to be passed in separately rather than
    /// embedded in `url` — a `url` containing a `?query` part panics inside
    /// the oauth1-request crate. Callers must send the request with the same
    /// `query` value (e.g. reqwest's `.query(query)`) so the signed
    /// parameters match what is actually sent.
    pub fn build_oauth_header_with_query<T: oauth::Request>(
        &self,
        method: &str,
        url: &str,
        query: &T,
    ) -> String {
        let token = oauth::Token::from_parts(
            &self.api_key,
            &self.api_secret,
            &self.access_token,
            &self.access_token_secret,
        );

        let signer = oauth::HmacSha1::new();

        match method {
            "GET" => oauth::get(url, query, &token, signer),
            "POST" => oauth::post(url, query, &token, signer),
            "DELETE" => oauth::delete(url, query, &token, signer),
            "PATCH" => oauth::patch(url, query, &token, signer),
            _ => oauth::get(url, query, &token, signer),
        }
    }

    pub async fn get_auth_user(&self) -> Result<Value> {
        let url = "https://api.smugmug.com/api/v2!authuser";

        let oauth_header = self.build_oauth_header("GET", url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("API request failed with status {}: {}", status, body_text);
        }

        let body: Value = serde_json::from_str(&body_text)?;
        Ok(body)
    }

    pub async fn get_user_features(&self, user_uri: &str) -> Result<Value> {
        let url = format!("https://api.smugmug.com{}!features", user_uri);

        let oauth_header = self.build_oauth_header("GET", &url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client.get(&url).headers(headers).send().await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("API request failed with status {}: {}", status, body_text);
        }

        let body: Value = serde_json::from_str(&body_text)?;
        Ok(body)
    }

    /// Authenticated request with any HTTP method to an arbitrary API path
    /// (e.g. `/api/v2!authuser?_verbosity=1`) or absolute URL, returning the
    /// HTTP status and raw body without interpreting either. Query parameters
    /// are signed separately, as OAuth1 requires. Used for exploring
    /// endpoints that aren't publicly documented; OPTIONS makes SmugMug
    /// describe an endpoint's methods and parameters. A `body` is sent as
    /// JSON, which is how SmugMug takes the parameters of POST/PATCH calls.
    pub async fn request_raw(
        &self,
        method: &str,
        path_or_url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, String)> {
        let full = if path_or_url.starts_with("http") {
            path_or_url.to_string()
        } else {
            format!("https://api.smugmug.com{}", path_or_url)
        };
        let mut url = reqwest::Url::parse(&full)?;
        let params: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        url.set_query(None);

        let token = oauth::Token::from_parts(
            self.api_key.as_str(),
            self.api_secret.as_str(),
            self.access_token.as_str(),
            self.access_token_secret.as_str(),
        );
        let oauth_header = oauth::Builder::with_token(token, oauth::HmacSha1::new()).authorize(
            method,
            url.as_str(),
            &oauth::ParameterList::new(params.clone()),
        );

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let mut request = self
            .client
            .request(reqwest::Method::from_bytes(method.as_bytes())?, url)
            .query(&params)
            .headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        Ok((status, response.text().await?))
    }

    /// GET a SmugMug list endpoint (e.g. `.../album/KEY!images`) and every
    /// following page, returning all items of the `locator` array (e.g.
    /// "AlbumImage", "Album", "Comment", "Node") from each page's `Response`.
    /// SmugMug returns one page per request (often 100 items, sometimes
    /// fewer) and links the next one as `Response.Pages.NextPage`; a missing
    /// `locator` array (empty list) yields no items. Later pages are fetched
    /// from the same host as `first_url`, with their query parameters
    /// signed separately as OAuth1 requires.
    pub async fn get_all_pages<T: serde::de::DeserializeOwned>(
        &self,
        first_url: &str,
        locator: &str,
    ) -> Result<Vec<T>> {
        let mut parsed = reqwest::Url::parse(first_url)?;
        let origin = parsed.origin().ascii_serialization();
        // The first URL's own query parameters are signed separately too.
        let first_params: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        parsed.set_query(None);

        let mut items = Vec::new();
        let mut next: Option<(String, Vec<(String, String)>)> =
            Some((parsed.to_string(), first_params));
        let mut pages = 0;

        while let Some((url, params)) = next.take() {
            pages += 1;
            if pages > 10_000 {
                anyhow::bail!("Gave up listing {} after 10,000 pages", first_url);
            }

            let response = self
                .send_retrying(true, || {
                    // A fresh signature (nonce, timestamp) for each attempt.
                    let oauth_header = self.build_oauth_header_with_query(
                        "GET",
                        &url,
                        &oauth::ParameterList::new(params.clone()),
                    );
                    let mut headers = HeaderMap::new();
                    headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
                    headers.insert("Accept", HeaderValue::from_static("application/json"));
                    let mut request = self.client.get(&url).headers(headers);
                    if !params.is_empty() {
                        request = request.query(&params);
                    }
                    Ok(request)
                })
                .await?;
            let status = response.status();
            let body_text = response.text().await?;
            if !status.is_success() {
                anyhow::bail!("Request to {} failed: {} - {}", url, status, body_text);
            }

            let mut body: Value = serde_json::from_str(&body_text)?;
            let response_data = &mut body["Response"];
            if let Some(array) = response_data.get_mut(locator).map(Value::take) {
                let page_items: Vec<T> = serde_json::from_value(array)?;
                items.extend(page_items);
            }

            next = response_data["Pages"]["NextPage"]
                .as_str()
                .map(|next_page| {
                    let (path, query) = next_page.split_once('?').unwrap_or((next_page, ""));
                    let params = url::form_urlencoded::parse(query.as_bytes())
                        .into_owned()
                        .collect();
                    (format!("{}{}", origin, path), params)
                });
        }

        Ok(items)
    }

    pub async fn get_with_auth(&self, url: &str) -> Result<reqwest::Response> {
        self.send_retrying(true, || {
            let oauth_header = self.build_oauth_header("GET", url);
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));
            Ok(self.client.get(url).headers(headers))
        })
        .await
    }

    pub async fn post_with_auth(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        self.send_retrying(false, || {
            let oauth_header = self.build_oauth_header("POST", url);
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
            headers.insert("Accept", HeaderValue::from_static("application/json"));
            headers.insert("Content-Type", HeaderValue::from_static("application/json"));
            Ok(self.client.post(url).headers(headers).json(&body))
        })
        .await
    }

    /// Send a request, retrying when SmugMug is rate limiting (429) or the
    /// connection failed, waiting as `Retry-After` says or backing off
    /// exponentially. Server errors (5xx) are retried only for `idempotent`
    /// requests: a POST that failed with one may still have created what it
    /// asked for. `make` builds the request afresh for each attempt (OAuth
    /// signatures can't be reused).
    async fn send_retrying(
        &self,
        idempotent: bool,
        make: impl Fn() -> Result<reqwest::RequestBuilder>,
    ) -> Result<reqwest::Response> {
        let mut attempt = 1;
        loop {
            let last = attempt >= MAX_ATTEMPTS;
            match make()?.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry = status == 429 || (idempotent && status >= 500);
                    if !retry || last {
                        return Ok(response);
                    }
                    let wait = retry_after(&response).unwrap_or_else(|| backoff(attempt));
                    tokio::time::sleep(wait).await;
                }
                Err(e) if !last && (e.is_connect() || (idempotent && e.is_timeout())) => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(e) => return Err(e.into()),
            }
            attempt += 1;
        }
    }

    pub async fn delete_with_auth(&self, url: &str) -> Result<reqwest::Response> {
        let oauth_header = self.build_oauth_header("DELETE", url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        Ok(self.client.delete(url).headers(headers).send().await?)
    }

    pub async fn patch_with_auth(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<reqwest::Response> {
        let oauth_header = self.build_oauth_header("PATCH", url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        Ok(self
            .client
            .patch(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Item {
        #[serde(rename = "Name")]
        name: String,
    }

    #[tokio::test]
    async fn test_get_all_pages_follows_next_page() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/api/v2/thing!items")
            .match_query(mockito::Matcher::Missing)
            .with_body(
                r#"{"Response":{"Item":[{"Name":"a"},{"Name":"b"}],
                    "Pages":{"Total":3,"Start":1,"Count":2,
                             "NextPage":"/api/v2/thing!items?start=3&count=2"}}}"#,
            )
            .create_async()
            .await;
        let second = server
            .mock("GET", "/api/v2/thing!items")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("start".into(), "3".into()),
                mockito::Matcher::UrlEncoded("count".into(), "2".into()),
            ]))
            .match_header(
                "authorization",
                mockito::Matcher::Regex("oauth_signature=".to_string()),
            )
            .with_body(
                r#"{"Response":{"Item":[{"Name":"c"}],
                    "Pages":{"Total":3,"Start":3,"Count":1}}}"#,
            )
            .create_async()
            .await;

        let client = create_test_client();
        let items: Vec<Item> = client
            .get_all_pages(&format!("{}/api/v2/thing!items", server.url()), "Item")
            .await
            .unwrap();

        first.assert_async().await;
        second.assert_async().await;
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn rate_limited_requests_are_retried() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/api/v2/thing!items")
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create_async()
            .await;
        let ok = server
            .mock("GET", "/api/v2/thing!items")
            .with_body(r#"{"Response":{"Item":[{"Name":"a"}]}}"#)
            .expect(1)
            .create_async()
            .await;

        let client = create_test_client();
        let items: Vec<Item> = client
            .get_all_pages(&format!("{}/api/v2/thing!items", server.url()), "Item")
            .await
            .unwrap();
        limited.assert_async().await;
        ok.assert_async().await;
        assert_eq!(items.len(), 1);
    }

    #[tokio::test]
    async fn failed_posts_are_not_retried_on_server_errors() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v2/node/x!children")
            .with_status(500)
            .expect(1)
            .create_async()
            .await;
        let client = create_test_client();
        let response = client
            .post_with_auth(
                &format!("{}/api/v2/node/x!children", server.url()),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 500);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_all_pages_empty_list() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/thing!items")
            .with_body(r#"{"Response":{"Pages":{"Total":0,"Start":1,"Count":0}}}"#)
            .create_async()
            .await;

        let client = create_test_client();
        let items: Vec<Item> = client
            .get_all_pages(&format!("{}/api/v2/thing!items", server.url()), "Item")
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_get_all_pages_error_status() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/thing!items")
            .with_status(404)
            .with_body(r#"{"Code":404,"Message":"Not Found"}"#)
            .create_async()
            .await;

        let client = create_test_client();
        let result: Result<Vec<Item>> = client
            .get_all_pages(&format!("{}/api/v2/thing!items", server.url()), "Item")
            .await;
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    fn create_test_client() -> SmugMugClient {
        SmugMugClient::new(
            "test_api_key".to_string(),
            "test_api_secret".to_string(),
            "test_access_token".to_string(),
            "test_access_token_secret".to_string(),
        )
    }

    #[test]
    fn test_smugmug_client_new() {
        let client = create_test_client();
        assert_eq!(client.api_key, "test_api_key");
        assert_eq!(client.api_secret, "test_api_secret");
        assert_eq!(client.access_token, "test_access_token");
        assert_eq!(client.access_token_secret, "test_access_token_secret");
    }

    #[test]
    fn test_build_oauth_header_get() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2!authuser";
        let header = client.build_oauth_header("GET", url);

        // Verify the header starts with "OAuth " and contains required parameters
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key="));
        assert!(header.contains("oauth_token="));
        assert!(header.contains("oauth_signature_method="));
        assert!(header.contains("oauth_timestamp="));
        assert!(header.contains("oauth_nonce="));
        assert!(header.contains("oauth_signature="));
    }

    #[test]
    fn test_build_oauth_header_post() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2/node/abc123!children";
        let header = client.build_oauth_header("POST", url);

        // Verify the header starts with "OAuth " and contains required parameters
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"test_api_key\""));
        assert!(header.contains("oauth_token=\"test_access_token\""));
        assert!(header.contains("oauth_signature_method=\"HMAC-SHA1\""));
    }

    #[test]
    fn test_build_oauth_header_delete() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2/image/IMG123";
        let header = client.build_oauth_header("DELETE", url);

        // Verify the header starts with "OAuth " and contains required parameters
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"test_api_key\""));
        assert!(header.contains("oauth_token=\"test_access_token\""));
        assert!(header.contains("oauth_signature_method=\"HMAC-SHA1\""));
    }

    #[test]
    fn test_build_oauth_header_patch() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2/image/IMG123";
        let header = client.build_oauth_header("PATCH", url);

        // Verify the header starts with "OAuth " and contains required parameters
        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"test_api_key\""));
        assert!(header.contains("oauth_token=\"test_access_token\""));
        assert!(header.contains("oauth_signature_method=\"HMAC-SHA1\""));
    }

    #[test]
    fn test_build_oauth_header_unknown_method() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2!authuser";
        // Unknown methods should default to GET behavior
        let header = client.build_oauth_header("PUT", url);

        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"test_api_key\""));
    }

    #[tokio::test]
    async fn test_get_auth_user_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v2!authuser")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"Response":{"User":{"Uri":"/api/v2/user/testuser"}}}"#)
            .create_async()
            .await;

        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        // Note: This test will actually try to connect to the real API
        // because we can't easily inject the mock server URL into the client
        // In a real-world scenario, you'd want to make the base URL configurable

        drop(mock);
    }

    #[tokio::test]
    async fn test_get_auth_user_unauthorized() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v2!authuser")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(401)
            .with_body("Unauthorized")
            .create_async()
            .await;

        // Note: Similar limitation as above - would need configurable base URL

        drop(mock);
    }

    #[test]
    fn test_node_tree_structure() {
        let tree = NodeTree {
            name: "Root".to_string(),
            node_type: "Folder".to_string(),
            children: vec![
                NodeTree {
                    name: "Child1".to_string(),
                    node_type: "Album".to_string(),
                    children: vec![],
                },
                NodeTree {
                    name: "Child2".to_string(),
                    node_type: "Folder".to_string(),
                    children: vec![],
                },
            ],
        };

        assert_eq!(tree.name, "Root");
        assert_eq!(tree.node_type, "Folder");
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].name, "Child1");
        assert_eq!(tree.children[1].name, "Child2");
    }
}
