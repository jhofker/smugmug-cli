# SmugMug CLI

A fast, reliable command-line tool for uploading photos to SmugMug with intelligent deduplication and automatic album organization.

## Features

- **Simple Upload**: Upload individual files or entire directories
- **Smart Deduplication**: Hash-based detection prevents re-uploading the same photo
- **Album Organization**: Automatically create and organize albums based on folder structure
- **Multi-threaded**: Concurrent uploads for maximum speed
- **Docker Support**: Easy deployment as a container
- **Local Cache**: Fast lookups of previously uploaded files

## Installation

### From Source

```bash
cargo install --path .
```

### Docker

Pre-built multi-architecture images are available from GitHub Container Registry:

```bash
# Pull the latest image
docker pull ghcr.io/jhofker/smugmug-cli:latest

# Or pull a specific version
docker pull ghcr.io/jhofker/smugmug-cli:0.2.1

# Run with your user's UID/GID to avoid permission issues
docker run -e PUID=$(id -u) -e PGID=$(id -g) \
           -v ~/.config/smugmug-cli:/home/smugmug/.config/smugmug-cli \
           -v ~/.cache/smugmug-cli:/home/smugmug/.cache/smugmug-cli \
           -v /path/to/photos:/photos:ro \
           ghcr.io/jhofker/smugmug-cli:latest upload /photos

# Or use specific UID/GID (defaults to 99:100)
docker run -e PUID=1000 -e PGID=1000 \
           -v ~/.config/smugmug-cli:/home/smugmug/.config/smugmug-cli \
           -v /path/to/photos:/photos:ro \
           ghcr.io/jhofker/smugmug-cli:latest upload /photos
```

**Build locally:**
```bash
docker build -t smugmug-cli .
```

### Docker Compose

See `docker-compose.yml` for a complete example. Basic usage:

```yaml
services:
  upload:
    image: ghcr.io/jhofker/smugmug-cli:latest
    command: upload /photos --album "My Photos"
    environment:
      - PUID=99  # Defaults to 99:100 if not specified
      - PGID=100
      - SMUGMUG_API_KEY=${SMUGMUG_API_KEY}
      - SMUGMUG_API_SECRET=${SMUGMUG_API_SECRET}
      - SMUGMUG_ACCESS_TOKEN=${SMUGMUG_ACCESS_TOKEN}
      - SMUGMUG_ACCESS_TOKEN_SECRET=${SMUGMUG_ACCESS_TOKEN_SECRET}
    volumes:
      - ~/.config/smugmug-cli:/home/smugmug/.config/smugmug-cli
      - ~/.cache/smugmug-cli:/home/smugmug/.cache/smugmug-cli
      - /path/to/photos:/photos:ro
```

To build locally instead, replace `image:` with `build: .`

## Quick Start

### Getting Your Credentials

1. Go to https://api.smugmug.com/api/developer/apply
2. Create an application to get your **API Key** and **API Secret**
3. Go to SmugMug Account Settings → Privacy → Authorized Services
4. Click **"token"** next to your application to get your **Access Token** and **Access Token Secret**

### Method 1: Using Environment Variables (Recommended for Development)

```bash
# Copy the example file
cp .env.example .env

# Edit .env with your credentials
SMUGMUG_API_KEY=your_api_key_here
SMUGMUG_API_SECRET=your_api_secret_here
SMUGMUG_ACCESS_TOKEN=your_access_token_here
SMUGMUG_ACCESS_TOKEN_SECRET=your_access_token_secret_here

# Test authentication
smugmug-cli test-auth

# Upload photos
smugmug-cli upload /path/to/photos
```

### Method 2: Using Config File (Recommended for Production)

```bash
# Interactive setup
smugmug-cli init

# Test authentication
smugmug-cli test-auth

# Upload photos
smugmug-cli upload /path/to/photos
```

Configuration is stored in `~/.config/smugmug-cli/config.toml`

### Upload with Options

```bash
smugmug-cli upload /path/to/photos \
  --threads 8 \
  --album "Vacation 2025"
```

## Configuration

The tool supports two configuration methods (environment variables take precedence):

### Environment Variables
```bash
SMUGMUG_API_KEY=...
SMUGMUG_API_SECRET=...
SMUGMUG_ACCESS_TOKEN=...
SMUGMUG_ACCESS_TOKEN_SECRET=...
```

### Config File (`~/.config/smugmug-cli/config.toml`)
```toml
[auth]
api_key = "your_api_key"
api_secret = "your_api_secret"
access_token = "your_access_token"
access_token_secret = "your_access_token_secret"

[upload]
threads = 4
retry_attempts = 3
# Folder for monthly albums when `upload` gets no --album (created private)
default_folder = "Uploads"

[deduplication]
enabled = true
```

## Commands

### Setup & Authentication

- `smugmug-cli init` - Initialize configuration and authenticate with SmugMug (interactive setup)
- `smugmug-cli auth` - Sign in through your browser to get (or refresh) an access token
- `smugmug-cli test-auth` - Test that your credentials are working

### Upload

- `smugmug-cli upload <PATH>` - Upload photos from the specified path
  - `--threads <N>` - Number of concurrent upload threads (default: 4)
  - `--album <NAME>` - Album name (creates if doesn't exist, private by default)
  - `--parent <PATH>` - Parent folder path (e.g., "2024/Travel")
  - `--structure` - Recreate the directory structure as SmugMug folders and albums
  - `--interactive` - Ask whether to upload to one album or keep the folder structure
  - `--dry-run` - Preview what would be uploaded without uploading (creates no folders or albums)
  - `--check-remote` - Check SmugMug for existing files by MD5 hash (slower but more reliable)
  - `--no-cache` - Disable local cache (always check files, even if previously uploaded)

**Where files go:** with no `--album`, files go to an album named for the current month
(e.g. `2026-09`) inside the `default_folder` from your config (`Uploads` unless changed), or
inside `--parent` if given. The default folder and all auto-created albums are private; use
`albums settings` to change privacy after creation.

**Album size limit:** SmugMug allows 5,000 photos and videos per album. When an upload would go
past that, it continues in `Name (2)`, `Name (3)`, and so on, filling any partly-used album in
the series first. Albums are only created when a file actually needs uploading, so skipped
duplicates never create empty albums, and a re-run skips files already in any album of the
series. This applies to `--album` too. (`--structure` uploads aren't split.)

**RAW files:** without a SmugMug Source subscription (detected by `init`/`auth`), RAW files are
skipped with a notice instead of being uploaded and failing.

### Albums

- `smugmug-cli albums list` - List all albums
- `smugmug-cli albums create <NAME>` - Create a new album (private by default)
  - `--privacy <LEVEL>` - Set privacy level (private, unlisted, public) - defaults to private
- `smugmug-cli albums delete <ALBUM>` - Delete an album
  - `--force` - Force deletion without confirmation
- `smugmug-cli albums download <ALBUM> [OPTIONS]` - Download all images from an album
  - `--output <DIR>` - Output directory (default: current directory)
  - `--threads <N>` - Number of concurrent download threads (default: 4)
- `smugmug-cli albums tree` - Show folder/album tree structure
- `smugmug-cli albums settings <ALBUM> [OPTIONS]` - Update album settings
  - `--privacy <LEVEL>` - Set privacy (public, unlisted, private)
  - `--description <TEXT>` - Set description
  - `--keywords <KEYWORDS>` - Set keywords (semicolon-separated)
  - `--sort-method <METHOD>` - Set sort method (Position, Caption, FileName, DateTimeOriginal, DateTimeUploaded)
  - `--sort-direction <DIR>` - Set sort direction (asc, desc)
- `smugmug-cli albums get-download-link <ALBUM>` - Get album download link (ZIP file)
  - `--wait` - Wait for download generation (polls until ready)

### Images

- `smugmug-cli images list <ALBUM>` - List images in an album
- `smugmug-cli images info <ALBUM> <IMAGE_KEY>` - Show detailed information about an image
- `smugmug-cli images delete <ALBUM> <IMAGE_KEY>` - Delete an image
  - `--force` - Force deletion without confirmation
- `smugmug-cli images update <ALBUM> <IMAGE_KEY> [OPTIONS]` - Update image metadata
  - `--caption <TEXT>` - Set image caption
  - `--title <TEXT>` - Set image title
  - `--keywords <KEYWORDS>` - Set keywords (semicolon-separated)
  - `--latitude <LAT>` - Set latitude
  - `--longitude <LON>` - Set longitude
- `smugmug-cli images move <SOURCE_ALBUM> <IMAGE_KEY> <TARGET_ALBUM>` - Move image to a different album
  - `--force` - Force move without confirmation

### Comments

- `smugmug-cli comments list <ALBUM> <IMAGE_KEY>` - List comments on an image
- `smugmug-cli comments create <ALBUM> <IMAGE_KEY> [OPTIONS]` - Create a new comment on an image
  - `--text <TEXT>` - Comment text (required)
  - `--name <NAME>` - Commenter name
  - `--email <EMAIL>` - Commenter email
  - `--rating <0-5>` - Rating (0-5)

### Cache

- `smugmug-cli status` - Show cache statistics and upload history
- `smugmug-cli cache clear` - Clear the local deduplication cache

## Development

See [DESIGN.md](DESIGN.md) for architecture details and development roadmap.

### Building

```bash
cargo build --release
```

### Running Tests

```bash
cargo test
```

## SmugMug API Setup

1. Go to https://api.smugmug.com/api/developer/apply
2. Create a new application
3. Note your API Key and API Secret
4. Use these during `smugmug-cli init`

## License

MIT
