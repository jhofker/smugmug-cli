use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::SmugMugClient;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Comment {
    #[serde(rename = "CommentKey", skip_serializing_if = "Option::is_none")]
    pub comment_key: Option<String>,

    #[serde(rename = "Comment")]
    pub text: String,

    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(rename = "Email", skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    #[serde(rename = "Rating", skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,

    #[serde(rename = "Link", skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,

    #[serde(rename = "SocialID", skip_serializing_if = "Option::is_none")]
    pub social_id: Option<String>,

    #[serde(rename = "ServiceID", skip_serializing_if = "Option::is_none")]
    pub service_id: Option<i32>,

    #[serde(rename = "Date", skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,

    #[serde(rename = "Uri", skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateCommentRequest {
    #[serde(rename = "Comment")]
    pub text: String,

    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(rename = "Email", skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    #[serde(rename = "Rating", skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,

    #[serde(rename = "Link", skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

// Shape of one page of a comments listing (read through
// `get_all_pages` in the client; kept for the deserialization tests).
#[cfg(test)]
#[derive(Debug, Deserialize)]
struct CommentsResponse {
    #[serde(rename = "Response")]
    response: CommentsResponseData,
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
struct CommentsResponseData {
    #[serde(rename = "Comment")]
    comments: Vec<Comment>,
}

#[derive(Debug, Deserialize)]
struct CommentResponse {
    #[serde(rename = "Response")]
    response: CommentResponseData,
}

#[derive(Debug, Deserialize)]
struct CommentResponseData {
    #[serde(rename = "Comment")]
    comment: Comment,
}

/// What a comment is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommentTarget<'a> {
    Image(&'a str),
    Album(&'a str),
}

impl CommentTarget<'_> {
    fn comments_url(&self) -> String {
        match self {
            CommentTarget::Image(key) => {
                format!("https://api.smugmug.com/api/v2/image/{}!comments", key)
            }
            CommentTarget::Album(key) => {
                format!("https://api.smugmug.com/api/v2/album/{}!comments", key)
            }
        }
    }
}

/// SmugMug's limits on a new comment (from the endpoint's OPTIONS).
fn validate(request: &CreateCommentRequest) -> Result<()> {
    if request.text.is_empty() {
        anyhow::bail!("Comment text cannot be empty");
    }
    if request.text.len() > 60000 {
        anyhow::bail!("Comment text must be less than 60,000 characters");
    }
    if request.name.as_ref().is_some_and(|n| n.len() > 50) {
        anyhow::bail!("Name must be less than 50 characters");
    }
    if request.email.as_ref().is_some_and(|e| e.len() > 50) {
        anyhow::bail!("Email must be less than 50 characters");
    }
    if request.rating.is_some_and(|r| r > 5) {
        anyhow::bail!("Rating must be between 0 and 5");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TopKeywordsResponse {
    #[serde(rename = "Response")]
    response: TopKeywordsData,
}

#[derive(Debug, Deserialize)]
struct TopKeywordsData {
    #[serde(rename = "UserTopKeywords")]
    top: TopKeywords,
}

#[derive(Debug, Deserialize)]
struct TopKeywords {
    #[serde(rename = "TopKeywords", default)]
    keywords: Vec<String>,
}

impl SmugMugClient {
    /// List all comments on an image
    pub async fn list_image_comments(&self, image_key: &str) -> Result<Vec<Comment>> {
        self.list_comments(CommentTarget::Image(image_key)).await
    }

    /// List all comments on an image or album
    pub async fn list_comments(&self, target: CommentTarget<'_>) -> Result<Vec<Comment>> {
        self.list_comments_at(&target.comments_url()).await
    }

    async fn list_comments_at(&self, url: &str) -> Result<Vec<Comment>> {
        self.get_all_pages(url, "Comment")
            .await
            .context("Failed to list comments")
    }

    /// Create a new comment on an image
    pub async fn create_image_comment(
        &self,
        image_key: &str,
        request: CreateCommentRequest,
    ) -> Result<Comment> {
        self.create_comment(CommentTarget::Image(image_key), request)
            .await
    }

    /// Create a new comment on an image or album
    pub async fn create_comment(
        &self,
        target: CommentTarget<'_>,
        request: CreateCommentRequest,
    ) -> Result<Comment> {
        self.create_comment_at(&target.comments_url(), request)
            .await
    }

    async fn create_comment_at(&self, url: &str, request: CreateCommentRequest) -> Result<Comment> {
        validate(&request)?;
        let body = serde_json::to_value(&request)?;
        let response = self.post_with_auth(url, body).await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            anyhow::bail!("Failed to create comment: {} - {}", status, body_text);
        }

        let comment_response: CommentResponse = serde_json::from_str(&body_text)?;
        Ok(comment_response.response.comment)
    }

    /// The signed-in user's most used keywords, most used first (SmugMug
    /// returns up to `count`, 1 to 2000).
    pub async fn top_keywords(&self, count: Option<u32>) -> Result<Vec<String>> {
        let mut url = format!(
            "https://api.smugmug.com/api/v2/user/{}!topkeywords",
            self.auth_user().await?.nickname
        );
        if let Some(count) = count {
            url.push_str(&format!("?NumKeywords={}", count));
        }
        self.top_keywords_at(&url).await
    }

    async fn top_keywords_at(&self, url: &str) -> Result<Vec<String>> {
        let response = self.get_with_auth(url).await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("Failed to get top keywords: {} - {}", status, body);
        }
        let parsed: TopKeywordsResponse = serde_json::from_str(&body)?;
        Ok(parsed.response.top.keywords)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[tokio::test]
    async fn test_list_comments_success() {
        let mut server = Server::new_async().await;
        let api_url = server.url();

        let _mock = server
            .mock("GET", "/api/v2/image/IMG123!comments")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("OAuth.*".to_string()),
            )
            .with_status(200)
            .with_body(
                r#"{
                "Response": {
                    "Comment": [
                        {
                            "CommentKey": "CMT123",
                            "Comment": "Great photo!",
                            "Name": "John Doe",
                            "Rating": 5,
                            "Date": "2024-12-15"
                        }
                    ]
                }
            }"#,
            )
            .create_async()
            .await;

        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        // Note: This test won't actually call the mock server because the URL is hardcoded
        // in the implementation. We're primarily testing the struct deserialization here.
        let result = serde_json::from_str::<CommentsResponse>(
            r#"{
            "Response": {
                "Comment": [
                    {
                        "CommentKey": "CMT123",
                        "Comment": "Great photo!",
                        "Name": "John Doe",
                        "Rating": 5,
                        "Date": "2024-12-15"
                    }
                ]
            }
        }"#,
        );

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.response.comments.len(), 1);
        assert_eq!(response.response.comments[0].text, "Great photo!");
    }

    #[tokio::test]
    async fn test_create_comment_validation_empty_text() {
        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        let request = CreateCommentRequest {
            text: String::new(),
            name: None,
            email: None,
            rating: None,
            link: None,
        };

        let result = client.create_image_comment("IMG123", request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_create_comment_validation_text_too_long() {
        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        let request = CreateCommentRequest {
            text: "a".repeat(60001),
            name: None,
            email: None,
            rating: None,
            link: None,
        };

        let result = client.create_image_comment("IMG123", request).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("60,000 characters")
        );
    }

    #[tokio::test]
    async fn test_create_comment_validation_name_too_long() {
        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        let request = CreateCommentRequest {
            text: "Great photo!".to_string(),
            name: Some("a".repeat(51)),
            email: None,
            rating: None,
            link: None,
        };

        let result = client.create_image_comment("IMG123", request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("50 characters"));
    }

    #[tokio::test]
    async fn test_create_comment_validation_rating_invalid() {
        let client = SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        );

        let request = CreateCommentRequest {
            text: "Great photo!".to_string(),
            name: None,
            email: None,
            rating: Some(6),
            link: None,
        };

        let result = client.create_image_comment("IMG123", request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("between 0 and 5"));
    }

    #[test]
    fn test_comment_deserialization() {
        let json = r#"{
            "CommentKey": "CMT123",
            "Comment": "Beautiful sunset!",
            "Name": "Jane Smith",
            "Email": "jane@example.com",
            "Rating": 4,
            "Date": "2024-12-15",
            "Uri": "/api/v2/comment/CMT123"
        }"#;

        let comment: Result<Comment, _> = serde_json::from_str(json);
        assert!(comment.is_ok());

        let comment = comment.unwrap();
        assert_eq!(comment.comment_key, Some("CMT123".to_string()));
        assert_eq!(comment.text, "Beautiful sunset!");
        assert_eq!(comment.name, Some("Jane Smith".to_string()));
        assert_eq!(comment.rating, Some(4));
    }

    #[test]
    fn test_create_comment_request_serialization() {
        let request = CreateCommentRequest {
            text: "Great photo!".to_string(),
            name: Some("John Doe".to_string()),
            email: Some("john@example.com".to_string()),
            rating: Some(5),
            link: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["Comment"], "Great photo!");
        assert_eq!(json["Name"], "John Doe");
        assert_eq!(json["Rating"], 5);
    }

    fn test_client() -> SmugMugClient {
        SmugMugClient::new(
            "test_key".to_string(),
            "test_secret".to_string(),
            "test_token".to_string(),
            "test_token_secret".to_string(),
        )
    }

    #[test]
    fn comment_targets_use_image_or_album_urls() {
        assert_eq!(
            CommentTarget::Album("SJT3DX").comments_url(),
            "https://api.smugmug.com/api/v2/album/SJT3DX!comments"
        );
        assert_eq!(
            CommentTarget::Image("abc-0").comments_url(),
            "https://api.smugmug.com/api/v2/image/abc-0!comments"
        );
    }

    #[tokio::test]
    async fn album_comments_are_listed_and_posted() {
        let mut server = Server::new_async().await;
        let list = server
            .mock("GET", "/api/v2/album/ALB!comments")
            .with_status(200)
            .with_body(
                r#"{"Response": {"Comment": [{"CommentKey": "C1", "Comment": "Lovely set"}],
                    "Pages": {"Total": 1, "Start": 1, "Count": 1}}}"#,
            )
            .create_async()
            .await;
        let post = server
            .mock("POST", "/api/v2/album/ALB!comments")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"Comment": "Thanks!", "Name": "Jordan"}),
            ))
            .with_status(201)
            .with_body(r#"{"Response": {"Comment": {"CommentKey": "C2", "Comment": "Thanks!"}}}"#)
            .create_async()
            .await;
        let client = test_client();
        let url = format!("{}/api/v2/album/ALB!comments", server.url());

        let comments = client.list_comments_at(&url).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "Lovely set");

        let request = CreateCommentRequest {
            text: "Thanks!".into(),
            name: Some("Jordan".into()),
            email: None,
            rating: None,
            link: None,
        };
        let created = client.create_comment_at(&url, request).await.unwrap();
        assert_eq!(created.comment_key.as_deref(), Some("C2"));
        list.assert_async().await;
        post.assert_async().await;
    }

    #[tokio::test]
    async fn top_keywords_reads_the_list() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/v2/user/me!topkeywords")
            .match_query(mockito::Matcher::UrlEncoded(
                "NumKeywords".into(),
                "3".into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"Response": {"Locator": "UserTopKeywords",
                    "UserTopKeywords": {"TopKeywords": ["iphone", "beans", "costa rica"]}}}"#,
            )
            .create_async()
            .await;
        let keywords = test_client()
            .top_keywords_at(&format!(
                "{}/api/v2/user/me!topkeywords?NumKeywords=3",
                server.url()
            ))
            .await
            .unwrap();
        assert_eq!(keywords, ["iphone", "beans", "costa rica"]);
    }
}
