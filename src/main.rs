use clap::{Parser, Subcommand};
use anyhow::Result;

mod api;
mod cache;
mod config;
mod scanner;
mod uploader;

#[derive(Parser)]
#[command(name = "smugmug-cli")]
#[command(about = "A CLI tool for uploading photos to SmugMug", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize configuration and authenticate with SmugMug
    Init,

    /// Test authentication credentials
    TestAuth,

    /// Album management
    Albums {
        #[command(subcommand)]
        command: AlbumCommands,
    },

    /// Upload photos to SmugMug
    Upload {
        /// Path to photos directory or file
        path: String,

        /// Number of concurrent upload threads
        #[arg(short, long, default_value = "4")]
        threads: usize,

        /// Album name (creates if doesn't exist)
        #[arg(short, long)]
        album: Option<String>,

        /// Dry run - don't actually upload
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Show cache status and statistics
    Status,

    /// Cache management
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
}

#[derive(Subcommand)]
enum AlbumCommands {
    /// List all albums
    List,

    /// Create a new album
    Create {
        /// Album name
        name: String,
    },
}

#[derive(Subcommand)]
enum CacheCommands {
    /// Clear the deduplication cache
    Clear,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            println!("Initializing SmugMug CLI...");
            config::init_config().await?;
        }
        Commands::TestAuth => {
            println!("Testing authentication...");
            let cfg = config::load_config()?;

            let client = api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            );

            match client.get_auth_user().await {
                Ok(user_info) => {
                    println!("\n✓ Authentication successful!");
                    println!("\nUser info:");
                    println!("{}", serde_json::to_string_pretty(&user_info)?);
                }
                Err(e) => {
                    println!("\n✗ Authentication failed: {}", e);
                    println!("\nPlease check your credentials and run 'smugmug-cli init' again.");
                }
            }
        }
        Commands::Albums { command } => {
            let cfg = config::load_config()?;
            let client = api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            );

            match command {
                AlbumCommands::List => {
                    println!("Fetching albums...");
                    match client.list_albums().await {
                        Ok(albums) => {
                            println!("\n✓ Found {} albums:\n", albums.len());
                            for album in albums {
                                println!("  • {} (Key: {})", album.name, album.album_key);
                                println!("    URL Name: {}", album.url_name);
                                println!("    Node ID: {}", album.node_id);
                                println!();
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                        }
                    }
                }
                AlbumCommands::Create { name } => {
                    println!("Creating album: {}", name);
                    match client.create_album(&name, None).await {
                        Ok(album) => {
                            println!("\n✓ Album created successfully!");
                            println!("  Name: {}", album.name);
                            println!("  Key: {}", album.album_key);
                            println!("  URL Name: {}", album.url_name);
                            println!("  Node ID: {}", album.node_id);
                            if let Some(web_uri) = album.web_uri {
                                println!("  Web URL: {}", web_uri);
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to create album: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Upload { path, threads, album, dry_run } => {
            println!("Uploading from: {}", path);
            println!("Threads: {}", threads);
            if let Some(album_name) = album {
                println!("Album: {}", album_name);
            }
            if dry_run {
                println!("DRY RUN - no files will be uploaded");
            }
            // TODO: Implement upload
        }
        Commands::Status => {
            println!("Cache status:");
            // TODO: Implement status
        }
        Commands::Cache { command } => {
            match command {
                CacheCommands::Clear => {
                    println!("Clearing cache...");
                    cache::clear_cache()?;
                }
            }
        }
    }

    Ok(())
}
