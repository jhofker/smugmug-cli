//! The account's public profile (`user/<nick>!profile`): bio, names, contact
//! email and social links, plus its bio and cover images.

use anyhow::Result;
use serde::Deserialize;

use super::SmugMugClient;

/// Social and web links a profile can carry: the name used on the command
/// line and SmugMug's field for it.
pub const LINKS: &[(&str, &str)] = &[
    ("facebook", "Facebook"),
    ("instagram", "Instagram"),
    ("twitter", "Twitter"),
    ("pinterest", "Pinterest"),
    ("tumblr", "Tumblr"),
    ("flickr", "Flickr"),
    ("500px", "FiveHundredPX"),
    ("linkedin", "LinkedIn"),
    ("vimeo", "Vimeo"),
    ("youtube", "YouTube"),
    ("wordpress", "Wordpress"),
    ("blogger", "Blogger"),
    ("custom", "Custom"),
];

/// SmugMug's field for a link name (`facebook` → `Facebook`), ignoring case.
pub fn link_field(name: &str) -> Option<&'static str> {
    LINKS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, field)| *field)
}

/// The profile's fields as SmugMug returns them; social links by field name.
#[derive(Debug, Default)]
pub struct Profile {
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl Profile {
    /// A text field, `None` when missing or empty.
    pub fn text(&self, field: &str) -> Option<&str> {
        self.fields
            .get(field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
}

/// Changes to make to the profile; `None` leaves a field as it is, an empty
/// string clears it.
#[derive(Debug, Default)]
pub struct ProfileUpdate {
    pub bio: Option<String>,
    pub display_name: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub contact_email: Option<String>,
    /// (SmugMug field, URL)
    pub links: Vec<(&'static str, String)>,
}

impl ProfileUpdate {
    pub fn is_empty(&self) -> bool {
        self.to_json().as_object().is_none_or(|o| o.is_empty())
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        let mut put = |field: &str, value: &Option<String>| {
            if let Some(value) = value {
                body.insert(field.into(), value.clone().into());
            }
        };
        put("BioText", &self.bio);
        put("DisplayName", &self.display_name);
        put("FirstName", &self.first_name);
        put("LastName", &self.last_name);
        put("ContactEmail", &self.contact_email);
        for (field, url) in &self.links {
            body.insert((*field).into(), url.clone().into());
        }
        serde_json::Value::Object(body)
    }
}

/// The bio or cover image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProfileImageKind {
    Bio,
    Cover,
}

impl ProfileImageKind {
    fn endpoint(self) -> (&'static str, &'static str) {
        match self {
            ProfileImageKind::Bio => ("bioimage", "BioImage"),
            ProfileImageKind::Cover => ("coverimage", "CoverImage"),
        }
    }
}

/// What a profile image is, as `!bioimage` / `!coverimage` return it.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct ProfileImage {
    #[serde(rename = "FileName", default)]
    pub file_name: String,
    #[serde(rename = "ImageKey", default)]
    pub image_key: String,
}

impl SmugMugClient {
    fn profile_url(nickname: &str) -> String {
        format!("https://api.smugmug.com/api/v2/user/{}!profile", nickname)
    }

    /// The signed-in user's profile.
    pub async fn get_profile(&self) -> Result<Profile> {
        let nickname = self.auth_user().await?.nickname;
        self.get_profile_at(&Self::profile_url(&nickname)).await
    }

    async fn get_profile_at(&self, url: &str) -> Result<Profile> {
        let response = self.get_with_auth(url).await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("Failed to get profile: {} - {}", status, body);
        }
        let mut body: serde_json::Value = serde_json::from_str(&body)?;
        let fields = match body["Response"]["UserProfile"].take() {
            serde_json::Value::Object(fields) => fields,
            _ => anyhow::bail!("Profile response has no UserProfile"),
        };
        Ok(Profile { fields })
    }

    /// Change the signed-in user's profile.
    pub async fn update_profile(&self, update: &ProfileUpdate) -> Result<()> {
        let nickname = self.auth_user().await?.nickname;
        self.update_profile_at(&Self::profile_url(&nickname), update)
            .await
    }

    async fn update_profile_at(&self, url: &str, update: &ProfileUpdate) -> Result<()> {
        let response = self.patch_with_auth(url, update.to_json()).await?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(
                "Failed to update profile: {} - {}",
                status,
                response.text().await?
            );
        }
        Ok(())
    }

    /// The signed-in user's bio or cover image, `None` when there isn't one.
    pub async fn profile_image(&self, kind: ProfileImageKind) -> Result<Option<ProfileImage>> {
        let nickname = self.auth_user().await?.nickname;
        let (path, _) = kind.endpoint();
        self.profile_image_at(
            &format!("https://api.smugmug.com/api/v2/user/{}!{}", nickname, path),
            kind,
        )
        .await
    }

    async fn profile_image_at(
        &self,
        url: &str,
        kind: ProfileImageKind,
    ) -> Result<Option<ProfileImage>> {
        let response = self.get_with_auth(url).await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("Failed to get profile image: {} - {}", status, body);
        }
        let mut body: serde_json::Value = serde_json::from_str(&body)?;
        let (_, locator) = kind.endpoint();
        match body["Response"][locator].take() {
            serde_json::Value::Null => Ok(None),
            image => Ok(Some(serde_json::from_value(image)?)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> SmugMugClient {
        SmugMugClient::new("k".into(), "s".into(), "t".into(), "ts".into())
    }

    #[test]
    fn link_names_map_to_profile_fields() {
        assert_eq!(link_field("Instagram"), Some("Instagram"));
        assert_eq!(link_field("500px"), Some("FiveHundredPX"));
        assert_eq!(link_field("myspace"), None);
    }

    #[test]
    fn update_sends_only_what_was_given() {
        let update = ProfileUpdate {
            bio: Some("Photos of things".into()),
            last_name: Some(String::new()),
            links: vec![("Instagram", "https://instagram.com/me".into())],
            ..Default::default()
        };
        assert_eq!(
            update.to_json(),
            serde_json::json!({
                "BioText": "Photos of things",
                "LastName": "",
                "Instagram": "https://instagram.com/me"
            })
        );
        assert!(!update.is_empty());
        assert!(ProfileUpdate::default().is_empty());
    }

    #[tokio::test]
    async fn profile_is_read_and_patched() {
        let mut server = mockito::Server::new_async().await;
        let _get = server
            .mock("GET", "/api/v2/user/me!profile")
            .with_status(200)
            .with_body(
                r#"{"Response": {"UserProfile": {"BioText": "co-founder",
                    "DisplayName": "Chris", "Twitter": ""}}}"#,
            )
            .create_async()
            .await;
        let patch = server
            .mock("PATCH", "/api/v2/user/me!profile")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"BioText": "New bio"}),
            ))
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;
        let url = format!("{}/api/v2/user/me!profile", server.url());
        let client = client();

        let profile = client.get_profile_at(&url).await.unwrap();
        assert_eq!(profile.text("BioText"), Some("co-founder"));
        assert_eq!(profile.text("Twitter"), None);

        let update = ProfileUpdate {
            bio: Some("New bio".into()),
            ..Default::default()
        };
        client.update_profile_at(&url, &update).await.unwrap();
        patch.assert_async().await;
    }

    #[tokio::test]
    async fn missing_profile_images_are_none() {
        let mut server = mockito::Server::new_async().await;
        let _cover = server
            .mock("GET", "/api/v2/user/me!coverimage")
            .with_status(200)
            .with_body(
                r#"{"Response": {"Locator": "CoverImage",
                    "CoverImage": {"FileName": "C03T7149.jpg", "ImageKey": "5T8MH4d"}}}"#,
            )
            .create_async()
            .await;
        let _bio = server
            .mock("GET", "/api/v2/user/me!bioimage")
            .with_status(404)
            .with_body(r#"{"Code": 404, "Message": "Not Found"}"#)
            .create_async()
            .await;
        let client = client();
        let cover = client
            .profile_image_at(
                &format!("{}/api/v2/user/me!coverimage", server.url()),
                ProfileImageKind::Cover,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cover.file_name, "C03T7149.jpg");
        let bio = client
            .profile_image_at(
                &format!("{}/api/v2/user/me!bioimage", server.url()),
                ProfileImageKind::Bio,
            )
            .await
            .unwrap();
        assert_eq!(bio, None);
    }
}
