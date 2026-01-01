use clap::{Parser, Subcommand};
use anyhow::Result;

#[derive(Debug, Clone)]
enum UploadMode {
    SingleAlbum,
    MaintainStructure,
}

fn print_tree(node: &api::NodeTree, prefix: &str, is_last: bool) {
    // Print the current node
    let connector = if is_last { "└── " } else { "├── " };
    let type_indicator = match node.node_type.as_str() {
        "Folder" => "📁",
        "Album" => "📷",
        _ => "📄",
    };

    println!("{}{}{} {}", prefix, connector, type_indicator, node.name);

    // Prepare prefix for children
    let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

    // Print children
    let child_count = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let is_last_child = i == child_count - 1;
        print_tree(child, &child_prefix, is_last_child);
    }
}

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

        /// Parent folder path (e.g., "2024/Travel" creates album in Travel folder)
        #[arg(short, long)]
        parent: Option<String>,

        /// Dry run - don't actually upload
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Check SmugMug for existing files by MD5 hash (slower but more reliable)
        #[arg(long)]
        check_remote: bool,

        /// Disable local cache (always check files, even if previously uploaded)
        #[arg(long)]
        no_cache: bool,
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

    /// Show folder/album tree structure
    Tree,
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
                AlbumCommands::Tree => {
                    println!("Fetching folder structure...\n");
                    match client.get_node_tree().await {
                        Ok(tree) => {
                            print_tree(&tree, "", true);
                        }
                        Err(e) => {
                            println!("✗ Failed to fetch folder structure: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Upload { path, threads, album, parent, dry_run, check_remote, no_cache } => {
            let cfg = config::load_config()?;
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

            // Determine upload mode and album name
            let (upload_mode, album_name) = if let Some(name) = album {
                // Album specified via CLI, use single album mode
                (UploadMode::SingleAlbum, name)
            } else {
                // No album specified, prompt user
                use dialoguer::{Select, Input};

                println!("\nNo album specified. How would you like to upload?");
                let choices = vec![
                    "Single album - flatten all images into one album",
                    "Maintain folder structure - create albums/folders matching your directory structure",
                ];

                let selection = Select::new()
                    .items(&choices)
                    .default(0)
                    .interact()?;

                match selection {
                    0 => {
                        // Single album mode
                        let path_obj = std::path::Path::new(&path);
                        let default_name = path_obj
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Uploads")
                            .to_string();

                        let album_name: String = Input::new()
                            .with_prompt("Album name")
                            .default(default_name)
                            .interact_text()?;

                        (UploadMode::SingleAlbum, album_name)
                    }
                    1 => {
                        // Maintain structure mode
                        println!("\nFolder structure will be preserved in SmugMug.");
                        (UploadMode::MaintainStructure, String::new())
                    }
                    _ => unreachable!(),
                }
            };

            println!("Uploading from: {}", path);
            if matches!(upload_mode, UploadMode::SingleAlbum) {
                println!("Album: {}", album_name);
            }
            println!("Threads: {}", threads);
            if dry_run {
                println!("DRY RUN - no files will be uploaded\n");
            } else {
                println!();
            }

            // Get cache directory (used by both upload modes)
            let cache_path = if let Some(proj_dirs) = directories::ProjectDirs::from("com", "smugmug-cli", "smugmug-cli") {
                proj_dirs.cache_dir().to_path_buf()
            } else {
                std::path::PathBuf::from(".cache")
            };

            // Create cache directory if it doesn't exist
            std::fs::create_dir_all(&cache_path)?;

            // Handle upload based on mode
            match upload_mode {
                UploadMode::SingleAlbum => {
                    // Find or create parent folder if specified
                    let parent_node_uri = if let Some(ref parent_path) = parent {
                        println!("Finding/creating folder path: {}", parent_path);
                        match client.find_or_create_folder_path(parent_path).await {
                            Ok(uri) => {
                                println!("✓ Using folder: {}\n", parent_path);
                                Some(uri)
                            }
                            Err(e) => {
                                println!("✗ Failed to find/create folder path: {}", e);
                                return Ok(());
                            }
                        }
                    } else {
                        None
                    };

                    // Get or create the album
                    println!("Looking up album...");
                    let album = if let Some(parent_uri) = parent_node_uri.as_deref() {
                        // Parent specified, check if album exists in that folder first
                        match client.find_album_in_folder(parent_uri, &album_name).await {
                            Ok(Some(existing_album)) => {
                                println!("✓ Found existing album: {} (Key: {})", existing_album.name, existing_album.album_key);
                                if let Some(ref web_uri) = existing_album.web_uri {
                                    println!("  URL: {}", web_uri);
                                }
                                println!();
                                existing_album
                            }
                            Ok(None) => {
                                // Album doesn't exist, create it
                                match client.create_album(&album_name, Some(parent_uri)).await {
                                    Ok(album) => {
                                        println!("✓ Created album: {} (Key: {})", album.name, album.album_key);
                                        if let Some(ref web_uri) = album.web_uri {
                                            println!("  URL: {}", web_uri);
                                        }
                                        println!();
                                        album
                                    }
                                    Err(e) => {
                                        println!("✗ Failed to create album: {}", e);
                                        return Ok(());
                                    }
                                }
                            }
                            Err(e) => {
                                println!("✗ Failed to search for album: {}", e);
                                return Ok(());
                            }
                        }
                    } else {
                        // No parent specified, use root and check for existing
                        match client.get_or_create_album(&album_name).await {
                            Ok(album) => {
                                println!("✓ Using album: {} (Key: {})", album.name, album.album_key);
                                if let Some(ref web_uri) = album.web_uri {
                                    println!("  URL: {}", web_uri);
                                }
                                println!();
                                album
                            }
                            Err(e) => {
                                println!("✗ Failed to get/create album: {}", e);
                                return Ok(());
                            }
                        }
                    };

                    // Set up upload options
                    let upload_options = uploader::UploadOptions {
                        path: std::path::PathBuf::from(path),
                        album,
                        client,
                        threads,
                        dry_run,
                        check_remote,
                        no_cache,
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
                UploadMode::MaintainStructure => {
                    // Upload with folder structure preservation
                    match uploader::upload_with_structure(uploader::UploadStructureOptions {
                        path: std::path::PathBuf::from(path),
                        client: client.clone(),
                        dry_run,
                        check_remote,
                        no_cache,
                        cache_path,
                    }).await {
                        Ok(stats) => {
                            println!("\n✓ Upload complete!");
                            println!("  Total files: {}", stats.total_files);
                            println!("  Uploaded: {}", stats.uploaded);
                            println!("  Skipped (duplicates): {}", stats.skipped);
                            println!("  Failed: {}", stats.failed);
                            println!("  Folders created: {}", stats.folders_created);
                            println!("  Albums created: {}", stats.albums_created);
                            println!("  Total size: {:.2} MB", stats.total_bytes as f64 / 1024.0 / 1024.0);
                        }
                        Err(e) => {
                            println!("\n✗ Upload failed: {}", e);
                        }
                    }
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
