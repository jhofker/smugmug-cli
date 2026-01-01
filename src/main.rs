use clap::{Parser, Subcommand};
use anyhow::Result;

mod api;
mod cache;
mod config;
mod downloader;
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

        /// Check SmugMug for existing files by MD5 hash (slower but more reliable)
        #[arg(long)]
        check_remote: bool,
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

    /// Download all images from an album
    Download {
        /// Album name or key
        album: String,

        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: String,

        /// Number of concurrent download threads
        #[arg(short, long, default_value = "4")]
        threads: usize,
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
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

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
                AlbumCommands::Download { album, output, threads } => {
                    println!("Downloading album: {}", album);
                    println!("Output directory: {}", output);
                    println!("Threads: {}\n", threads);

                    // Try to find album by name or use as key
                    let album_key = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums.iter().find(|a| a.name == album || a.album_key == album) {
                                found.album_key.clone()
                            } else {
                                println!("✗ Album not found: {}", album);
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    let download_options = downloader::DownloadOptions {
                        album_key,
                        output_dir: std::path::PathBuf::from(output),
                        client,
                        threads,
                    };

                    match downloader::download_album(download_options).await {
                        Ok(stats) => {
                            println!("\n✓ Download complete!");
                            println!("  Total images: {}", stats.total_images);
                            println!("  Downloaded: {}", stats.downloaded);
                            println!("  Failed: {}", stats.failed);
                            println!("  Total size: {:.2} MB", stats.total_bytes as f64 / 1024.0 / 1024.0);
                        }
                        Err(e) => {
                            println!("\n✗ Download failed: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Upload { path, threads, album, dry_run, check_remote } => {
            let cfg = config::load_config()?;
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

            // Determine album name
            let album_name = match album {
                Some(name) => name,
                None => {
                    println!("Error: Album name is required for upload");
                    println!("Usage: smugmug-cli upload <path> --album <album-name>");
                    return Ok(());
                }
            };

            println!("Uploading from: {}", path);
            println!("Album: {}", album_name);
            println!("Threads: {}", threads);
            if dry_run {
                println!("DRY RUN - no files will be uploaded\n");
            } else {
                println!();
            }

            // Get or create the album
            println!("Looking up album...");
            let album = match client.get_or_create_album(&album_name).await {
                Ok(album) => {
                    println!("✓ Using album: {} (Key: {})\n", album.name, album.album_key);
                    album
                }
                Err(e) => {
                    println!("✗ Failed to get/create album: {}", e);
                    return Ok(());
                }
            };

            // Get cache directory
            let cache_path = if let Some(proj_dirs) = directories::ProjectDirs::from("com", "smugmug-cli", "smugmug-cli") {
                proj_dirs.cache_dir().to_path_buf()
            } else {
                std::path::PathBuf::from(".cache")
            };

            // Create cache directory if it doesn't exist
            std::fs::create_dir_all(&cache_path)?;

            // Set up upload options
            let upload_options = uploader::UploadOptions {
                path: std::path::PathBuf::from(path),
                album,
                client,
                threads,
                dry_run,
                check_remote,
                cache_path,
            };

            // Perform upload
            match uploader::upload_files(upload_options).await {
                Ok(stats) => {
                    println!("\n✓ Upload complete!");
                    println!("  Total files: {}", stats.total_files);
                    println!("  Uploaded: {}", stats.uploaded);
                    println!("  Skipped (duplicates): {}", stats.skipped);
                    println!("  Failed: {}", stats.failed);
                    println!("  Total size: {:.2} MB", stats.total_bytes as f64 / 1024.0 / 1024.0);
                }
                Err(e) => {
                    println!("\n✗ Upload failed: {}", e);
                }
            }
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
