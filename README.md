# SmugMug CLI

A fast, reliable command-line tool for uploading photos to SmugMug with intelligent deduplication and automatic album organization.

## Features

- **Simple Upload**: Upload individual files or entire directories
- **Albums by Date**: Files go into albums by the day they were taken (`2014/07/2014-07-12`)
- **Scheduled Backups**: `backup` keeps a library backed up on an interval, re-reading only files that changed
- **Smart Deduplication**: Hash-based detection prevents re-uploading the same photo
- **Album Organization**: Or keep your folder structure, or put everything in one album
- **Multi-threaded**: Concurrent uploads for maximum speed
- **Docker Support**: Easy deployment as a container
- **Local Cache**: Fast lookups of previously uploaded files

## Installation

### From crates.io

```bash
cargo install smugmug-cli
```

Requires Rust 1.89 or newer.

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

See `docker-compose.yml` for a complete example. A scheduled backup:

```yaml
services:
  backup:
    image: ghcr.io/jhofker/smugmug-cli:latest
    command: backup /photos --interval 6h --exclude "old_backup/"
    restart: unless-stopped
    stop_grace_period: 2m
    environment:
      - PUID=99  # Defaults to 99:100 if not specified
      - PGID=100
      - TZ=America/Chicago
      - SMUGMUG_API_KEY=${SMUGMUG_API_KEY}
      - SMUGMUG_API_SECRET=${SMUGMUG_API_SECRET}
      - SMUGMUG_ACCESS_TOKEN=${SMUGMUG_ACCESS_TOKEN}
      - SMUGMUG_ACCESS_TOKEN_SECRET=${SMUGMUG_ACCESS_TOKEN_SECRET}
    volumes:
      - ~/.config/smugmug-cli:/home/smugmug/.config/smugmug-cli
      - ~/.cache/smugmug-cli:/home/smugmug/.cache/smugmug-cli  # must persist
      - /path/to/photos:/photos:ro
```

With the `SMUGMUG_*` variables set, no config file is needed. The container runs at low CPU
priority (`NICE`, default 10) and idle I/O priority where the kernel allows it
(`IONICE_CLASS`, default 3); set either to an empty string to turn it off.

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
# Folder for albums by date when `upload` gets no --album (created private)
default_folder = "Uploads"
# RAW files: "auto" (originals with SmugMug Source, otherwise rendered JPEGs),
# "render", "original" or "skip"
raw_mode = "auto"
# Files read (hashed, dated, RAWs rendered) at once; keep low for spinning disks
read_threads = 2
# Video half of a Live Photo (IMG_1234.HEIC + IMG_1234.MOV): "upload" or "skip"
live_photo_videos = "upload"

[deduplication]
enabled = true

# For `smugmug-cli backup`
[backup]
sources = ["/photos"]
folder = "Backup"
# .gitignore syntax, relative to each source
exclude = ["old_backup/", "**/Screenshots/"]
# Time between runs; without it, backup runs once
interval = "6h"
# Optional overrides of the [upload] settings above
# upload_threads = 6
# read_threads = 2
# live_photo_videos = "skip"
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
  - `--exclude <PATTERN>` - Leave out matching paths (.gitignore syntax; repeatable). Uploads by date only
  - `--structure` - Recreate the directory structure as SmugMug folders and albums
  - `--interactive` - Ask whether to upload to one album or keep the folder structure
  - `--dry-run` - Preview what would be uploaded without uploading (creates no folders or albums)
  - `--check-remote` - Check SmugMug for existing files by MD5 hash (slower but more reliable)
  - `--no-cache` - Disable local cache (always check files, even if previously uploaded)
  - `--raw <MODE>` - How to handle RAW files (`auto`, `render`, `original`, `skip`), overriding `raw_mode` in the config

**Where files go:** with no `--album`, each file goes into a private album for the day it was
taken, `YEAR/MONTH/YEAR-MONTH-DAY` (e.g. `Uploads/2014/07/2014-07-12`), inside the
`default_folder` from your config (`Uploads` unless changed), or inside `--parent` if given.
See [Albums by date](#albums-by-date) below. `--dry-run` shows how many files would go where
without reading files in full or contacting SmugMug.

**Album size limit:** SmugMug allows 5,000 photos and videos per album. When an upload would go
past that, it continues in `Name (2)`, `Name (3)`, and so on, filling any partly-used album in
the series first. Albums are only created when a file actually needs uploading, so skipped
duplicates never create empty albums, and a re-run skips files already in any album of the
series. This applies to `--album` too. (`--structure` uploads aren't split.)

**RAW files:** RAW originals can only be uploaded with a SmugMug Source subscription (detected
by `init`/`auth`). Without one, each RAW file is uploaded as a JPEG instead: the full-size
preview the camera embedded in it, under the same name with a `.jpg` extension
(`IMG_1234.CR2` → `IMG_1234.jpg`), with the RAW's EXIF (capture date, camera, lens, exposure,
GPS, orientation) copied in. Things to know:

- It's the camera's own rendering (picture style, white balance), not a fresh RAW
  conversion, so edits made in Lightroom, darktable and the like (including `.xmp` sidecars)
  aren't included.
- A RAW with a JPEG or HEIC of the same name next to it (shooting RAW+JPEG) is skipped, so
  the camera's JPEG is kept rather than replaced.
- When a RAW file has no preview of at least 1600 px on the long edge (common for DNGs made
  by Adobe DNG Converter with its default medium-size preview), the RAW data itself is
  converted instead, with [rawler](https://github.com/dnglab/dnglab). That takes a few seconds
  and several hundred MB of memory per file (one at a time), and looks flatter than the
  camera's rendering. Files it can't decode fail and are listed as failed.
- With `--album`, a re-run skips a RAW whose `.jpg` is already in the album, without comparing
  contents. Uploads by date compare the rendered JPEG instead, so a metadata-only edit to a
  RAW (a DNG whose embedded XMP changed) isn't uploaded again.

Set `raw_mode` in the config (or pass `--raw`) to change this: `render` always uploads JPEGs,
`original` uploads RAW originals (skipped without Source), `skip` leaves RAW files out.

### Albums by date

Used by `upload` without `--album` and by `backup`:

- **Dates** come from, in order: the photo's EXIF capture date; a video's metadata (Apple's
  local creation date, else the movie header's UTC time shown in the local time zone, so set
  `TZ` in containers); a date in the file name (`IMG_20140712_…`, `2014-07-12 …`,
  `IMG-20140712-WA0001`, `PXL_…`); the EXIF "last written" date; and finally the file's
  modification time. The summary says how many files were dated each way.
- **Live Photos**: a video next to a photo of the same name (`IMG_1234.HEIC` +
  `IMG_1234.MOV`) goes into the photo's day album. Set `live_photo_videos = "skip"` to leave
  them out.
- **Days with more than 5,000 files** continue in `2014-07-12 (2)` and so on. Year and month
  folders and day albums are created only when a file needs them, all private. The folders it
  creates list their contents by name, ascending, so years, months and days read in order
  (SmugMug's own default is newest-modified first). Folders that already exist keep their sort
  order.
- **Duplicates** (the same content at several paths, e.g. a backup copy of a folder) are
  uploaded once; the other copies are linked to that image. A file already on SmugMug in
  another album (uploaded with `--album`, say) is added to its day album rather than
  uploaded again.
- **Same name, different photo** (two cameras both writing `IMG_0001.JPG` on one day): the
  second is uploaded as `IMG_0001~1a2b3c4d.JPG` instead of overwriting the first.
- **Edited files** replace the image uploaded from that path, unless an identical copy
  elsewhere shares that image, in which case the edit is uploaded as a new image.
- **Re-runs are cheap**: the cache records each file's size and modification time, so an
  unchanged file is skipped without being read, and a run over an unchanged library makes no
  SmugMug requests at all. A file whose time changed but content didn't is re-read once and
  not uploaded. Files SmugMug refuses (too big, unsupported) aren't retried until they change.
- **Excluded**: paths matching `--exclude`/`exclude` patterns, hidden files and folders
  (`.DS_Store`, `._*` AppleDouble files), NAS metadata folders (`@eaDir`, `#recycle`), and
  anything listed in a `.smugmugignore` file (.gitignore syntax) in any folder.

Nothing is ever deleted from SmugMug: removing or moving a local file leaves its image there.

### Backup

- `smugmug-cli backup [SOURCES...]` - Back up directories into albums by date, again every interval
  - `--folder <NAME>` - SmugMug folder to back up into (default: `Backup`)
  - `--exclude <PATTERN>` - Leave out matching paths (.gitignore syntax, relative to each source; repeatable)
  - `--interval <TIME>` - Time between the end of one run and the start of the next (`30m`, `6h`, `1d`)
  - `--once` - Run once even if an interval is configured
  - `--threads <N>` / `--read-threads <N>` - Concurrent uploads / file reads
  - `--dry-run` - Show how many files would go into which years and days (reads only metadata; contacts nothing)
  - `--raw <MODE>` - RAW handling, as for `upload`

Everything can also be set in the `[backup]` section of the config, and sources, folder and
interval through `SMUGMUG_BACKUP_SOURCES` (comma-separated), `SMUGMUG_BACKUP_FOLDER` and
`SMUGMUG_BACKUP_INTERVAL`. Each run prints a summary and writes it to `last_run.json` in the
cache directory. Ctrl-C or `docker stop` finishes the uploads in progress and saves the cache
before exiting; the rest is picked up next run.

**First run on a big library**: the first run reads every file once (to hash and date it), so
it takes a while; later runs only `stat` files. Start with `--dry-run` to check exclusions
and how files will be dated. On Unraid, the *Dynamix Cache Directories* plugin keeps
directory listings in memory so the periodic `stat` walk doesn't spin up array disks, and
the cache directory belongs on the SSD pool (e.g. `/mnt/user/appdata/smugmug-cli`).

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

MIT. Release binaries also include [rawler](https://github.com/dnglab/dnglab), which is LGPL-2.1
licensed; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
