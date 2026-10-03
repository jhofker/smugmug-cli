# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SmugMug CLI is a Rust-based command-line tool for uploading photos to SmugMug with intelligent deduplication and automatic album organization. It uses OAuth 1.0a authentication and the SmugMug API v2.

## Commits, PRs and Comments

Don't add advertising or session links to commit messages, PR descriptions or GitHub comments: no "Generated with Claude Code" lines, no claude.ai session URLs and no `Co-Authored-By: Claude` trailers.

## Build and Test Commands

### Building
```bash
# Debug build
cargo build

# Release build
cargo build --release

# Install locally
cargo install --path .
```

### Testing
```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_hash_calculation

# Run integration tests
cargo test --test cache_test
```

### Running
```bash
# Run from source (debug)
cargo run -- <command>

# Run installed binary
smugmug-cli <command>
```

## Key Architecture

### Module Structure
- **src/api/**: SmugMug API client with OAuth 1.0a authentication
  - `mod.rs`: Core client with OAuth header building (`build_oauth_header`)
  - `albums.rs`: Album listing, creation, folder path handling
  - `images.rs`: Image metadata fetching
  - `upload.rs`: Image upload operations
- **src/cache/**: Local deduplication using sled embedded database
  - `hash_store.rs`: SHA256 hash storage mapping to uploaded file metadata
- **src/scanner/**: File system traversal for finding uploadable images
- **src/uploader/**: Multi-threaded upload orchestration
  - `queue.rs`: Thread-safe upload queue
  - `worker.rs`: Individual upload worker logic with deduplication checks
  - `collect.rs`: collects files already on SmugMug into the album instead of re-uploading (see Collecting below)
  - `capture_time.rs`: EXIF `DateTimeOriginal` → the UTC time SmugMug stores, for exact searches
  - `mod.rs`: Main upload coordination, supports both flat and structured uploads
- **src/downloader/**: Album download functionality
- **src/raw/**: RAW → JPEG for accounts without SmugMug Source (`raw_mode`)
  - `mod.rs`: uses the largest embedded preview (≥1600 px long edge); otherwise converts the RAW data with `rawler` (LGPL-2.1, see THIRD-PARTY-NOTICES.md), one conversion at a time behind a mutex because each needs hundreds of MB
  - `jpeg.rs`: finds embedded JPEG previews by walking the file for well-formed JPEG streams (rejects lossless SOF3 streams, which are sensor data in CR2/DNG)
  - `exif.rs`: reads IFD0/EXIF/GPS from TIFF-based RAWs and CR3 `CMT1/2/4` boxes (formats `little_exif` doesn't parse) and writes them into the JPEG with `little_exif`
- **src/config.rs`: Configuration management (TOML file + environment variables)

### Authentication Flow
Uses OAuth 1.0a with HMAC-SHA1 signing (via `oauth1-request` crate). The client requires:
- API Key and Secret (from SmugMug developer portal)
- Access Token and Access Token Secret (from SmugMug account settings)

Authentication is handled by `SmugMugClient::build_oauth_header()` which generates the OAuth Authorization header for each request.

### Deduplication Strategy
1. Calculate SHA256 hash of file contents
2. Check local cache (sled database) for hash
3. Optionally check remote SmugMug MD5 hashes with `--check-remote`
4. Skip upload if duplicate found
5. Store hash + metadata after successful upload

Cache location: `~/.cache/smugmug-cli/hash_store/`

### Upload Modes
- **Default destination** (no `--album`): monthly album (`YYYY-MM`) in the private `upload.default_folder` (default `Uploads`), or in `--parent`
- **Single Album** (`--album`): Flatten all images into one album (with optional parent folder)
- **Maintain Structure** (`--structure`): Preserve directory structure as folders/albums on SmugMug
- `--interactive` brings back the old prompt to choose between the last two

Single-album and default uploads go into an album series (`uploader/album_series.rs`): SmugMug caps albums at 5,000 images, so files overflow into `Name (2)`, `Name (3)`, .... `AlbumSeries::load` finds the existing albums; each worker calls `claim()` only once a file actually needs a new upload (creating the next album on demand) and `release()` if it fails, so skipped files never create albums. Duplicate/replace detection covers every existing album in the series. Tested against an in-memory backend. Scanning and RAW filtering happen before anything touches SmugMug; dry runs create no folders or albums.

RAW files follow `upload.raw_mode` / `--raw` (`RawMode::handling` turns it into upload, render or skip given `has_smugmug_source`). `uploader::select_raw_files` runs before upload and drops RAWs that would render to the same name as a JPEG/HEIC next to them. Rendered RAWs are cached under the RAW file's SHA-256, uploaded as `<stem>.jpg`, and skipped (not replaced) when that name is already in the album. Rendering runs in `spawn_blocking`; preview extraction is deterministic, but don't rely on MD5 matches across versions of this code or its dependencies.

SmugMug list endpoints (`!albums`, `!images`, `!comments`, `!children`, ...) return one page per request (often 100 items, sometimes 50) with the next page in `Response.Pages.NextPage`. Read lists through `SmugMugClient::get_all_pages(url, locator)`, which follows every page; a single `get_with_auth` on a list endpoint silently drops everything after the first page. (The `!children` lookups in `albums.rs` page by hand so they can stop as soon as they find a match.)

SmugMug's Library ("All Media") would be the natural default destination, but its endpoints (`upload.smugmug.com/api/v2/library`, `/api/v2/library!assets`) return 404 to OAuth API keys as of 2026-09; `api::upload::upload_to_library` and the hidden `debug` commands are kept for when that changes.

### Configuration Priority
1. Environment variables (SMUGMUG_API_KEY, etc.)
2. .env file in project root
3. Config file at `~/.config/smugmug-cli/config.toml`

## Testing Notes

- Unit tests are inline with modules (using `#[cfg(test)]`)
- Integration tests are in `tests/` directory
- Use `mockito` for API mocking (see `src/api/mod.rs` tests)
- Use `tempfile` or temp_dir() for file system tests
- Tests include comprehensive coverage for config serialization, cache operations, and thread safety

## Important Implementation Details

### OAuth Header Generation
The `build_oauth_header()` method in `api/mod.rs` constructs OAuth 1.0a headers. It currently defaults to GET for unknown HTTP methods.

### Album and Folder Management
- Albums are created under a parent node (folder or root)
- `find_or_create_folder_path()` in `api/albums.rs` handles nested folder creation
- Node URIs are used to reference locations in the SmugMug hierarchy

### Concurrent Upload Workers
The uploader uses tokio async tasks to process files concurrently. Each worker:
1. Polls the queue for next file
2. Checks cache for duplicates
3. Calculates MD5 hash
4. Uploads to SmugMug
5. Stores hash in cache
6. Updates progress bar

### Remote Duplicate Detection
When `--check-remote` is enabled, the tool fetches all images in an album with their MD5 hashes before uploading, allowing detection of duplicates even if the local cache is empty.

### Collecting (one image in several albums)
SmugMug doesn't deduplicate uploads: the same file uploaded to two albums becomes two images with separate stored files. An image can instead be *collected* into more albums (`POST album/<key>!collectimages`, JSON body `{"CollectUris": "<uri>,<uri>"}`; a query string gets a 400). Before the workers start, `uploader::collect::plan_collects` (single-album and default uploads; not yet `--structure`) collects files that already exist elsewhere in the account, spending as few requests as possible:
- A cache hit whose `album_key` isn't one of the series' albums is collected from the cached URI with no lookup.
- With `--check-remote`, other files with an EXIF capture time are found with `image!search?Scope=<user>&DateTakenStart&DateTakenEnd` over exact times: photos within 5 minutes of each other share one search (pages are capped at 100), and matches are confirmed by `ArchivedMD5`. Files without a capture time, and rendered RAWs, are just uploaded.
- SmugMug stores EXIF `DateTimeOriginal` as **US Pacific time** (DST-aware), ignoring `OffsetTimeOriginal` and the account's time zone; `capture_time.rs` converts the same way.
- Collects go in batches of 100 per album and take series slots like uploads. One refused URI makes the whole request a 400 listing it under `UriProblems` (the others may still have been collected); refused cache entries are removed, the rest are collected again (it's idempotent) and refused files are uploaded. A collect that fails outright (e.g. a 503) counts its files as failed: they're not uploaded (that would duplicate them) and their cache entries stay, so the next run retries.
- New uploads take a few minutes to show up in search; the cache covers files from the current run.

## Common Development Patterns

### Adding a New API Endpoint
1. Add method to `SmugMugClient` in appropriate module (albums, images, upload)
2. Use `get_with_auth()` or `post_with_auth()` for authenticated requests
3. Define response structs with serde derives
4. Add unit tests with mockito

### Adding CLI Commands
1. Update `Commands` enum in `src/main.rs`
2. Add command-specific options struct
3. Implement handler in main match statement
4. Update help text via clap attributes

### Working with the Cache
```rust
let store = HashStore::new(cache_path)?;
let file_hash = calculate_file_hash(&path)?;
if let Some(uploaded_file) = store.get(&file_hash)? {
    // File was previously uploaded
}
```
