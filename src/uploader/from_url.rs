//! `sm upload --url`: SmugMug fetches each URL into the album itself
//! (`album/<key>!uploadfromuri`), so the bytes never reach this machine and
//! the local hash cache can't tell duplicates apart. Albums come from the
//! same `AlbumSeries` as file uploads, so a full album overflows into
//! `Name (2)`.

use anyhow::{Result, bail};

use super::album_series::{AlbumSeries, AlbumSeriesBackend};
use crate::api::SmugMugClient;

/// Sending one URL to an album.
// Only implemented and used inside this crate with concrete types, so the
// Send-bound caveat of async fns in public traits doesn't matter here.
#[allow(async_fn_in_trait)]
pub trait UrlUploader {
    /// Returns the new image's URI when SmugMug says.
    async fn upload_url(
        &self,
        album_key: &str,
        url: &str,
        file_name: Option<&str>,
    ) -> Result<Option<String>>;
}

impl UrlUploader for SmugMugClient {
    async fn upload_url(
        &self,
        album_key: &str,
        url: &str,
        file_name: Option<&str>,
    ) -> Result<Option<String>> {
        crate::api::upload::upload_from_uri(self, album_key, url, file_name).await
    }
}

/// What happened to one URL.
#[derive(Debug, PartialEq)]
pub struct UrlOutcome {
    pub url: String,
    /// The album it went (or, on a dry run, would go) into.
    pub album: Option<String>,
    /// The new image's URI, or why it failed.
    pub result: std::result::Result<Option<String>, String>,
}

/// Check that `url` is something SmugMug can fetch: an http(s) address.
pub fn check_url(url: &str) -> Result<()> {
    match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => Ok(()),
        _ => bail!("Not an http(s) URL: {}", url),
    }
}

/// The file name a URL points at ("https://x/photos/IMG_1.jpg?w=1" is
/// IMG_1.jpg), when its last path segment looks like a file.
pub fn file_name_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let decoded = percent_decode(last);
    let has_extension = std::path::Path::new(&decoded)
        .extension()
        .is_some_and(|e| !e.is_empty());
    has_extension.then_some(decoded)
}

fn percent_decode(segment: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", segment.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| segment.to_string())
}

/// Send each URL to the series' current album, one at a time (SmugMug does
/// the fetching). A failed URL gives its album slot back.
pub async fn upload_urls<B: AlbumSeriesBackend, U: UrlUploader>(
    series: &AlbumSeries<B>,
    uploader: &U,
    urls: &[String],
    dry_run: bool,
) -> Vec<UrlOutcome> {
    let mut outcomes = Vec::new();
    for url in urls {
        let album = match series.claim().await {
            Ok(album) => album,
            Err(e) => {
                outcomes.push(UrlOutcome {
                    url: url.clone(),
                    album: None,
                    result: Err(e.to_string()),
                });
                continue;
            }
        };
        let result = if dry_run {
            Ok(None)
        } else {
            let file_name = file_name_from_url(url);
            match uploader
                .upload_url(&album.album_key, url, file_name.as_deref())
                .await
            {
                Ok(uri) => Ok(uri),
                Err(e) => {
                    series.release(&album).await;
                    Err(e.to_string())
                }
            }
        };
        outcomes.push(UrlOutcome {
            url: url.clone(),
            album: Some(album.name),
            result,
        });
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::albums::Album;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        /// Albums that exist, with their image counts.
        albums: Mutex<Vec<(Album, u64)>>,
        sent: Mutex<Vec<(String, String, Option<String>)>>,
    }

    fn album(name: &str) -> Album {
        Album {
            album_key: format!("key-{}", name),
            name: name.to_string(),
            url_name: String::new(),
            node_id: String::new(),
            uri: String::new(),
            web_uri: None,
            uris: None,
            image_count: None,
            allow_downloads: None,
        }
    }

    impl AlbumSeriesBackend for &Fake {
        async fn find_album(&self, name: &str) -> Result<Option<Album>> {
            Ok(self
                .albums
                .lock()
                .unwrap()
                .iter()
                .find(|(a, _)| a.name == name)
                .map(|(a, _)| a.clone()))
        }
        async fn image_count(&self, album: &Album) -> Result<u64> {
            Ok(self
                .albums
                .lock()
                .unwrap()
                .iter()
                .find(|(a, _)| a.name == album.name)
                .map_or(0, |(_, n)| *n))
        }
        async fn create_album(&self, name: &str) -> Result<Album> {
            let created = album(name);
            self.albums.lock().unwrap().push((created.clone(), 0));
            Ok(created)
        }
    }

    impl UrlUploader for Fake {
        async fn upload_url(
            &self,
            album_key: &str,
            url: &str,
            file_name: Option<&str>,
        ) -> Result<Option<String>> {
            if url.contains("broken") {
                bail!("400 Bad Request");
            }
            self.sent.lock().unwrap().push((
                album_key.to_string(),
                url.to_string(),
                file_name.map(str::to_string),
            ));
            Ok(Some("/api/v2/image/NEW-0".into()))
        }
    }

    #[tokio::test]
    async fn sends_urls_into_the_series_and_overflows() {
        let fake = Fake::default();
        fake.albums.lock().unwrap().push((album("Web"), 1));
        let series = AlbumSeries::load(&fake, "Web", 2, false).await.unwrap();
        let urls = vec![
            "https://x/a.jpg".to_string(),
            "https://x/broken.jpg".to_string(),
            "https://x/b".to_string(),
        ];
        let outcomes = upload_urls(&series, &fake, &urls, false).await;

        assert_eq!(outcomes[0].album.as_deref(), Some("Web"));
        assert!(outcomes[1].result.as_ref().unwrap_err().contains("400"));
        // "Web" filled up, so the failed URL claimed (and gave back) a slot
        // in a new "Web (2)", where the next one goes.
        assert_eq!(outcomes[1].album.as_deref(), Some("Web (2)"));
        assert_eq!(outcomes[2].album.as_deref(), Some("Web (2)"));
        assert_eq!(
            *fake.sent.lock().unwrap(),
            vec![
                (
                    "key-Web".to_string(),
                    "https://x/a.jpg".to_string(),
                    Some("a.jpg".to_string())
                ),
                ("key-Web (2)".to_string(), "https://x/b".to_string(), None),
            ]
        );
    }

    #[tokio::test]
    async fn dry_run_sends_and_creates_nothing() {
        let fake = Fake::default();
        let series = AlbumSeries::load(&fake, "Web", 5000, true).await.unwrap();
        let outcomes = upload_urls(&series, &fake, &["https://x/a.jpg".to_string()], true).await;
        assert_eq!(outcomes[0].album.as_deref(), Some("Web"));
        assert_eq!(outcomes[0].result, Ok(None));
        assert!(fake.sent.lock().unwrap().is_empty());
        assert!(fake.albums.lock().unwrap().is_empty());
    }

    #[test]
    fn checks_and_names_urls() {
        assert!(check_url("https://example.com/a.jpg").is_ok());
        assert!(check_url("ftp://example.com/a.jpg").is_err());
        assert!(check_url("photo.jpg").is_err());
        assert_eq!(
            file_name_from_url("https://x/p/IMG%201.JPG?w=100").as_deref(),
            Some("IMG 1.JPG")
        );
        assert_eq!(
            file_name_from_url("https://x/p/a+b.jpg").as_deref(),
            Some("a+b.jpg")
        );
        assert_eq!(file_name_from_url("https://x/photo?id=3"), None);
        assert_eq!(file_name_from_url("https://x/"), None);
    }
}
