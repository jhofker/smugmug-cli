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
