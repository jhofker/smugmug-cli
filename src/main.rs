use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;

#[derive(Debug, Clone)]
enum UploadMode {
    SingleAlbum,
    MaintainStructure,
}

fn print_logo() {
    println!(
        "{}",
        r#"
 ____                        __  __              ____ _     ___
/ ___| _ __ ___  _   _  ___ |  \/  |_   _  __ _ / ___| |   |_ _|
\___ \| '_ ` _ \| | | |/ _ `| |\/| | | | |/ _` | |   | |    | |
 ___) | | | | | | |_| | (_| | |  | | |_| | (_| | |___| |___ | |
|____/|_| |_| |_|\__,_|\__, |_|  |_|\__,_|\__, |\____|_____|___|
                       |___/              |___/
    "#
        .bright_cyan()
        .bold()
    );
}

// Helper functions for consistent colored output
fn success(msg: &str) -> String {
    format!("{} {}", "✓".green().bold(), msg.green())
}

fn error(msg: &str) -> String {
    format!("{} {}", "✗".red().bold(), msg.red())
}

fn warning(msg: &str) -> String {
    format!("{} {}", "⚠".yellow().bold(), msg.yellow())
}

fn info(msg: &str) -> String {
    msg.cyan().to_string()
}

fn highlight(msg: &str) -> String {
    msg.bright_white().bold().to_string()
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

/// Format a number with thousands separators
fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }

    result
}

/// Format bytes into a human-readable string
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

/// Format duration in seconds into a human-readable string
fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, secs)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
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

    /// Sign in through your browser to get (or refresh) an access token
    Auth,

    /// Test authentication credentials
    TestAuth,

    /// Album management
    Albums {
        #[command(subcommand)]
        command: AlbumCommands,
    },

    /// Image management
    Images {
        #[command(subcommand)]
        command: ImageCommands,
    },

    /// Comment management
    Comments {
        #[command(subcommand)]
        command: CommentCommands,
    },

    /// Upload photos to SmugMug
    Upload {
        /// Path to photos directory or file
        path: String,

        /// Number of concurrent upload threads
        #[arg(short, long, default_value = "4")]
        threads: usize,

        /// Album name (creates if doesn't exist). Without it, files go to a
        /// monthly album (e.g. "2026-09") in the configured default folder
        /// ("Uploads" unless changed), or in --parent if given
        #[arg(short, long)]
        album: Option<String>,

        /// Parent folder path (e.g., "2024/Travel" creates album in Travel folder)
        #[arg(short, long)]
        parent: Option<String>,

        /// Recreate the directory structure as SmugMug folders and albums
        #[arg(long, conflicts_with_all = ["album", "parent", "interactive"])]
        structure: bool,

        /// Ask how to upload (single album or folder structure) instead of
        /// using the default destination
        #[arg(short, long, conflicts_with = "album")]
        interactive: bool,

        /// Dry run - don't actually upload
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Also match existing SmugMug images by content hash regardless of
        /// filename, to skip re-uploading the same content under a
        /// different name (in addition to the default same-filename
        /// skip/replace behavior, which always runs)
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

    /// Raw API probes for exploring undocumented endpoints
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
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

        /// Privacy level (private, unlisted, public) - defaults to private
        #[arg(long, default_value = "private")]
        privacy: String,
    },

    /// Delete an album
    Delete {
        /// Album name or key
        album: String,

        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
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

    /// Update album settings
    Settings {
        /// Album name or key
        album: String,

        /// Set privacy (public, unlisted, private)
        #[arg(long)]
        privacy: Option<String>,

        /// Set description
        #[arg(long)]
        description: Option<String>,

        /// Set keywords (semicolon-separated)
        #[arg(long)]
        keywords: Option<String>,

        /// Set sort method (Position, Caption, FileName, DateTimeOriginal, DateTimeUploaded)
        #[arg(long)]
        sort_method: Option<String>,

        /// Set sort direction (asc, desc)
        #[arg(long)]
        sort_direction: Option<String>,
    },

    /// Get album download link (ZIP file)
    GetDownloadLink {
        /// Album name or key
        album: String,

        /// Wait for download generation (polls until ready)
        #[arg(short, long)]
        wait: bool,
    },
}

#[derive(Subcommand)]
enum DebugCommands {
    /// Authenticated GET of an API path (e.g. "/api/v2!authuser"); prints status and raw JSON
    Get {
        /// API path or absolute URL, query string allowed
        path: String,

        /// HTTP method to send instead of GET (e.g. OPTIONS)
        #[arg(short = 'X', long, default_value = "GET")]
        method: String,
    },

    /// Upload one file to the Library (no album); prints status and raw JSON
    LibraryUpload {
        /// File to upload
        file: String,

        /// Value for the Filepath field (defaults to the file name)
        #[arg(long)]
        filepath: Option<String>,
    },
}

#[derive(Subcommand)]
enum CacheCommands {
    /// Clear the deduplication cache
    Clear,
}

#[derive(Subcommand)]
enum ImageCommands {
    /// List images in an album
    List {
        /// Album name or key
        album: String,
    },

    /// Show detailed information about an image
    Info {
        /// Album name or key
        album: String,

        /// Image key to view
        image_key: String,
    },

    /// Delete an image
    Delete {
        /// Album name or key
        album: String,

        /// Image key to delete
        image_key: String,

        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Update image metadata
    Update {
        /// Album name or key
        album: String,

        /// Image key to update
        image_key: String,

        /// Set image caption
        #[arg(long)]
        caption: Option<String>,

        /// Set image title
        #[arg(long)]
        title: Option<String>,

        /// Set keywords (semicolon-separated)
        #[arg(long)]
        keywords: Option<String>,

        /// Set latitude
        #[arg(long)]
        latitude: Option<f64>,

        /// Set longitude
        #[arg(long)]
        longitude: Option<f64>,
    },

    /// Move image to a different album
    Move {
        /// Source album name or key
        source_album: String,

        /// Image key to move
        image_key: String,

        /// Target album name or key
        target_album: String,

        /// Force move without confirmation
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum CommentCommands {
    /// List comments on an image
    List {
        /// Album name or key
        album: String,

        /// Image key
        image_key: String,
    },

    /// Create a new comment on an image
    Create {
        /// Album name or key
        album: String,

        /// Image key
        image_key: String,

        /// Comment text
        #[arg(short, long)]
        text: String,

        /// Commenter name
        #[arg(short, long)]
        name: Option<String>,

        /// Commenter email
        #[arg(short, long)]
        email: Option<String>,

        /// Rating (0-5)
        #[arg(short, long)]
        rating: Option<u8>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    print_logo();
    let cli = Cli::parse();

    // Check if auth is configured for commands that require it
    if !matches!(cli.command, Commands::Init | Commands::Auth) && !config::is_auth_configured() {
        println!("\n{}", warning("Authentication not configured!"));
        println!(
            "\n{}",
            info("To get started, you'll need to set up your SmugMug API credentials.")
        );
        println!("\n{}", highlight("Run the following command to configure:"));
        println!("  {}\n", "smugmug-cli init".bright_white().bold());

        println!("{}", info("You'll need:"));
        println!(
            "  • API Key and Secret from {}",
            "https://api.smugmug.com/api/developer/apply".bright_cyan()
        );
        println!("  • Access Token and Secret from your SmugMug Account Settings\n");

        return Ok(());
    }

    match cli.command {
        Commands::Init => {
            println!("Initializing SmugMug CLI...");
            config::init_config().await?;
        }
        Commands::Auth => {
            config::auth_command().await?;
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
                    println!("\n{}", success("Authentication successful!"));
                    println!("\n{}", info("User info:"));
                    println!("{}", serde_json::to_string_pretty(&user_info)?);

                    // Try to fetch features
                    if let Some(user_uri) = user_info["Response"]["User"]["Uri"].as_str() {
                        println!("\n{}", info("Fetching user features..."));
                        match client.get_user_features(user_uri).await {
                            Ok(features) => {
                                println!("\n{}", info("User features:"));
                                println!("{}", serde_json::to_string_pretty(&features)?);
                            }
                            Err(e) => {
                                println!(
                                    "\n{}",
                                    error(&format!("Failed to fetch features: {}", e))
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("\n{}", error(&format!("Authentication failed: {}", e)));
                    println!(
                        "\n{}",
                        warning("Please check your credentials and run 'smugmug-cli init' again.")
                    );
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
                AlbumCommands::Create { name, privacy } => {
                    // Validate and normalize privacy value
                    let normalized_privacy = match privacy.to_lowercase().as_str() {
                        "public" => "Public",
                        "unlisted" => "Unlisted",
                        "private" => "Private",
                        _ => {
                            println!(
                                "\n{}",
                                error(&format!("Invalid privacy value: {}", privacy))
                            );
                            println!("  Valid values: public, unlisted, private");
                            return Ok(());
                        }
                    };

                    println!(
                        "Creating album: {} ({})",
                        name,
                        normalized_privacy.to_lowercase()
                    );
                    match client.create_album(&name, None, normalized_privacy).await {
                        Ok(album) => {
                            println!("\n{}", success("Album created successfully!"));
                            println!("  Name: {}", album.name);
                            println!("  Privacy: {}", normalized_privacy);
                            println!("  Key: {}", album.album_key);
                            println!("  URL Name: {}", album.url_name);
                            println!("  Node ID: {}", album.node_id);
                            if let Some(web_uri) = album.web_uri {
                                println!("  Web URL: {}", web_uri);
                            }
                        }
                        Err(e) => {
                            println!("\n{}", error(&format!("Failed to create album: {}", e)));
                        }
                    }
                }
                AlbumCommands::Delete { album, force } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let (album_key, album_info) = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                (found.album_key.clone(), Some(found.clone()))
                            } else {
                                // Album not found by name, try using the input as key directly
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Show album details
                    if let Some(info) = album_info {
                        println!("\n Album to delete:");
                        println!("  Name: {}", info.name);
                        println!("  Key: {}", info.album_key);
                        if let Some(web_uri) = &info.web_uri {
                            println!("  URL: {}", web_uri);
                        }
                    }

                    // Confirm deletion unless --force is used
                    if !force {
                        use dialoguer::Confirm;

                        let confirmed = Confirm::new()
                            .with_prompt("\nAre you sure you want to delete this album? This cannot be undone.")
                            .default(false)
                            .interact()?;

                        if !confirmed {
                            println!("\n Deletion cancelled.");
                            return Ok(());
                        }
                    }

                    // Delete the album
                    println!("\nDeleting album...");
                    match client.delete_album(&album_key).await {
                        Ok(()) => {
                            println!("\n✓ Album deleted successfully!");
                        }
                        Err(e) => {
                            println!("\n✗ Failed to delete album: {}", e);
                        }
                    }
                }
                AlbumCommands::Download {
                    album,
                    output,
                    threads,
                } => {
                    println!("Downloading album: {}", album);
                    println!("Output directory: {}", output);
                    println!("Threads: {}\n", threads);

                    // Try to find album by name or use as key
                    let album_key = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
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
                            println!(
                                "  Total size: {:.2} MB",
                                stats.total_bytes as f64 / 1024.0 / 1024.0
                            );
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
                AlbumCommands::Settings {
                    album,
                    privacy,
                    description,
                    keywords,
                    sort_method,
                    sort_direction,
                } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let (album_key, album_info) = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                (found.album_key.clone(), Some(found.clone()))
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Check if any settings were provided
                    if privacy.is_none()
                        && description.is_none()
                        && keywords.is_none()
                        && sort_method.is_none()
                        && sort_direction.is_none()
                    {
                        println!("\n✗ No settings specified to update");
                        println!(
                            "  Use --privacy, --description, --keywords, --sort-method, or --sort-direction"
                        );
                        return Ok(());
                    }

                    // Validate and normalize privacy value
                    let privacy = if let Some(p) = privacy {
                        let normalized = match p.to_lowercase().as_str() {
                            "public" => "Public",
                            "unlisted" => "Unlisted",
                            "private" => "Private",
                            _ => {
                                println!("\n✗ Invalid privacy value: {}", p);
                                println!("  Valid values: public, unlisted, private");
                                return Ok(());
                            }
                        };
                        Some(normalized.to_string())
                    } else {
                        None
                    };

                    // Validate and normalize sort direction
                    let sort_direction = if let Some(d) = sort_direction {
                        let normalized = match d.to_lowercase().as_str() {
                            "asc" | "ascending" => "Ascending",
                            "desc" | "descending" => "Descending",
                            _ => {
                                println!("\n✗ Invalid sort direction: {}", d);
                                println!("  Valid values: asc, desc");
                                return Ok(());
                            }
                        };
                        Some(normalized.to_string())
                    } else {
                        None
                    };

                    // Show current album info
                    if let Some(info) = album_info {
                        println!("\nAlbum: {}", info.name);
                        println!("  Key: {}", info.album_key);
                        if let Some(web_uri) = &info.web_uri {
                            println!("  URL: {}", web_uri);
                        }
                    }

                    // Show what will be updated
                    println!("\nSettings to update:");
                    if let Some(ref p) = privacy {
                        println!("  Privacy: {}", p);
                    }
                    if let Some(ref d) = description {
                        println!("  Description: {}", d);
                    }
                    if let Some(ref k) = keywords {
                        println!("  Keywords: {}", k);
                    }
                    if let Some(ref sm) = sort_method {
                        println!("  Sort Method: {}", sm);
                    }
                    if let Some(ref sd) = sort_direction {
                        println!("  Sort Direction: {}", sd);
                    }

                    // Create settings update struct
                    let settings = api::albums::AlbumSettingsUpdate {
                        privacy,
                        description,
                        keywords,
                        sort_method,
                        sort_direction,
                    };

                    println!("\nUpdating album settings...");
                    match client.update_album_settings(&album_key, settings).await {
                        Ok(()) => {
                            println!("\n✓ Album settings updated successfully!");
                        }
                        Err(e) => {
                            println!("\n✗ Failed to update album settings: {}", e);
                        }
                    }
                }
                AlbumCommands::GetDownloadLink { album, wait } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    if wait {
                        println!("Requesting album download...");
                        println!("(This may take a few minutes for large albums)\n");

                        match client.get_album_download_link(&album_key).await {
                            Ok(url) => {
                                println!("\n✓ Download Ready!");
                                println!("  URL: {}", url);
                                println!("\nDownload with:");
                                println!("  wget \"{}\"", url);
                                println!("  curl -O \"{}\"", url);
                            }
                            Err(e) => {
                                println!("\n✗ Failed to get download link: {}", e);
                            }
                        }
                    } else {
                        println!("Requesting album download...\n");

                        match client.request_album_download(&album_key).await {
                            Ok(info) => {
                                println!("✓ Download requested successfully!");
                                if let Some(status) = &info.status {
                                    println!("  Status: {}", status);
                                }
                                if let Some(uri) = &info.uri {
                                    println!("  Status URI: {}", uri);
                                }
                                println!("\nTo wait for completion, use:");
                                println!(
                                    "  smugmug-cli albums get-download-link \"{}\" --wait",
                                    album
                                );
                            }
                            Err(e) => {
                                println!("\n✗ Failed to request download: {}", e);
                            }
                        }
                    }
                }
            }
        }
        Commands::Images { command } => {
            let cfg = config::load_config()?;
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

            match command {
                ImageCommands::List { album } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    println!("Fetching images from album...\n");
                    match client.list_album_images(&album_key).await {
                        Ok(images) => {
                            if images.is_empty() {
                                println!("No images found in this album.");
                            } else {
                                println!("✓ Found {} images:\n", format_number(images.len()));
                                for image in images {
                                    println!("  Image Key: {}", image.image_key);
                                    println!("    File: {}", image.file_name);
                                    println!("    Size: {}", format_size(image.file_size));
                                    println!("    Format: {}", image.format);
                                    if let Some(title) = &image.title {
                                        println!("    Title: {}", title);
                                    }
                                    if let Some(md5) = &image.archived_md5 {
                                        println!("    MD5: {}", md5);
                                    }
                                    println!();
                                }
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list images: {}", e);
                        }
                    }
                }
                ImageCommands::Info { album, image_key } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key_resolved = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the album
                    println!("Verifying image exists in album...");
                    let image_exists = match client.list_album_images(&album_key_resolved).await {
                        Ok(images) => images.iter().any(|img| img.image_key == image_key),
                        Err(e) => {
                            println!("\n✗ Failed to verify image: {}", e);
                            return Ok(());
                        }
                    };

                    if !image_exists {
                        println!("\n✗ Image not found in album: {}", image_key);
                        println!(
                            "  Use 'smugmug-cli images list {}' to see available images",
                            album
                        );
                        return Ok(());
                    }

                    // Fetch detailed image information
                    println!("Fetching image details...\n");
                    match client.get_image_details(&image_key).await {
                        Ok(details) => {
                            println!("✓ Image Details\n");
                            println!("Basic Information:");
                            println!("  Image Key:     {}", details.image_key);
                            println!("  File Name:     {}", details.file_name);
                            println!("  Format:        {}", details.format);
                            println!("  File Size:     {}", format_size(details.file_size));
                            println!();

                            println!("Metadata:");
                            println!(
                                "  Title:         {}",
                                details.title.as_deref().unwrap_or("N/A")
                            );
                            println!(
                                "  Caption:       {}",
                                details.caption.as_deref().unwrap_or("N/A")
                            );
                            println!(
                                "  Keywords:      {}",
                                details.keywords.as_deref().unwrap_or("N/A")
                            );
                            println!();

                            println!("Location:");
                            if let Some(lat) = details.latitude {
                                println!("  Latitude:      {}", lat);
                            } else {
                                println!("  Latitude:      N/A");
                            }
                            if let Some(lon) = details.longitude {
                                println!("  Longitude:     {}", lon);
                            } else {
                                println!("  Longitude:     N/A");
                            }
                            if let Some(alt) = details.altitude {
                                println!("  Altitude:      {} m", alt);
                            } else {
                                println!("  Altitude:      N/A");
                            }
                            println!();

                            println!("Technical:");
                            println!("  Archived URI:  {}", details.archived_uri);
                            println!(
                                "  Archived MD5:  {}",
                                details.archived_md5.as_deref().unwrap_or("N/A")
                            );
                            println!(
                                "  Upload Key:    {}",
                                details.upload_key.as_deref().unwrap_or("N/A")
                            );
                            println!("  API URI:       {}", details.uri);
                            if let Some(web_uri) = &details.web_uri {
                                println!("  Web URL:       {}", web_uri);
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to fetch image details: {}", e);
                        }
                    }
                }
                ImageCommands::Delete {
                    album,
                    image_key,
                    force,
                } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key_resolved = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the album
                    println!("Verifying image exists...");
                    let image_info = match client.list_album_images(&album_key_resolved).await {
                        Ok(images) => {
                            if let Some(found) =
                                images.iter().find(|img| img.image_key == image_key)
                            {
                                found.clone()
                            } else {
                                println!("\n✗ Image not found in album: {}", image_key);
                                println!(
                                    "  Use 'smugmug-cli images list {}' to see available images",
                                    album
                                );
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list images: {}", e);
                            return Ok(());
                        }
                    };

                    // Show confirmation prompt unless --force is specified
                    if !force {
                        use dialoguer::Confirm;

                        println!("\nImage details:");
                        println!("  File: {}", image_info.file_name);
                        println!("  Key: {}", image_info.image_key);
                        println!("  Size: {}", format_size(image_info.file_size));
                        println!("  Format: {}", image_info.format);

                        let confirmed = Confirm::new()
                            .with_prompt("Are you sure you want to delete this image?")
                            .default(false)
                            .interact()?;

                        if !confirmed {
                            println!("\nDeletion cancelled.");
                            return Ok(());
                        }
                    }

                    println!("\nDeleting image...");
                    match client.delete_image(&image_key).await {
                        Ok(()) => {
                            println!("\n✓ Image deleted successfully!");
                        }
                        Err(e) => {
                            println!("\n✗ Failed to delete image: {}", e);
                        }
                    }
                }
                ImageCommands::Update {
                    album,
                    image_key,
                    caption,
                    title,
                    keywords,
                    latitude,
                    longitude,
                } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key_resolved = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the album
                    println!("Verifying image exists...");
                    let image_info = match client.list_album_images(&album_key_resolved).await {
                        Ok(images) => {
                            if let Some(found) =
                                images.iter().find(|img| img.image_key == image_key)
                            {
                                found.clone()
                            } else {
                                println!("\n✗ Image not found in album: {}", image_key);
                                println!(
                                    "  Use 'smugmug-cli images list {}' to see available images",
                                    album
                                );
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list images: {}", e);
                            return Ok(());
                        }
                    };

                    // Check if any updates were provided
                    if caption.is_none()
                        && title.is_none()
                        && keywords.is_none()
                        && latitude.is_none()
                        && longitude.is_none()
                    {
                        println!("\n✗ No metadata fields specified to update");
                        println!(
                            "  Use --caption, --title, --keywords, --latitude, or --longitude"
                        );
                        return Ok(());
                    }

                    // Show current image info
                    println!("\nImage details:");
                    println!("  File: {}", image_info.file_name);
                    println!("  Key: {}", image_info.image_key);
                    println!("  Size: {}", format_size(image_info.file_size));
                    println!("  Format: {}", image_info.format);

                    // Show what will be updated
                    println!("\nMetadata updates:");
                    if let Some(ref c) = caption {
                        println!("  Caption: {}", c);
                    }
                    if let Some(ref t) = title {
                        println!("  Title: {}", t);
                    }
                    if let Some(ref k) = keywords {
                        println!("  Keywords: {}", k);
                    }
                    if let Some(lat) = latitude {
                        println!("  Latitude: {}", lat);
                    }
                    if let Some(lon) = longitude {
                        println!("  Longitude: {}", lon);
                    }

                    // Create metadata update struct
                    let metadata = api::images::ImageMetadataUpdate {
                        caption,
                        title,
                        keywords,
                        latitude,
                        longitude,
                    };

                    println!("\nUpdating image metadata...");
                    match client.update_image_metadata(&image_key, metadata).await {
                        Ok(()) => {
                            println!("\n✓ Image metadata updated successfully!");
                        }
                        Err(e) => {
                            println!("\n✗ Failed to update image metadata: {}", e);
                        }
                    }
                }
                ImageCommands::Move {
                    source_album,
                    image_key,
                    target_album,
                    force,
                } => {
                    println!("Looking up source album: {}", source_album);

                    // Try to find source album by name or use as key
                    let (source_album_key, source_album_name) = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == source_album || a.album_key == source_album)
                            {
                                (found.album_key.clone(), found.name.clone())
                            } else {
                                println!("\n✗ Source album not found: {}", source_album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the source album
                    println!("Verifying image exists in source album...");
                    let image_info = match client.list_album_images(&source_album_key).await {
                        Ok(images) => {
                            if let Some(found) =
                                images.iter().find(|img| img.image_key == image_key)
                            {
                                found.clone()
                            } else {
                                println!("\n✗ Image not found in source album: {}", image_key);
                                println!(
                                    "  Use 'smugmug-cli images list {}' to see available images",
                                    source_album
                                );
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list images in source album: {}", e);
                            return Ok(());
                        }
                    };

                    // Try to find target album by name or use as key
                    println!("Looking up target album: {}", target_album);
                    let (target_album_key, target_album_name) = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == target_album || a.album_key == target_album)
                            {
                                (found.album_key.clone(), found.name.clone())
                            } else {
                                println!("\n✗ Target album not found: {}", target_album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Show confirmation prompt unless --force is specified
                    if !force {
                        use dialoguer::Confirm;

                        println!("\nImage details:");
                        println!("  File: {}", image_info.file_name);
                        println!("  Key: {}", image_info.image_key);
                        println!("  Size: {}", format_size(image_info.file_size));
                        println!("  Format: {}", image_info.format);
                        if let Some(title) = &image_info.title {
                            println!("  Title: {}", title);
                        }
                        println!();
                        println!("Move from:");
                        println!("  Album: {} (Key: {})", source_album_name, source_album_key);
                        println!("To:");
                        println!("  Album: {} (Key: {})", target_album_name, target_album_key);

                        let confirmed = Confirm::new()
                            .with_prompt("Are you sure you want to move this image?")
                            .default(false)
                            .interact()?;

                        if !confirmed {
                            println!("\nMove cancelled.");
                            return Ok(());
                        }
                    }

                    println!("\nMoving image...");
                    match client.move_image(&image_key, &target_album_key).await {
                        Ok(()) => {
                            println!("\n✓ Image moved successfully!");
                            println!("  From: {}", source_album_name);
                            println!("  To: {}", target_album_name);
                        }
                        Err(e) => {
                            println!("\n✗ Failed to move image: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Comments { command } => {
            let cfg = config::load_config()?;
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

            match command {
                CommentCommands::List { album, image_key } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key_resolved = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the album
                    println!("Verifying image exists...");
                    let image_exists = match client.list_album_images(&album_key_resolved).await {
                        Ok(images) => images.iter().any(|img| img.image_key == image_key),
                        Err(e) => {
                            println!("\n✗ Failed to verify image: {}", e);
                            return Ok(());
                        }
                    };

                    if !image_exists {
                        println!("\n✗ Image not found in album: {}", image_key);
                        println!(
                            "  Use 'smugmug-cli images list {}' to see available images",
                            album
                        );
                        return Ok(());
                    }

                    // List comments
                    println!("Fetching comments...\n");
                    match client.list_image_comments(&image_key).await {
                        Ok(comments) => {
                            if comments.is_empty() {
                                println!("No comments found for this image.");
                            } else {
                                println!(
                                    "✓ Found {} comments for image {}:\n",
                                    format_number(comments.len()),
                                    image_key
                                );
                                for comment in comments {
                                    if let Some(key) = &comment.comment_key {
                                        println!("  Comment Key: {}", key);
                                    }
                                    if let Some(name) = &comment.name {
                                        println!("  Author:      {}", name);
                                    } else {
                                        println!("  Author:      Anonymous");
                                    }
                                    if let Some(rating) = comment.rating {
                                        let stars = "★".repeat(rating as usize)
                                            + &"☆".repeat((5 - rating) as usize);
                                        println!("  Rating:      {} ({}/5)", stars, rating);
                                    }
                                    println!("  Text:        {}", comment.text);
                                    if let Some(date) = &comment.date {
                                        println!("  Date:        {}", date);
                                    }
                                    println!();
                                }
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list comments: {}", e);
                        }
                    }
                }
                CommentCommands::Create {
                    album,
                    image_key,
                    text,
                    name,
                    email,
                    rating,
                } => {
                    println!("Looking up album: {}", album);

                    // Try to find album by name or use as key
                    let album_key_resolved = match client.list_albums().await {
                        Ok(albums) => {
                            if let Some(found) = albums
                                .iter()
                                .find(|a| a.name == album || a.album_key == album)
                            {
                                found.album_key.clone()
                            } else {
                                println!("\n✗ Album not found: {}", album);
                                println!("  Use 'smugmug-cli albums list' to see available albums");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list albums: {}", e);
                            return Ok(());
                        }
                    };

                    // Verify the image exists in the album
                    println!("Verifying image exists...");
                    let image_info = match client.list_album_images(&album_key_resolved).await {
                        Ok(images) => {
                            if let Some(found) =
                                images.iter().find(|img| img.image_key == image_key)
                            {
                                found.clone()
                            } else {
                                println!("\n✗ Image not found in album: {}", image_key);
                                println!(
                                    "  Use 'smugmug-cli images list {}' to see available images",
                                    album
                                );
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to list images: {}", e);
                            return Ok(());
                        }
                    };

                    // Show image details and comment preview
                    println!("\nImage: {}", image_info.file_name);
                    println!("Comment preview:");
                    println!("  Author: {}", name.as_deref().unwrap_or("Anonymous"));
                    println!("  Text: {}", text);
                    if let Some(r) = rating {
                        println!("  Rating: {}/5", r);
                    }

                    // Confirm
                    use dialoguer::Confirm;
                    let confirmed = Confirm::new()
                        .with_prompt("Post this comment?")
                        .default(true)
                        .interact()?;

                    if !confirmed {
                        println!("\nComment cancelled.");
                        return Ok(());
                    }

                    // Create comment
                    println!("\nPosting comment...");
                    let request = api::comments::CreateCommentRequest {
                        text,
                        name,
                        email,
                        rating,
                        link: None,
                    };

                    match client.create_image_comment(&image_key, request).await {
                        Ok(comment) => {
                            println!("\n✓ Comment posted successfully!");
                            if let Some(key) = comment.comment_key {
                                println!("  Comment Key: {}", key);
                            }
                        }
                        Err(e) => {
                            println!("\n✗ Failed to post comment: {}", e);
                        }
                    }
                }
            }
        }
        Commands::Upload {
            path,
            threads,
            album,
            parent,
            structure,
            interactive,
            dry_run,
            check_remote,
            no_cache,
        } => {
            let cfg = config::load_config()?;
            let client = std::sync::Arc::new(api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            ));

            // With no album, folder or mode given, upload to this month's album
            // in the default folder.
            let use_default_destination = album.is_none() && !structure && !interactive;

            // Determine upload mode and album name
            let (upload_mode, album_name) = if structure {
                (UploadMode::MaintainStructure, String::new())
            } else if let Some(name) = album {
                // Album specified via CLI, use single album mode
                (UploadMode::SingleAlbum, name)
            } else if use_default_destination {
                (
                    UploadMode::SingleAlbum,
                    chrono::Local::now().format("%Y-%m").to_string(),
                )
            } else {
                // --interactive: prompt user
                use dialoguer::{Input, Select};

                println!("\nNo album specified. How would you like to upload?");
                let choices = vec![
                    "Single album - flatten all images into one album",
                    "Maintain folder structure - create albums/folders matching your directory structure",
                ];

                let selection = Select::new().items(&choices).default(0).interact()?;

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

            // Folder the album series lives in, if any
            let folder_path = if use_default_destination && parent.is_none() {
                Some(cfg.upload.default_folder.clone())
            } else {
                parent.clone()
            };

            println!("Uploading from: {}", path);
            if matches!(upload_mode, UploadMode::SingleAlbum) {
                match &folder_path {
                    Some(folder) => println!("Album: {}/{}", folder, album_name),
                    None => println!("Album: {}", album_name),
                }
            }
            println!("Threads: {}", threads);
            if dry_run {
                println!("DRY RUN - no files will be uploaded\n");
            } else {
                println!();
            }

            // Get cache directory (used by both upload modes)
            let cache_path = if let Some(proj_dirs) =
                directories::ProjectDirs::from("com", "smugmug-cli", "smugmug-cli")
            {
                proj_dirs.cache_dir().to_path_buf()
            } else {
                std::path::PathBuf::from(".cache")
            };

            // Create cache directory if it doesn't exist
            std::fs::create_dir_all(&cache_path)?;

            // Handle upload based on mode
            match upload_mode {
                UploadMode::SingleAlbum => {
                    use uploader::album_series::{
                        AlbumScope, ClientAlbumSeries, MAX_ALBUM_IMAGES, plan_album_batches,
                    };

                    // Find or create the folder the album series lives in
                    // (a dry run only looks). The default folder is created
                    // private; a --parent folder keeps SmugMug's default
                    // privacy, as before.
                    let scope = if let Some(ref folder) = folder_path {
                        println!("Finding/creating folder path: {}", folder);
                        let privacy = if use_default_destination && parent.is_none() {
                            Some("Private")
                        } else {
                            None
                        };
                        let found = if dry_run {
                            client.find_folder_path(folder).await
                        } else {
                            client
                                .find_or_create_folder_path(folder, privacy)
                                .await
                                .map(Some)
                        };
                        match found {
                            Ok(Some(uri)) => {
                                println!("✓ Using folder: {}\n", folder);
                                AlbumScope::Folder(uri)
                            }
                            Ok(None) => {
                                println!("• Would create folder: {}\n", folder);
                                AlbumScope::MissingFolder
                            }
                            Err(e) => {
                                println!("✗ Failed to find/create folder path: {}", e);
                                return Ok(());
                            }
                        }
                    } else {
                        AlbumScope::Anywhere
                    };

                    let files: Vec<std::path::PathBuf> =
                        match scanner::scan_directory(std::path::Path::new(&path)) {
                            Ok(scanned) => scanned.into_iter().map(|f| f.path).collect(),
                            Err(e) => {
                                println!("✗ Failed to scan {}: {}", path, e);
                                return Ok(());
                            }
                        };

                    // Spread the files over "Name", "Name (2)", ... so no
                    // album goes over SmugMug's per-gallery limit.
                    println!("Looking up album...");
                    let backend = ClientAlbumSeries::new(client.clone(), scope);
                    let batches = match plan_album_batches(
                        &backend,
                        &album_name,
                        files,
                        MAX_ALBUM_IMAGES,
                        dry_run,
                    )
                    .await
                    {
                        Ok(batches) => batches,
                        Err(e) => {
                            println!("✗ Failed to find/create album: {}", e);
                            return Ok(());
                        }
                    };

                    for batch in &batches {
                        let count = batch.files.len();
                        if batch.new_album && batch.album.album_key.is_empty() {
                            println!(
                                "• Would create album: {} [Private] ({} files)",
                                batch.album.name, count
                            );
                        } else if batch.new_album {
                            println!(
                                "✓ Created album: {} (Key: {}) [Private] ({} files)",
                                batch.album.name, batch.album.album_key, count
                            );
                        } else {
                            println!(
                                "✓ Using album: {} (Key: {}) ({} files)",
                                batch.album.name, batch.album.album_key, count
                            );
                        }
                        if let Some(ref web_uri) = batch.album.web_uri {
                            println!("  URL: {}", web_uri);
                        }
                    }
                    if batches.len() > 1 {
                        println!(
                            "  (split across {} albums: SmugMug allows {} per album)",
                            batches.len(),
                            MAX_ALBUM_IMAGES
                        );
                    }
                    println!();

                    // Set up upload options
                    let upload_options = uploader::UploadOptions {
                        batches,
                        client,
                        threads,
                        dry_run,
                        check_remote,
                        no_cache,
                        cache_path,
                        retry_attempts: cfg.upload.retry_attempts,
                        has_smugmug_source: cfg.upload.has_smugmug_source,
                    };

                    // Perform upload
                    match uploader::upload_files(upload_options).await {
                        Ok(stats) => {
                            println!("\n{}", success("Upload complete!"));
                            println!(
                                "  {}: {}",
                                info("Total files"),
                                highlight(&stats.total_files.to_string())
                            );
                            println!(
                                "  {}: {}",
                                "Uploaded".green(),
                                highlight(&stats.uploaded.to_string())
                            );
                            println!("  {}: {}", "Replaced (modified)".cyan(), stats.replaced);
                            println!("  {}: {}", "Skipped (duplicates)".yellow(), stats.skipped);
                            println!("  {}: {}", "Failed".red(), stats.failed);
                            println!(
                                "  {}: {}",
                                info("Total size"),
                                highlight(&format!(
                                    "{:.2} MB",
                                    stats.total_bytes as f64 / 1024.0 / 1024.0
                                ))
                            );
                            println!(
                                "  {}: {}",
                                info("Duration"),
                                highlight(&format_duration(stats.duration_secs))
                            );
                        }
                        Err(e) => {
                            println!("\n{}", error(&format!("Upload failed: {}", e)));
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
                        retry_attempts: cfg.upload.retry_attempts,
                        has_smugmug_source: cfg.upload.has_smugmug_source,
                    })
                    .await
                    {
                        Ok(stats) => {
                            println!("\n{}", success("Upload complete!"));
                            println!(
                                "  {}: {}",
                                info("Total files"),
                                highlight(&stats.total_files.to_string())
                            );
                            println!(
                                "  {}: {}",
                                "Uploaded".green(),
                                highlight(&stats.uploaded.to_string())
                            );
                            println!("  {}: {}", "Replaced (modified)".cyan(), stats.replaced);
                            println!("  {}: {}", "Skipped (duplicates)".yellow(), stats.skipped);
                            println!("  {}: {}", "Failed".red(), stats.failed);
                            println!(
                                "  {}: {}",
                                info("Folders created"),
                                highlight(&stats.folders_created.to_string())
                            );
                            println!(
                                "  {}: {}",
                                info("Albums created"),
                                highlight(&stats.albums_created.to_string())
                            );
                            println!(
                                "  {}: {}",
                                info("Total size"),
                                highlight(&format!(
                                    "{:.2} MB",
                                    stats.total_bytes as f64 / 1024.0 / 1024.0
                                ))
                            );
                            println!(
                                "  {}: {}",
                                info("Duration"),
                                highlight(&format_duration(stats.duration_secs))
                            );
                        }
                        Err(e) => {
                            println!("\n{}", error(&format!("Upload failed: {}", e)));
                        }
                    }
                }
            }
        }
        Commands::Debug { command } => {
            let cfg = config::load_config()?;
            let client = api::SmugMugClient::new(
                cfg.auth.api_key,
                cfg.auth.api_secret,
                cfg.auth.access_token,
                cfg.auth.access_token_secret,
            );

            let is_upload = matches!(command, DebugCommands::LibraryUpload { .. });
            let (status, body) = match command {
                DebugCommands::Get { path, method } => {
                    client.request_raw(&method.to_uppercase(), &path).await?
                }
                DebugCommands::LibraryUpload { file, filepath } => {
                    let file_path = std::path::Path::new(&file);
                    let filepath = filepath.unwrap_or_else(|| {
                        file_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("image")
                            .to_string()
                    });
                    api::upload::send_library_upload(
                        &client,
                        api::upload::LIBRARY_UPLOAD_URL,
                        file_path,
                        &filepath,
                    )
                    .await?
                }
            };

            println!("HTTP {}", status);
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(json) => println!("{}", serde_json::to_string_pretty(&json)?),
                Err(_) => println!("{}", body),
            }
            if is_upload {
                match api::upload::parse_library_upload_response(status, &body) {
                    Ok(result) => println!(
                        "\n{}",
                        success(&format!(
                            "Parsed: image key {} ({})",
                            result.image_key, result.image_uri
                        ))
                    ),
                    Err(e) => println!("\n{}", error(&format!("Could not parse: {}", e))),
                }
            }
        }
        Commands::Status => {
            let cache_path = cache::get_cache_path()?;

            println!("Cache Status:");
            println!("  Location: {}", cache_path.display());

            match cache::HashStore::new(cache_path.to_str().unwrap()) {
                Ok(store) => match store.stats() {
                    Ok(stats) => {
                        println!("  Total entries: {}", format_number(stats.total_entries));
                        println!("  Cache size: {}", format_size(stats.total_size));

                        if let Some(oldest) = stats.oldest_entry {
                            println!("  Oldest entry: {}", oldest.format("%Y-%m-%d"));
                        } else {
                            println!("  Oldest entry: N/A");
                        }

                        if let Some(newest) = stats.newest_entry {
                            println!("  Newest entry: {}", newest.format("%Y-%m-%d"));
                        } else {
                            println!("  Newest entry: N/A");
                        }
                    }
                    Err(e) => {
                        println!("  Error retrieving stats: {}", e);
                    }
                },
                Err(e) => {
                    println!("  Error opening cache: {}", e);
                }
            }
        }
        Commands::Cache { command } => match command {
            CacheCommands::Clear => {
                println!("Clearing cache...");
                cache::clear_cache()?;
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn test_format_number_small() {
        assert_eq!(format_number(123), "123");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn test_format_number_thousands() {
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(9999), "9,999");
    }

    #[test]
    fn test_format_number_millions() {
        assert_eq!(format_number(1000000), "1,000,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn test_format_number_large() {
        assert_eq!(format_number(1234567890), "1,234,567,890");
    }

    #[test]
    fn test_format_size_bytes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1), "1 bytes");
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(1023), "1023 bytes");
    }

    #[test]
    fn test_format_size_kb() {
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(10240), "10.00 KB");
        assert_eq!(format_size(1024 * 1024 - 1), "1024.00 KB");
    }

    #[test]
    fn test_format_size_mb() {
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 5), "5.00 MB");
        assert_eq!(format_size(1024 * 1024 + 512 * 1024), "1.50 MB");
        assert_eq!(format_size(1024 * 1024 * 1024 - 1), "1024.00 MB");
    }

    #[test]
    fn test_format_size_gb() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_size(1024u64 * 1024 * 1024 * 5), "5.00 GB");
        assert_eq!(
            format_size(1024u64 * 1024 * 1024 + 512 * 1024 * 1024),
            "1.50 GB"
        );
    }

    #[test]
    fn test_format_size_precision() {
        // Test that we maintain 2 decimal places
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1587), "1.55 KB");
        assert_eq!(format_size(1024 + 10), "1.01 KB");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(1), "1s");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m 0s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3599), "59m 59s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h 0m 0s");
        assert_eq!(format_duration(3661), "1h 1m 1s");
        assert_eq!(format_duration(7200), "2h 0m 0s");
        assert_eq!(format_duration(7265), "2h 1m 5s");
    }
}
