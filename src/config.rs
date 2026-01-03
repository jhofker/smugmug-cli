use anyhow::{Context, Result};
use colored::*;
use dialoguer::{Input, Confirm};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub auth: AuthConfig,
    pub upload: UploadConfig,
    pub deduplication: DeduplicationConfig,
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
            },
            deduplication: DeduplicationConfig {
                enabled: true,
                cache_path: cache_dir.join("hashes.db"),
            },
        }
    }
}

fn get_config_path() -> Result<PathBuf> {
    let proj_dirs = directories::ProjectDirs::from("", "", "smugmug-cli")
        .context("Failed to determine config directory")?;

    let config_dir = proj_dirs.config_dir();
    fs::create_dir_all(config_dir)
        .context("Failed to create config directory")?;

    Ok(config_dir.join("config.toml"))
}

pub async fn init_config() -> Result<()> {
    println!("SmugMug CLI Configuration\n");
    println!("To get your API credentials:");
    println!("1. Go to https://api.smugmug.com/api/developer/apply");
    println!("2. Create an application to get your API Key and Secret");
    println!("3. Go to your SmugMug Account Settings > Privacy > Authorized Services");
    println!("4. Click 'token' next to your application to get your Access Token and Secret\n");

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

    // Test authentication and detect SmugMug Source
    println!("\n{}", "Testing authentication and detecting features...".cyan());

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
                        if let Some(premium_storage) = features["Response"]["Features"]["PremiumStorage"].as_bool() {
                            has_smugmug_source = premium_storage;

                            if premium_storage {
                                println!("{} {}", "✓".green().bold(), "Detected SmugMug Source subscription (PremiumStorage enabled)".green());
                                println!("  {} {}", "→".bright_cyan(), "RAW file uploads enabled".cyan());
                                println!("  {} {}", "→".bright_cyan(), "Original file downloads enabled".cyan());
                                println!("  {} {}\n", "→".bright_cyan(), "Cloud storage backup enabled".cyan());
                            } else {
                                println!("{} {}", "ℹ".cyan().bold(), "SmugMug Source not detected".cyan());
                                println!("  {}\n", "RAW file uploads will not be available".bright_black());
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
                            println!("{} {}", "⚠".yellow().bold(), "Could not auto-detect SmugMug Source".yellow());
                            has_smugmug_source = Confirm::new()
                                .with_prompt("Do you have a SmugMug Source subscription?")
                                .default(false)
                                .interact()?;
                        }
                    }
                    Err(_) => {
                        // Couldn't fetch features, ask user
                        println!("{} {}", "⚠".yellow().bold(), "Could not fetch account features".yellow());
                        has_smugmug_source = Confirm::new()
                            .with_prompt("Do you have a SmugMug Source subscription?")
                            .default(false)
                            .interact()?;
                    }
                }
            }
        }
        Err(e) => {
            println!("{} {}", "✗".red().bold(), format!("Authentication test failed: {}", e).red());
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
    println!("\n{} {}", "✓".green().bold(), format!("Configuration saved to: {}", config_path.display()).green());
    println!("\n{} {}", "You can now upload photos using:".cyan(), "smugmug-cli upload <path>".bright_white().bold());

    Ok(())
}

pub fn load_config() -> Result<Config> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        anyhow::bail!(
            "Config file not found at: {}\n\n\
             Run 'smugmug-cli init' to create your configuration.\n\
             (Tip: You can create a .env file with your credentials, and init will use them as defaults)",
            config_path.display()
        );
    }

    let contents = fs::read_to_string(&config_path)
        .context("Failed to read config file")?;

    let config: Config = toml::from_str(&contents)
        .context("Failed to parse config file")?;

    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    let config_path = get_config_path()?;

    let toml_string = toml::to_string_pretty(config)
        .context("Failed to serialize config")?;

    fs::write(&config_path, toml_string)
        .context("Failed to write config file")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

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
            },
            deduplication: DeduplicationConfig {
                enabled: false,
                cache_path: PathBuf::from("/tmp/test_cache.db"),
            },
        }
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
        assert!(config.deduplication.cache_path.to_string_lossy().ends_with("hashes.db"));
    }

    #[test]
    fn test_config_serialization_to_toml() {
        let config = create_test_config();

        let toml_string = toml::to_string_pretty(&config)
            .expect("Failed to serialize config to TOML");

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

        let config: Config = toml::from_str(toml_string)
            .expect("Failed to deserialize config from TOML");

        // Verify auth fields
        assert_eq!(config.auth.api_key, "deserialize_api_key");
        assert_eq!(config.auth.api_secret, "deserialize_api_secret");
        assert_eq!(config.auth.access_token, "deserialize_access_token");
        assert_eq!(config.auth.access_token_secret, "deserialize_access_token_secret");

        // Verify upload fields
        assert_eq!(config.upload.threads, 16);
        assert_eq!(config.upload.retry_attempts, 10);
        assert_eq!(config.upload.timeout_seconds, 1200);

        // Verify deduplication fields
        assert!(config.deduplication.enabled);
        assert_eq!(config.deduplication.cache_path, PathBuf::from("/custom/path/cache.db"));
    }

    #[test]
    fn test_config_round_trip_serialization() {
        let original_config = create_test_config();

        // Serialize to TOML
        let toml_string = toml::to_string_pretty(&original_config)
            .expect("Failed to serialize config");

        // Deserialize back to Config
        let deserialized_config: Config = toml::from_str(&toml_string)
            .expect("Failed to deserialize config");

        // Verify all fields match
        assert_eq!(original_config.auth.api_key, deserialized_config.auth.api_key);
        assert_eq!(original_config.auth.api_secret, deserialized_config.auth.api_secret);
        assert_eq!(original_config.auth.access_token, deserialized_config.auth.access_token);
        assert_eq!(original_config.auth.access_token_secret, deserialized_config.auth.access_token_secret);
        assert_eq!(original_config.upload.threads, deserialized_config.upload.threads);
        assert_eq!(original_config.upload.retry_attempts, deserialized_config.upload.retry_attempts);
        assert_eq!(original_config.upload.timeout_seconds, deserialized_config.upload.timeout_seconds);
        assert_eq!(original_config.deduplication.enabled, deserialized_config.deduplication.enabled);
        assert_eq!(original_config.deduplication.cache_path, deserialized_config.deduplication.cache_path);
    }

    #[test]
    fn test_invalid_toml_deserialization() {
        let invalid_toml = r#"
[auth]
api_key = "test"
# Missing required fields
"#;

        let result: Result<Config, _> = toml::from_str(invalid_toml);
        assert!(result.is_err(), "Should fail when required fields are missing");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with 0 threads");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with large values");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with empty strings");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with special characters");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with deduplication disabled");
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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with relative path");
        assert_eq!(config.deduplication.cache_path, PathBuf::from("relative/path/cache.db"));

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

        let config: Config = toml::from_str(toml_string)
            .expect("Should parse config with absolute path");
        assert_eq!(config.deduplication.cache_path, PathBuf::from("/absolute/path/cache.db"));
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
            },
            deduplication: DeduplicationConfig {
                enabled: true,
                cache_path: PathBuf::from("/custom/cache.db"),
            },
        };

        assert_eq!(config.auth.api_key, "test_key");
        assert_eq!(config.upload.threads, 8);
        assert!(config.deduplication.enabled);
    }
}
