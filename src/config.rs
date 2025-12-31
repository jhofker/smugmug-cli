use anyhow::{Context, Result};
use dialoguer::Input;
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

    let api_key: String = Input::new()
        .with_prompt("API Key")
        .interact_text()?;

    let api_secret: String = Input::new()
        .with_prompt("API Secret")
        .interact_text()?;

    let access_token: String = Input::new()
        .with_prompt("Access Token")
        .interact_text()?;

    let access_token_secret: String = Input::new()
        .with_prompt("Access Token Secret")
        .interact_text()?;

    let mut config = Config::default();
    config.auth.api_key = api_key;
    config.auth.api_secret = api_secret;
    config.auth.access_token = access_token;
    config.auth.access_token_secret = access_token_secret;

    save_config(&config)?;

    let config_path = get_config_path()?;
    println!("\n✓ Configuration saved to: {}", config_path.display());
    println!("\nYou can now upload photos using: smugmug-cli upload <path>");

    Ok(())
}

pub fn load_config() -> Result<Config> {
    // Try to load from environment variables first
    if let Ok(config) = load_from_env() {
        return Ok(config);
    }

    // Fall back to config file
    let config_path = get_config_path()?;

    if !config_path.exists() {
        anyhow::bail!(
            "Config not found. Either:\n\
             1. Run 'smugmug-cli init' to create config file at: {}\n\
             2. Set environment variables: SMUGMUG_API_KEY, SMUGMUG_API_SECRET, SMUGMUG_ACCESS_TOKEN, SMUGMUG_ACCESS_TOKEN_SECRET\n\
             3. Create a .env file with these variables",
            config_path.display()
        );
    }

    let contents = fs::read_to_string(&config_path)
        .context("Failed to read config file")?;

    let config: Config = toml::from_str(&contents)
        .context("Failed to parse config file")?;

    Ok(config)
}

fn load_from_env() -> Result<Config> {
    // Try to load .env file (silently ignore if it doesn't exist)
    let _ = dotenvy::dotenv();

    let api_key = std::env::var("SMUGMUG_API_KEY")?;
    let api_secret = std::env::var("SMUGMUG_API_SECRET")?;
    let access_token = std::env::var("SMUGMUG_ACCESS_TOKEN")?;
    let access_token_secret = std::env::var("SMUGMUG_ACCESS_TOKEN_SECRET")?;

    let mut config = Config::default();
    config.auth.api_key = api_key;
    config.auth.api_secret = api_secret;
    config.auth.access_token = access_token;
    config.auth.access_token_secret = access_token_secret;

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
