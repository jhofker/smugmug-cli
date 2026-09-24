//! OAuth 1.0a three-legged sign-in for SmugMug, used to obtain an access
//! token pair from just the API key and secret. Uses the out-of-band ("oob")
//! callback: the user approves access in a browser and SmugMug shows a
//! 6-digit verifier code to type back into the CLI.

use anyhow::{Context, Result};
use oauth1_request as oauth;

const OAUTH_BASE: &str = "https://api.smugmug.com/services/oauth/1.0a";

/// A token/secret pair returned by one of the OAuth token endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenPair {
    pub token: String,
    pub secret: String,
}

/// Endpoints for the sign-in flow; overridable so tests can point at a mock
/// server.
pub struct OAuthEndpoints {
    pub request_token_url: String,
    pub access_token_url: String,
    pub authorize_url: String,
}

impl Default for OAuthEndpoints {
    fn default() -> Self {
        OAuthEndpoints {
            request_token_url: format!("{}/getRequestToken", OAUTH_BASE),
            access_token_url: format!("{}/getAccessToken", OAUTH_BASE),
            authorize_url: format!("{}/authorize", OAUTH_BASE),
        }
    }
}

/// Step 1: get a temporary request token, signed with only the API key and
/// secret.
pub async fn get_request_token(
    endpoints: &OAuthEndpoints,
    api_key: &str,
    api_secret: &str,
) -> Result<TokenPair> {
    let mut builder: oauth::Builder<'_, _, &str> = oauth::Builder::new(
        oauth::Credentials::new(api_key, api_secret),
        oauth::HmacSha1::new(),
    );
    builder.callback("oob");
    let header = builder.get(&endpoints.request_token_url, &());

    fetch_token_pair(&endpoints.request_token_url, header)
        .await
        .context("Failed to get a request token (check your API key and secret)")
}

/// Step 2: the URL where the user approves access. Full access with modify
/// permission is needed for uploads and deletes.
pub fn authorize_url(endpoints: &OAuthEndpoints, request_token: &TokenPair) -> String {
    let mut url = url::Url::parse(&endpoints.authorize_url).expect("valid authorize URL");
    url.query_pairs_mut()
        .append_pair("oauth_token", &request_token.token)
        .append_pair("Access", "Full")
        .append_pair("Permissions", "Modify");
    url.to_string()
}

/// Step 3: exchange the request token plus the verifier code the user was
/// shown for a long-lived access token.
pub async fn get_access_token(
    endpoints: &OAuthEndpoints,
    api_key: &str,
    api_secret: &str,
    request_token: &TokenPair,
    verifier: &str,
) -> Result<TokenPair> {
    let token = oauth::Token::from_parts(
        api_key,
        api_secret,
        request_token.token.as_str(),
        request_token.secret.as_str(),
    );
    let mut builder = oauth::Builder::with_token(token, oauth::HmacSha1::new());
    builder.verifier(verifier);
    let header = builder.get(&endpoints.access_token_url, &());

    fetch_token_pair(&endpoints.access_token_url, header)
        .await
        .context("Failed to exchange the verifier code for an access token")
}

async fn fetch_token_pair(url: &str, authorization: String) -> Result<TokenPair> {
    let response = reqwest::Client::new()
        .get(url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("HTTP {}: {}", status.as_u16(), body.trim());
    }
    parse_token_response(&body)
}

/// Parse an `oauth_token=...&oauth_token_secret=...` form-encoded body.
fn parse_token_response(body: &str) -> Result<TokenPair> {
    let mut token = None;
    let mut secret = None;
    for (key, value) in url::form_urlencoded::parse(body.trim().as_bytes()) {
        match key.as_ref() {
            "oauth_token" => token = Some(value.into_owned()),
            "oauth_token_secret" => secret = Some(value.into_owned()),
            _ => {}
        }
    }
    match (token, secret) {
        (Some(token), Some(secret)) => Ok(TokenPair { token, secret }),
        _ => anyhow::bail!("Unexpected token response: {}", body.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints(server: &mockito::Server) -> OAuthEndpoints {
        OAuthEndpoints {
            request_token_url: format!("{}/getRequestToken", server.url()),
            access_token_url: format!("{}/getAccessToken", server.url()),
            authorize_url: format!("{}/authorize", server.url()),
        }
    }

    #[test]
    fn test_parse_token_response() {
        let pair = parse_token_response(
            "oauth_token=abc&oauth_token_secret=d%2Fef&oauth_callback_confirmed=true",
        )
        .unwrap();
        assert_eq!(pair.token, "abc");
        assert_eq!(pair.secret, "d/ef");
    }

    #[test]
    fn test_parse_token_response_missing_secret() {
        assert!(parse_token_response("oauth_problem=signature_invalid").is_err());
    }

    #[test]
    fn test_authorize_url() {
        let url = authorize_url(
            &OAuthEndpoints::default(),
            &TokenPair {
                token: "req token".to_string(),
                secret: "s".to_string(),
            },
        );
        assert_eq!(
            url,
            "https://api.smugmug.com/services/oauth/1.0a/authorize?oauth_token=req+token&Access=Full&Permissions=Modify"
        );
    }

    #[tokio::test]
    async fn test_request_token_sends_oob_callback() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/getRequestToken")
            .match_header(
                "authorization",
                mockito::Matcher::AllOf(vec![
                    mockito::Matcher::Regex("oauth_callback=\"oob\"".to_string()),
                    mockito::Matcher::Regex("oauth_consumer_key=\"key\"".to_string()),
                ]),
            )
            .with_body("oauth_token=req&oauth_token_secret=reqsecret&oauth_callback_confirmed=true")
            .create_async()
            .await;

        let pair = get_request_token(&endpoints(&server), "key", "secret")
            .await
            .unwrap();
        mock.assert_async().await;
        assert_eq!(
            pair,
            TokenPair {
                token: "req".to_string(),
                secret: "reqsecret".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_access_token_sends_verifier_and_request_token() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/getAccessToken")
            .match_header(
                "authorization",
                mockito::Matcher::AllOf(vec![
                    mockito::Matcher::Regex("oauth_verifier=\"123456\"".to_string()),
                    mockito::Matcher::Regex("oauth_token=\"req\"".to_string()),
                ]),
            )
            .with_body("oauth_token=access&oauth_token_secret=accesssecret")
            .create_async()
            .await;

        let request_token = TokenPair {
            token: "req".to_string(),
            secret: "reqsecret".to_string(),
        };
        let pair = get_access_token(
            &endpoints(&server),
            "key",
            "secret",
            &request_token,
            "123456",
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(pair.token, "access");
        assert_eq!(pair.secret, "accesssecret");
    }

    #[tokio::test]
    async fn test_request_token_error_status() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/getRequestToken")
            .with_status(401)
            .with_body("oauth_problem=consumer_key_unknown")
            .create_async()
            .await;

        let err = get_request_token(&endpoints(&server), "bad", "bad")
            .await
            .unwrap_err();
        assert!(format!("{:#}", err).contains("consumer_key_unknown"));
    }
}
