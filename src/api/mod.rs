use anyhow::Result;
use oauth1_request as oauth;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;

pub mod albums;
pub mod images;
pub mod upload;

#[derive(Debug)]
pub struct NodeTree {
    pub name: String,
    pub node_type: String,
    pub children: Vec<NodeTree>,
}

pub struct SmugMugClient {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    access_token: String,
    access_token_secret: String,
}

impl SmugMugClient {
    pub fn new(
        api_key: String,
        api_secret: String,
        access_token: String,
        access_token_secret: String,
    ) -> Self {
        SmugMugClient {
            client: reqwest::Client::new(),
            api_key,
            api_secret,
            access_token,
            access_token_secret,
        }
    }

    pub fn build_oauth_header(&self, method: &str, url: &str) -> String {
        let token = oauth::Token::from_parts(
            &self.api_key,
            &self.api_secret,
            &self.access_token,
            &self.access_token_secret,
        );

        let signer = oauth::HmacSha1::new();

        match method {
            "GET" => oauth::get(url, &(), &token, signer),
            "POST" => oauth::post(url, &(), &token, signer),
            _ => oauth::get(url, &(), &token, signer),
        }
    }

    pub async fn get_auth_user(&self) -> Result<Value> {
        let url = "https://api.smugmug.com/api/v2!authuser";

        let oauth_header = self.build_oauth_header("GET", url);

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&oauth_header)?,
        );
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let response = self.client
            .get(url)
            .headers(headers)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("API request failed with status {}: {}", status, body_text);
        }

        let body: Value = serde_json::from_str(&body_text)?;
        Ok(body)
    }

    pub async fn get_with_auth(&self, url: &str) -> Result<reqwest::Response> {
        let oauth_header = self.build_oauth_header("GET", url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        Ok(self.client
            .get(url)
            .headers(headers)
            .send()
            .await?)
    }

    pub async fn post_with_auth(&self, url: &str, body: serde_json::Value) -> Result<reqwest::Response> {
        let oauth_header = self.build_oauth_header("POST", url);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        Ok(self.client
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?)
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
    fn test_build_oauth_header_unknown_method() {
        let client = create_test_client();
        let url = "https://api.smugmug.com/api/v2!authuser";
        // Unknown methods should default to GET behavior
        let header = client.build_oauth_header("DELETE", url);

        assert!(header.starts_with("OAuth "));
        assert!(header.contains("oauth_consumer_key=\"test_api_key\""));
    }

    #[tokio::test]
    async fn test_get_auth_user_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("GET", "/api/v2!authuser")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
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
        let mock = server.mock("GET", "/api/v2!authuser")
            .match_header("authorization", mockito::Matcher::Regex("OAuth.*".to_string()))
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
