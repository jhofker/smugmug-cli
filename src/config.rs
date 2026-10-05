use anyhow::{Context, Result};
use colored::*;
use dialoguer::{Confirm, Input};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub auth: AuthConfig,
    pub upload: UploadConfig,
    pub deduplication: DeduplicationConfig,
    #[serde(default)]
    pub backup: BackupConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthConfig {
    pub api_key: String,
    pub api_secret: String,
    pub access_token: String,
    pub access_token_secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadConfig {
    pub threads: usize,
    pub retry_attempts: u32,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub has_smugmug_source: bool,
    /// Folder that `upload` puts monthly albums in when no `--album` is
    /// given. Created private if it doesn't exist.
    #[serde(default = "default_upload_folder")]
    pub default_folder: String,
    /// What to do with RAW files (see `RawMode`).
    #[serde(default)]
    pub raw_mode: RawMode,
    /// Files read (hashed, dated, RAWs rendered) at once. Uploads don't
    /// wait on reads, so a couple are enough to keep them busy without
    /// thrashing spinning disks.
    #[serde(default = "default_read_threads")]
    pub read_threads: usize,
    /// What to do with the video half of a Live Photo (see
    /// `LivePhotoVideos`).
    #[serde(default)]
    pub live_photo_videos: LivePhotoVideos,
}

/// The video half of a Live Photo: a short video with the same name as a
/// photo next to it (`IMG_1234.HEIC` + `IMG_1234.MOV`). SmugMug shows it as
/// a separate video.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LivePhotoVideos {
    /// Upload it, into the same day album as its photo
    #[default]
    Upload,
    /// Leave it out
    Skip,
}

fn default_read_threads() -> usize {
    2
}

/// `smugmug-cli backup`: what to back up, where to, and how often.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// Directories to back up.
    pub sources: Vec<PathBuf>,
    /// SmugMug folder the year/month/day albums go in. Created private.
    pub folder: String,
    /// Paths to leave out, in .gitignore syntax, relative to each source
    /// (e.g. "old_backup/", "**/Thumbs.db").
    pub exclude: Vec<String>,
    /// Time between the end of one run and the start of the next (e.g.
    /// "6h", "30m", "1d"). Without it, `backup` runs once.
    pub interval: Option<String>,
    /// Overrides `upload.threads` for backups.
    pub upload_threads: Option<usize>,
    /// Overrides `upload.read_threads` for backups.
    pub read_threads: Option<usize>,
    /// Overrides `upload.live_photo_videos` for backups.
    pub live_photo_videos: Option<LivePhotoVideos>,
}

impl Default for BackupConfig {
    fn default() -> Self {
        BackupConfig {
            sources: Vec::new(),
            folder: "Backup".to_string(),
            exclude: Vec::new(),
            interval: None,
            upload_threads: None,
            read_threads: None,
            live_photo_videos: None,
        }
    }
}

/// How `upload` treats RAW files. Uploading RAW originals needs a SmugMug
/// Source subscription; without one, a JPEG rendered from the RAW (its
/// embedded camera preview) can be uploaded instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RawMode {
    /// Upload the RAW original with SmugMug Source, otherwise a rendered JPEG
    #[default]
    Auto,
    /// Always upload a rendered JPEG instead of the RAW original
    Render,
    /// Upload the RAW original (skipped without SmugMug Source)
    Original,
    /// Don't upload RAW files
    Skip,
}

/// What actually happens to RAW files in an upload, once `RawMode` is
/// weighed against the account's subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawHandling {
    Upload,
    Render,
    Skip,
}

impl RawMode {
    pub fn handling(self, has_smugmug_source: bool) -> RawHandling {
        match (self, has_smugmug_source) {
            (RawMode::Auto, true) | (RawMode::Original, true) => RawHandling::Upload,
            (RawMode::Auto, false) | (RawMode::Render, _) => RawHandling::Render,
            (RawMode::Original, false) | (RawMode::Skip, _) => RawHandling::Skip,
        }
    }
}

fn default_upload_folder() -> String {
    "Uploads".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeduplicationConfig {
    pub enabled: bool,
    pub cache_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let cache_dir = directories::ProjectDirs::from("", "", "smugmug-cli")
            .map(|dirs| dirs.cache_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".cache"));

        Config {
            auth: AuthConfig {
                api_key: String::new(),
                api_secret: String::new(),
                access_token: String::new(),
                access_token_secret: String::new(),
            },
            upload: UploadConfig {
                threads: 4,
                retry_attempts: 3,
                timeout_seconds: 300,
                has_smugmug_source: false,
                default_folder: default_upload_folder(),
                raw_mode: RawMode::default(),
                read_threads: default_read_threads(),
                live_photo_videos: LivePhotoVideos::default(),
            },
            deduplication: DeduplicationConfig {
                enabled: true,
                cache_path: cache_dir.join("hashes.db"),
            },
            backup: BackupConfig::default(),
        }
    }
}

fn get_config_path() -> Result<PathBuf> {
    let proj_dirs = directories::ProjectDirs::from("", "", "smugmug-cli")
        .context("Failed to determine config directory")?;

    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir).context("Failed to create config directory")?;

    Ok(config_dir.join("config.toml"))
}

pub async fn init_config() -> Result<()> {
    println!("SmugMug CLI Configuration\n");
    println!("To get your API credentials:");
    println!("1. Go to https://api.smugmug.com/api/developer/apply");
    println!("2. Create an application to get your API Key and Secret");
    println!("3. Sign in through your browser when prompted to get an Access Token and Secret");
    println!("   (or run 'smugmug-cli auth' later to refresh them)\n");

    // Try to load defaults from .env file (if it exists)
    let _ = dotenvy::dotenv();
    let env_api_key = std::env::var("SMUGMUG_API_KEY").ok();
    let env_api_secret = std::env::var("SMUGMUG_API_SECRET").ok();
    let env_access_token = std::env::var("SMUGMUG_ACCESS_TOKEN").ok();
    let env_access_token_secret = std::env::var("SMUGMUG_ACCESS_TOKEN_SECRET").ok();

    let api_key: String = {
        let mut input = Input::new().with_prompt("API Key");
        if let Some(default) = env_api_key {
            input = input.with_initial_text(default);
        }
        input.interact_text()?
    };

    let api_secret: String = {
        let mut input = Input::new().with_prompt("API Secret");
        if let Some(default) = env_api_secret {
            input = input.with_initial_text(default);
        }
        input.interact_text()?
    };

    let use_browser = env_access_token.is_none()
        && Confirm::new()
            .with_prompt("Sign in to SmugMug in your browser to get an access token?")
            .default(true)
            .interact()?;

    let (access_token, access_token_secret) = if use_browser {
        let pair = browser_sign_in(&api_key, &api_secret).await?;
        (pair.token, pair.secret)
    } else {
        let access_token: String = {
            let mut input = Input::new().with_prompt("Access Token");
            if let Some(default) = env_access_token {
                input = input.with_initial_text(default);
            }
            input.interact_text()?
        };

        let access_token_secret: String = {
            let mut input = Input::new().with_prompt("Access Token Secret");
            if let Some(default) = env_access_token_secret {
                input = input.with_initial_text(default);
            }
            input.interact_text()?
        };
        (access_token, access_token_secret)
    };

    // Test authentication and detect SmugMug Source
    println!(
        "\n{}",
        "Testing authentication and detecting features...".cyan()
    );

    let test_client = crate::api::SmugMugClient::new(
        api_key.clone(),
        api_secret.clone(),
        access_token.clone(),
        access_token_secret.clone(),
    );

    let mut has_smugmug_source = false;

    match test_client.get_auth_user().await {
        Ok(user_info) => {
            println!("{}", "✓ Authentication successful!".green().bold());

            // Try to fetch features to detect SmugMug Source
            if let Some(user_uri) = user_info["Response"]["User"]["Uri"].as_str() {
                match test_client.get_user_features(user_uri).await {
                    Ok(features) => {
                        // Check PremiumStorage field to detect SmugMug Source
                        if let Some(premium_storage) =
                            features["Response"]["Features"]["PremiumStorage"].as_bool()
                        {
                            has_smugmug_source = premium_storage;

                            if premium_storage {
                                println!(
                                    "{} {}",
                                    "✓".green().bold(),
                                    "Detected SmugMug Source subscription (PremiumStorage enabled)"
                                        .green()
                                );
                                println!(
                                    "  {} {}",
                                    "→".bright_cyan(),
                                    "RAW file uploads enabled".cyan()
                                );
                                println!(
                                    "  {} {}",
                                    "→".bright_cyan(),
                                    "Original file downloads enabled".cyan()
                                );
                                println!(
                                    "  {} {}\n",
                                    "→".bright_cyan(),
                                    "Cloud storage backup enabled".cyan()
                                );
                            } else {
                                println!(
                                    "{} {}",
                                    "ℹ".cyan().bold(),
                                    "SmugMug Source not detected".cyan()
                                );
                                println!(
                                    "  {}\n",
                                    "RAW files will be uploaded as JPEGs rendered from them"
                                        .bright_black()
                                );
                            }

                            // Allow user to override detection
                            let override_detection = Confirm::new()
                                .with_prompt("Override auto-detection?")
                                .default(false)
                                .interact()?;

                            if override_detection {
                                has_smugmug_source = Confirm::new()
                                    .with_prompt("Do you have a SmugMug Source subscription?")
                                    .default(has_smugmug_source)
                                    .interact()?;
                            }
                        } else {
                            // Couldn't detect, ask user
                            println!(
                                "{} {}",
                                "⚠".yellow().bold(),
                                "Could not auto-detect SmugMug Source".yellow()
                            );
                            has_smugmug_source = Confirm::new()
                                .with_prompt("Do you have a SmugMug Source subscription?")
                                .default(false)
                                .interact()?;
                        }
                    }
                    Err(_) => {
                        // Couldn't fetch features, ask user
                        println!(
                            "{} {}",
                            "⚠".yellow().bold(),
                            "Could not fetch account features".yellow()
                        );
                        has_smugmug_source = Confirm::new()
                            .with_prompt("Do you have a SmugMug Source subscription?")
                            .default(false)
                            .interact()?;
                    }
                }
            }
        }
        Err(e) => {
            println!(
                "{} {}",
                "✗".red().bold(),
                format!("Authentication test failed: {}", e).red()
            );
            println!("{}", "Please verify your credentials are correct.".yellow());
            return Err(e);
        }
    }

    let mut config = Config::default();
    config.auth.api_key = api_key;
    config.auth.api_secret = api_secret;
    config.auth.access_token = access_token;
    config.auth.access_token_secret = access_token_secret;
    config.upload.has_smugmug_source = has_smugmug_source;

    save_config(&config)?;

    let config_path = get_config_path()?;
    println!(
        "\n{} {}",
        "✓".green().bold(),
        format!("Configuration saved to: {}", config_path.display()).green()
    );
    println!(
        "\n{} {}",
        "You can now upload photos using:".cyan(),
        "smugmug-cli upload <path>".bright_white().bold()
    );

    Ok(())
}

/// Interactive OAuth sign-in: prints the authorize link, waits for the
/// verifier code SmugMug shows after approval, and returns the access token
/// pair.
async fn browser_sign_in(
    api_key: &str,
    api_secret: &str,
) -> Result<crate::api::oauth_flow::TokenPair> {
    use crate::api::oauth_flow::{
        OAuthEndpoints, authorize_url, get_access_token, get_request_token,
    };

    let endpoints = OAuthEndpoints::default();
    let request_token = get_request_token(&endpoints, api_key, api_secret).await?;

    println!(
        "\n{}",
        "Open this link, sign in, and approve access:".cyan()
    );
    println!(
        "\n  {}\n",
        authorize_url(&endpoints, &request_token)
            .bright_white()
            .bold()
    );

    let verifier: String = Input::new()
        .with_prompt("Code shown by SmugMug after approving")
        .interact_text()?;

    let pair = get_access_token(
        &endpoints,
        api_key,
        api_secret,
        &request_token,
        verifier.trim(),
    )
    .await?;
    println!("{}", "✓ Access token received".green().bold());
    Ok(pair)
}

/// `smugmug-cli auth`: obtain a new access token through the browser and
/// store it. Uses the API key/secret from the existing config, else from
/// the environment/.env, else prompts. Creates the config if missing.
pub async fn auth_command() -> Result<()> {
    let _ = dotenvy::dotenv();
    // The file alone: it's saved back below, and credentials that only came
    // from the environment mustn't end up in it.
    let existing = load_config_file().ok();

    let (api_key, api_secret) = match &existing {
        Some(cfg) if !cfg.auth.api_key.is_empty() && !cfg.auth.api_secret.is_empty() => {
            (cfg.auth.api_key.clone(), cfg.auth.api_secret.clone())
        }
        _ => {
            let key = match std::env::var("SMUGMUG_API_KEY") {
                Ok(v) if !v.is_empty() => v,
                _ => Input::new().with_prompt("API Key").interact_text()?,
            };
            let secret = match std::env::var("SMUGMUG_API_SECRET") {
                Ok(v) if !v.is_empty() => v,
                _ => Input::new().with_prompt("API Secret").interact_text()?,
            };
            (key, secret)
        }
    };

    let pair = browser_sign_in(&api_key, &api_secret).await?;

    let client = crate::api::SmugMugClient::new(
        api_key.clone(),
        api_secret.clone(),
        pair.token.clone(),
        pair.secret.clone(),
    );
    let user = client
        .get_auth_user()
        .await
        .context("Signed in, but the new access token was rejected")?;
    if let Some(nick) = user["Response"]["User"]["NickName"].as_str() {
        println!(
            "{} {}",
            "✓ Authenticated as".green().bold(),
            nick.bright_white().bold()
        );
    }

    let is_new = existing.is_none();
    let mut config = existing.unwrap_or_default();
    config.auth.api_key = api_key;
    config.auth.api_secret = api_secret;
    config.auth.access_token = pair.token;
    config.auth.access_token_secret = pair.secret;

    if is_new {
        // Same detection init does, without the interactive override.
        if let Some(user_uri) = user["Response"]["User"]["Uri"].as_str() {
            if let Ok(features) = client.get_user_features(user_uri).await {
                config.upload.has_smugmug_source =
                    features["Response"]["Features"]["PremiumStorage"]
                        .as_bool()
                        .unwrap_or(false);
            }
        }
    }

    save_config(&config)?;
    println!(
        "{} {}",
        "✓".green().bold(),
        format!("Credentials saved to: {}", get_config_path()?.display()).green()
    );
    Ok(())
}

/// The credential variables, in `AuthConfig` field order.
const CREDENTIAL_VARS: [&str; 4] = [
    "SMUGMUG_API_KEY",
    "SMUGMUG_API_SECRET",
    "SMUGMUG_ACCESS_TOKEN",
    "SMUGMUG_ACCESS_TOKEN_SECRET",
];

/// The config to run with: the config file with any credentials from the
/// environment (as a container gets them) put over its own. Without a file,
/// all four credentials must be in the environment. Never save this back:
/// use `load_config_file` for that, so environment credentials stay out of
/// the file.
pub fn load_config() -> Result<Config> {
    let file = match load_config_file() {
        Ok(config) => Some(config),
        Err(e) if get_config_path()?.exists() => return Err(e),
        Err(_) => None,
    };
    with_env_credentials(file, |name| {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    })
}

/// `config` (the file's, if there is one) with credentials from `env` put
/// over its own.
fn with_env_credentials(
    config: Option<Config>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Config> {
    let values: Vec<Option<String>> = CREDENTIAL_VARS.iter().map(|name| env(name)).collect();
    let mut config = match config {
        Some(config) => config,
        None if values.iter().all(Option::is_none) => {
            anyhow::bail!(
                "Config file not found at: {}\n\n\
                 Run 'smugmug-cli init' to create your configuration, or set {}.\n\
                 (Tip: You can create a .env file with your credentials, and init will use them as defaults)",
                get_config_path()?.display(),
                CREDENTIAL_VARS.join(", ")
            );
        }
        None => {
            let missing: Vec<&str> = CREDENTIAL_VARS
                .iter()
                .zip(&values)
                .filter(|(_, v)| v.is_none())
                .map(|(name, _)| *name)
                .collect();
            if !missing.is_empty() {
                anyhow::bail!(
                    "No config file, and these credentials aren't set: {}",
                    missing.join(", ")
                );
            }
            Config::default()
        }
    };
    let [key, secret, token, token_secret] = <[Option<String>; 4]>::try_from(values).unwrap();
    let auth = &mut config.auth;
    for (field, value) in [
        (&mut auth.api_key, key),
        (&mut auth.api_secret, secret),
        (&mut auth.access_token, token),
        (&mut auth.access_token_secret, token_secret),
    ] {
        if let Some(value) = value {
            *field = value;
        }
    }
    Ok(config)
}

/// The config file exactly as written (no environment overrides), for
/// changing and saving back.
pub fn load_config_file() -> Result<Config> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        anyhow::bail!(
            "Config file not found at: {}\n\n\
             Run 'smugmug-cli init' to create your configuration.\n\
             (Tip: You can create a .env file with your credentials, and init will use them as defaults)",
            config_path.display()
        );
    }

    let contents = fs::read_to_string(&config_path).context("Failed to read config file")?;

    toml::from_str(&contents).context("Failed to parse config file")
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;

    let toml_string = toml::to_string_pretty(config).context("Failed to serialize config")?;

    fs::write(&config_path, toml_string).context("Failed to write config file")?;

    Ok(())
}

pub fn is_auth_configured() -> bool {
    // Try to load config
    let config = match load_config() {
        Ok(cfg) => cfg,
        Err(_) => return false,
    };

    // Check if all auth fields are filled out
    !config.auth.api_key.is_empty()
        && !config.auth.api_secret.is_empty()
        && !config.auth.access_token.is_empty()
        && !config.auth.access_token_secret.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function to create a test config
    fn create_test_config() -> Config {
        Config {
            auth: AuthConfig {
                api_key: "test_api_key".to_string(),
                api_secret: "test_api_secret".to_string(),
                access_token: "test_access_token".to_string(),
                access_token_secret: "test_access_token_secret".to_string(),
            },
            upload: UploadConfig {
                threads: 8,
                retry_attempts: 5,
                timeout_seconds: 600,
                has_smugmug_source: false,
                default_folder: default_upload_folder(),
                raw_mode: RawMode::default(),
                read_threads: default_read_threads(),
                live_photo_videos: LivePhotoVideos::default(),
            },
            deduplication: DeduplicationConfig {
                enabled: false,
                cache_path: PathBuf::from("/tmp/test_cache.db"),
            },
            backup: BackupConfig::default(),
        }
    }

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn environment_credentials_override_the_file() {
        let config = with_env_credentials(
            Some(create_test_config()),
            env_of(&[("SMUGMUG_ACCESS_TOKEN", "env-token")]),
        )
        .unwrap();
        assert_eq!(config.auth.access_token, "env-token");
        assert_eq!(config.auth.api_key, create_test_config().auth.api_key);
    }

    #[test]
    fn without_a_file_all_four_credentials_are_needed() {
        let err = with_env_credentials(
            None,
            env_of(&[("SMUGMUG_API_KEY", "k"), ("SMUGMUG_ACCESS_TOKEN", "t")]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("SMUGMUG_API_SECRET"), "{}", err);
        assert!(err.contains("SMUGMUG_ACCESS_TOKEN_SECRET"), "{}", err);
        assert!(!err.contains("SMUGMUG_API_KEY,"), "{}", err);

        let config = with_env_credentials(
            None,
            env_of(&[
                ("SMUGMUG_API_KEY", "k"),
                ("SMUGMUG_API_SECRET", "s"),
                ("SMUGMUG_ACCESS_TOKEN", "t"),
                ("SMUGMUG_ACCESS_TOKEN_SECRET", "ts"),
            ]),
        )
        .unwrap();
        assert_eq!(
            (
                config.auth.api_key.as_str(),
                config.auth.api_secret.as_str(),
                config.auth.access_token.as_str(),
                config.auth.access_token_secret.as_str()
            ),
            ("k", "s", "t", "ts")
        );
    }

    #[test]
    fn without_a_file_or_credentials_it_says_how_to_set_up() {
        let err = with_env_credentials(None, env_of(&[]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("smugmug-cli init"), "{}", err);
    }

    #[test]
    fn test_raw_mode_handling() {
        assert_eq!(RawMode::Auto.handling(true), RawHandling::Upload);
        assert_eq!(RawMode::Auto.handling(false), RawHandling::Render);
        assert_eq!(RawMode::Render.handling(true), RawHandling::Render);
        assert_eq!(RawMode::Original.handling(true), RawHandling::Upload);
        assert_eq!(RawMode::Original.handling(false), RawHandling::Skip);
        assert_eq!(RawMode::Skip.handling(true), RawHandling::Skip);

        let upload: UploadConfig = toml::from_str(
            "threads = 4\nretry_attempts = 3\ntimeout_seconds = 300\nraw_mode = \"render\"",
        )
        .unwrap();
        assert_eq!(upload.raw_mode, RawMode::Render);
    }

    #[test]
    fn test_config_without_default_folder_uses_uploads() {
        // Configs written before default_folder existed must still load.
        let toml_str = r#"
[auth]
api_key = "k"
api_secret = "s"
access_token = "t"
access_token_secret = "ts"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.upload.default_folder, "Uploads");
        assert_eq!(Config::default().upload.default_folder, "Uploads");
        assert_eq!(config.upload.raw_mode, RawMode::Auto);
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();

        // Auth fields should be empty strings
        assert_eq!(config.auth.api_key, "");
        assert_eq!(config.auth.api_secret, "");
        assert_eq!(config.auth.access_token, "");
        assert_eq!(config.auth.access_token_secret, "");

        // Upload defaults
        assert_eq!(config.upload.threads, 4);
        assert_eq!(config.upload.retry_attempts, 3);
        assert_eq!(config.upload.timeout_seconds, 300);

        // Deduplication defaults
        assert!(config.deduplication.enabled);
        assert!(
            config
                .deduplication
                .cache_path
                .to_string_lossy()
                .ends_with("hashes.db")
        );
    }

    #[test]
    fn test_config_serialization_to_toml() {
        let config = create_test_config();

        let toml_string =
            toml::to_string_pretty(&config).expect("Failed to serialize config to TOML");

        // Verify the TOML contains expected keys
        assert!(toml_string.contains("api_key"));
        assert!(toml_string.contains("api_secret"));
        assert!(toml_string.contains("access_token"));
        assert!(toml_string.contains("access_token_secret"));
        assert!(toml_string.contains("threads"));
        assert!(toml_string.contains("retry_attempts"));
        assert!(toml_string.contains("timeout_seconds"));
        assert!(toml_string.contains("enabled"));
        assert!(toml_string.contains("cache_path"));

        // Verify the values are correct
        assert!(toml_string.contains("test_api_key"));
        assert!(toml_string.contains("test_api_secret"));
        assert!(toml_string.contains("8"));
        assert!(toml_string.contains("5"));
        assert!(toml_string.contains("600"));
    }

    #[test]
    fn test_config_deserialization_from_toml() {
        let toml_string = r#"
[auth]
api_key = "deserialize_api_key"
api_secret = "deserialize_api_secret"
access_token = "deserialize_access_token"
access_token_secret = "deserialize_access_token_secret"

[upload]
threads = 16
retry_attempts = 10
timeout_seconds = 1200

[deduplication]
enabled = true
cache_path = "/custom/path/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Failed to deserialize config from TOML");

        // Verify auth fields
        assert_eq!(config.auth.api_key, "deserialize_api_key");
        assert_eq!(config.auth.api_secret, "deserialize_api_secret");
        assert_eq!(config.auth.access_token, "deserialize_access_token");
        assert_eq!(
            config.auth.access_token_secret,
            "deserialize_access_token_secret"
        );

        // Verify upload fields
        assert_eq!(config.upload.threads, 16);
        assert_eq!(config.upload.retry_attempts, 10);
        assert_eq!(config.upload.timeout_seconds, 1200);

        // Verify deduplication fields
        assert!(config.deduplication.enabled);
        assert_eq!(
            config.deduplication.cache_path,
            PathBuf::from("/custom/path/cache.db")
        );
    }

    #[test]
    fn test_config_round_trip_serialization() {
        let original_config = create_test_config();

        // Serialize to TOML
        let toml_string =
            toml::to_string_pretty(&original_config).expect("Failed to serialize config");

        // Deserialize back to Config
        let deserialized_config: Config =
            toml::from_str(&toml_string).expect("Failed to deserialize config");

        // Verify all fields match
        assert_eq!(
            original_config.auth.api_key,
            deserialized_config.auth.api_key
        );
        assert_eq!(
            original_config.auth.api_secret,
            deserialized_config.auth.api_secret
        );
        assert_eq!(
            original_config.auth.access_token,
            deserialized_config.auth.access_token
        );
        assert_eq!(
            original_config.auth.access_token_secret,
            deserialized_config.auth.access_token_secret
        );
        assert_eq!(
            original_config.upload.threads,
            deserialized_config.upload.threads
        );
        assert_eq!(
            original_config.upload.retry_attempts,
            deserialized_config.upload.retry_attempts
        );
        assert_eq!(
            original_config.upload.timeout_seconds,
            deserialized_config.upload.timeout_seconds
        );
        assert_eq!(
            original_config.deduplication.enabled,
            deserialized_config.deduplication.enabled
        );
        assert_eq!(
            original_config.deduplication.cache_path,
            deserialized_config.deduplication.cache_path
        );
    }

    #[test]
    fn test_invalid_toml_deserialization() {
        let invalid_toml = r#"
[auth]
api_key = "test"
# Missing required fields
"#;

        let result: Result<Config, _> = toml::from_str(invalid_toml);
        assert!(
            result.is_err(),
            "Should fail when required fields are missing"
        );
    }

    #[test]
    fn test_invalid_type_in_toml() {
        let invalid_toml = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = "not_a_number"
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache.db"
"#;

        let result: Result<Config, _> = toml::from_str(invalid_toml);
        assert!(result.is_err(), "Should fail when field has wrong type");
    }

    #[test]
    fn test_config_with_zero_threads() {
        let toml_string = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = 0
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with 0 threads");
        assert_eq!(config.upload.threads, 0);
    }

    #[test]
    fn test_config_with_large_values() {
        let toml_string = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = 999999
retry_attempts = 4294967295
timeout_seconds = 9999999999

[deduplication]
enabled = true
cache_path = "/tmp/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with large values");
        assert_eq!(config.upload.threads, 999999);
        assert_eq!(config.upload.retry_attempts, u32::MAX);
        assert_eq!(config.upload.timeout_seconds, 9999999999);
    }

    #[test]
    fn test_config_with_empty_strings() {
        let toml_string = r#"
[auth]
api_key = ""
api_secret = ""
access_token = ""
access_token_secret = ""

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = ""
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with empty strings");
        assert_eq!(config.auth.api_key, "");
        assert_eq!(config.auth.api_secret, "");
        assert_eq!(config.auth.access_token, "");
        assert_eq!(config.auth.access_token_secret, "");
        assert_eq!(config.deduplication.cache_path, PathBuf::from(""));
    }

    #[test]
    fn test_config_with_special_characters() {
        let toml_string = r#"
[auth]
api_key = "test!@#$%^&*()_+-={}[]|:;<>?,."
api_secret = "test\nwith\nnewlines"
access_token = "test with spaces"
access_token_secret = "test'with'quotes"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache with spaces.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with special characters");
        assert!(config.auth.api_key.contains("!@#$"));
        assert!(config.auth.access_token.contains(" "));
    }

    #[test]
    fn test_deduplication_enabled_false() {
        let toml_string = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = false
cache_path = "/tmp/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with deduplication disabled");
        assert!(!config.deduplication.enabled);
    }

    #[test]
    fn test_config_path_variations() {
        // Test various path formats
        let toml_string = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "relative/path/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with relative path");
        assert_eq!(
            config.deduplication.cache_path,
            PathBuf::from("relative/path/cache.db")
        );

        // Test absolute path
        let toml_string = r#"
[auth]
api_key = "test"
api_secret = "test"
access_token = "test"
access_token_secret = "test"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/absolute/path/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with absolute path");
        assert_eq!(
            config.deduplication.cache_path,
            PathBuf::from("/absolute/path/cache.db")
        );
    }

    #[test]
    fn test_auth_config_debug() {
        let auth = AuthConfig {
            api_key: "key".to_string(),
            api_secret: "secret".to_string(),
            access_token: "token".to_string(),
            access_token_secret: "token_secret".to_string(),
        };

        let debug_string = format!("{:?}", auth);
        assert!(debug_string.contains("AuthConfig"));
        assert!(debug_string.contains("api_key"));
    }

    #[test]
    fn test_upload_config_debug() {
        let upload = UploadConfig {
            threads: 4,
            retry_attempts: 3,
            timeout_seconds: 300,
            has_smugmug_source: false,
            default_folder: default_upload_folder(),
            raw_mode: RawMode::default(),
            read_threads: 2,
            live_photo_videos: LivePhotoVideos::default(),
        };

        let debug_string = format!("{:?}", upload);
        assert!(debug_string.contains("UploadConfig"));
        assert!(debug_string.contains("threads"));
    }

    #[test]
    fn test_deduplication_config_debug() {
        let dedup = DeduplicationConfig {
            enabled: true,
            cache_path: PathBuf::from("/tmp/cache.db"),
        };

        let debug_string = format!("{:?}", dedup);
        assert!(debug_string.contains("DeduplicationConfig"));
        assert!(debug_string.contains("enabled"));
    }

    #[test]
    fn test_config_struct_creation() {
        let config = Config {
            auth: AuthConfig {
                api_key: "test_key".to_string(),
                api_secret: "test_secret".to_string(),
                access_token: "test_token".to_string(),
                access_token_secret: "test_token_secret".to_string(),
            },
            upload: UploadConfig {
                threads: 8,
                retry_attempts: 5,
                timeout_seconds: 600,
                has_smugmug_source: false,
                default_folder: default_upload_folder(),
                raw_mode: RawMode::default(),
                read_threads: default_read_threads(),
                live_photo_videos: LivePhotoVideos::default(),
            },
            deduplication: DeduplicationConfig {
                enabled: true,
                cache_path: PathBuf::from("/custom/cache.db"),
            },
            backup: BackupConfig::default(),
        };

        assert_eq!(config.auth.api_key, "test_key");
        assert_eq!(config.upload.threads, 8);
        assert!(config.deduplication.enabled);
    }

    #[test]
    fn test_is_auth_configured_with_empty_fields() {
        let toml_string = r#"
[auth]
api_key = ""
api_secret = ""
access_token = ""
access_token_secret = ""

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with empty auth fields");

        // Verify that empty strings are detected as not configured
        assert!(config.auth.api_key.is_empty());
        assert!(config.auth.api_secret.is_empty());
        assert!(config.auth.access_token.is_empty());
        assert!(config.auth.access_token_secret.is_empty());
    }

    #[test]
    fn test_is_auth_configured_with_filled_fields() {
        let toml_string = r#"
[auth]
api_key = "test_key"
api_secret = "test_secret"
access_token = "test_token"
access_token_secret = "test_token_secret"

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "/tmp/cache.db"
"#;

        let config: Config =
            toml::from_str(toml_string).expect("Should parse config with filled auth fields");

        // Verify that filled strings are properly populated
        assert!(!config.auth.api_key.is_empty());
        assert!(!config.auth.api_secret.is_empty());
        assert!(!config.auth.access_token.is_empty());
        assert!(!config.auth.access_token_secret.is_empty());
    }
}
