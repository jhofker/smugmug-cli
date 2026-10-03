# SmugMug CLI - Design

## Overview
A Rust command-line tool for uploading photos to SmugMug with deduplication and automatic album organization. It also covers the day-to-day album, image and comment management you would otherwise do in the web UI, and can download albums.

See [README.md](README.md) for usage and [CLAUDE.md](CLAUDE.md) for implementation notes aimed at contributors.

## Core Features

### 1. Upload
- Upload single files or whole directories (JPEG, PNG, HEIF, RAW, video, ...)
- Multi-threaded workers (default 4) with progress bars
- Retries for failed uploads (`retry_attempts`, default 3)
- `--dry-run` previews everything; it creates no folders or albums

### 2. Deduplication
- SHA256 of file contents, stored in a local sled database
- By default, a file whose name already exists in the destination album is skipped if its content is unchanged and replaced in place if it differs
- `--check-remote` also matches existing SmugMug images by content hash regardless of filename
- Files already on SmugMug in another album are added to the destination album ("collected") instead of uploaded again: always when the cache knows them, and with `--check-remote` also when a search by capture time finds them
- `--no-cache` bypasses the local cache

### 3. Album Organization
- **Default destination**: a monthly album (`YYYY-MM`) in a private folder (`upload.default_folder`, default `Uploads`), or under `--parent`
- **Single album** (`--album`): flatten everything into one album
- **Maintain structure** (`--structure`): mirror the local directory tree as SmugMug folders and albums
- `--interactive` prompts for single album vs. structure
- SmugMug caps albums at 5,000 images, so uploads overflow into `Name (2)`, `Name (3)`, ... (`uploader/album_series.rs`)

### 4. RAW files
Uploading RAW originals needs a SmugMug Source subscription. `upload.raw_mode` / `--raw` chooses between `auto` (originals with Source, rendered JPEGs without), `render`, `original` and `skip`. A JPEG is rendered from the RAW's largest embedded preview (at least 1600 px on the long edge), or by converting the RAW data with `rawler`, and EXIF/GPS is copied across (`src/raw/`).

### 5. Management and download
- `albums` list/create/delete/settings/tree/download/get-download-link
- `images` list/info/update/delete
- `comments` list/create
- `status` and `cache clear` for the local cache
- A hidden `debug` command for probing raw API endpoints

## Architecture

### Project Structure
```
smugmug-cli/
├── src/
│   ├── main.rs              # CLI entry point and command handlers
│   ├── lib.rs
│   ├── config.rs            # TOML config + environment variables
│   ├── api/                 # SmugMug API v2 client
│   │   ├── mod.rs           # Client, OAuth 1.0a signing, paging helpers
│   │   ├── oauth_flow.rs    # Browser-based sign-in (`auth`)
│   │   ├── albums.rs        # Albums, folders, folder paths
│   │   ├── images.rs        # Image metadata
│   │   ├── comments.rs
│   │   └── upload.rs        # Upload/replace, Library probe
│   ├── cache/               # Local dedup state
│   │   ├── mod.rs
│   │   └── hash_store.rs    # SHA256 -> uploaded file metadata
│   ├── scanner/             # File system scanning
│   │   ├── mod.rs
│   │   └── walker.rs
│   ├── raw/                 # RAW -> JPEG rendering
│   │   ├── mod.rs
│   │   ├── jpeg.rs          # Embedded preview extraction
│   │   └── exif.rs          # Metadata copy into the JPEG
│   ├── uploader/            # Upload orchestration
│   │   ├── mod.rs           # Flat and structured uploads
│   │   ├── album_series.rs  # 5,000-image overflow planning
│   │   ├── capture_time.rs  # EXIF capture time as SmugMug stores it
│   │   ├── collect.rs       # Add already-uploaded files to the album
│   │   ├── queue.rs
│   │   └── worker.rs        # Per-file dedup, upload, replace
│   └── downloader/          # Album downloads
├── tests/                   # Integration tests (cache)
├── licenses/                # Third-party license texts (rawler, LGPL-2.1)
├── Dockerfile, docker-compose.yml, docker-entrypoint.sh
├── dist-workspace.toml      # cargo-dist release config
└── .github/workflows/       # release, docker-publish, crates-publish
```

### Key Dependencies

- **HTTP & API**: `reqwest` (native-tls), `oauth1-request` with `hmac`/`sha1` (OAuth 1.0a), `serde`, `serde_json`, `toml`
- **CLI & UX**: `clap` (derive), `indicatif`, `colored`, `dialoguer`
- **Concurrency**: `tokio`, `rayon`
- **Files**: `walkdir`, `sha2`, `md5`, `mime_guess`
- **Storage**: `sled`, `directories`
- **RAW**: `rawler` (LGPL-2.1, see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)), `little_exif`, `image`
- **Errors**: `anyhow`, `thiserror`

The minimum supported Rust version is 1.89 (required by `rawler` 0.8).

## SmugMug API Integration

### Authentication
OAuth 1.0a, HMAC-SHA1. SmugMug access tokens do not expire, so there is no refresh flow.

1. `init` walks through entering API key/secret and tokens
2. `auth` signs in through the browser to obtain or renew an access token
3. `test-auth` verifies the stored credentials

Every request is signed in `SmugMugClient::build_oauth_header()`.

### Endpoints (API v2)
- `/api/v2!authuser`, `/api/v2/user/{nickname}` - user info
- `/api/v2/node/{id}` and `!children` - folders and albums as nodes
- `/api/v2/album/{key}` and `!images` - album settings and image lists
- `/api/v2/album/{key}!collectimages` - add existing images to an album
- `/api/v2/image!search` - find images by capture time (exact ranges, 100 per page)
- `upload.smugmug.com` - image upload and replace (separate domain)

List endpoints return one page per request; `get_all_pages` follows `Pages.NextPage` so nothing past the first page is dropped.

SmugMug's Library ("All Media") endpoints currently return 404 to OAuth API keys, so they are not used as a destination. The code and the `debug` commands remain for when that changes.

### Concurrency
Configurable worker count (default 4). Retries use `retry_attempts`; explicit rate-limit handling and exponential backoff are not implemented yet (see Future Enhancements).

## Configuration

Priority: environment variables (`SMUGMUG_API_KEY`, ...), then `.env`, then `~/.config/smugmug-cli/config.toml`.

```toml
[auth]
api_key = "..."
api_secret = "..."
access_token = "..."
access_token_secret = "..."

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300
has_smugmug_source = false     # account has SmugMug Source (RAW originals)
default_folder = "Uploads"     # where monthly albums go
raw_mode = "auto"              # auto | render | original | skip

[deduplication]
enabled = true
cache_path = "~/.cache/smugmug-cli/hashes.db"
```

## Deduplication Strategy

### Hash Cache Schema
```
file_hash (SHA256) -> {
  smugmug_uri: String,
  album_key: String,
  image_key: String,
  uploaded_at: DateTime,
  file_size: u64,
  original_path: String
}
```

### Upload Decision Logic (per file)
1. Calculate the SHA256 of the file
2. Check the local cache; skip if present
3. Compare against images in the destination album (every album in the series): same name and same content is skipped, same name with different content is replaced in place
4. With `--check-remote`, also skip content that exists under a different name
5. If the file is already on SmugMug in another album (per the cache, or with `--check-remote` a capture-time search confirmed by MD5), collect it into the destination album instead (`uploader/collect.rs`, batched before the workers start)
6. Otherwise upload (creating the next album in the series only at this point), then store the hash

Rendered RAWs are cached under the RAW file's SHA256 and uploaded as `<stem>.jpg`; they are skipped, not replaced, when that name already exists.

## Distribution

- **Binaries**: cargo-dist builds macOS (arm64/x64), Linux x64 and Windows x64 artifacts, shell/PowerShell installers and a Homebrew formula on version tags (`release.yml`)
- **Docker**: multi-arch image on ghcr.io on version tags (`docker-publish.yml`); Alpine-based, drops to `PUID`/`PGID` through `docker-entrypoint.sh`
- **crates.io**: published on version tags through trusted publishing (`crates-publish.yml`)

## Testing Strategy

- **Unit tests** are inline with their modules: config, cache, hashing, OAuth signing, RAW handling, album-series planning (against an in-memory backend)
- **Integration tests** live in `tests/` (cache operations)
- **API mocking** uses `mockito`
- **Manual testing**: real SmugMug account, large file sets, network failures, RAW files from several camera makes

## Status

The original phased plan (core infrastructure, API client, file processing, deduplication, polish) is complete, and the tool is released as 0.4.0. Work since then has been monthly default albums with 5,000-image rollover, browser-based sign-in, on-demand album creation and full pagination, RAW-to-JPEG rendering, and the release pipeline.

## Future Enhancements
- Rate-limit handling with exponential backoff
- Watch mode for continuous sync
- Include/exclude patterns
- Library ("All Media") as the default destination, once SmugMug allows it for OAuth API keys
- Web UI for monitoring
- Metrics and structured logging
