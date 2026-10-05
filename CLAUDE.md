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
  - `collect.rs`: adds files the cache knows are in another album to this one instead of skipping them (see Collecting below)
  - `mod.rs`: Main upload coordination, supports both flat and structured uploads
- **src/dated/**: uploads into albums by capture date, `<root>/YYYY/MM/YYYY-MM-DD` (default `upload` destination and `backup`)
  - `scan.rs`: walks with the `ignore` crate (exclude globs as overrides, `.smugmugignore`, hidden files, NAS dirs pruned), stat only; pairs Live Photo videos with their photo and drops RAWs with a JPEG sibling (`select_raw_files`)
  - `index.rs`: sled tree `files` (path → size, mtime, SHA-256, uploaded MD5, date, image URI) and `refs` (image URI → paths using it), in the hash store's database
  - `date.rs`: EXIF original date (kamadak-exif; RAWs via `raw::exif`), QuickTime `com.apple.quicktime.creationdate`/`mvhd`, file-name dates, EXIF `DateTime`, mtime
  - `plan.rs`: year/month folders and day album series (`AlbumSeries` per day), created lazily and listed once per run; per-day file-name map for same-name/different-content renames (`stem~md5[..8].ext`); `Smug` trait with an in-memory fake for tests
  - `run.rs`: the pipeline (see below); `Uploads` trait so tests run against the fake
- **src/backup.rs**: `backup` command: interval loop, `last_run.json`
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
- **Default destination** (no `--album`): albums by capture date (`YYYY/MM/YYYY-MM-DD`) in the private `upload.default_folder` (default `Uploads`), or in `--parent`, via `dated::run` (see Dated uploads below)
- **Single Album** (`--album`): Flatten all images into one album (with optional parent folder)
- **Maintain Structure** (`--structure`): Preserve directory structure as folders/albums on SmugMug
- `--interactive` brings back the old prompt to choose between the last two

Single-album uploads (and each day in dated uploads) go into an album series (`uploader/album_series.rs`): SmugMug caps albums at 5,000 images, so files overflow into `Name (2)`, `Name (3)`, .... `AlbumSeries::load` finds the existing albums; each worker calls `claim()` only once a file actually needs a new upload (creating the next album on demand) and `release()` if it fails, so skipped files never create albums. Duplicate/replace detection covers every existing album in the series. Tested against an in-memory backend. Scanning and RAW filtering happen before anything touches SmugMug; dry runs create no folders or albums.

RAW files follow `upload.raw_mode` / `--raw` (`RawMode::handling` turns it into upload, render or skip given `has_smugmug_source`). `uploader::select_raw_files` runs before upload and drops RAWs that would render to the same name as a JPEG/HEIC next to them. Rendered RAWs are cached under the RAW file's SHA-256, uploaded as `<stem>.jpg`, and skipped (not replaced) when that name is already in the album. Rendering runs in `spawn_blocking`; preview extraction is deterministic, but don't rely on MD5 matches across versions of this code or its dependencies.

SmugMug list endpoints (`!albums`, `!images`, `!comments`, `!children`, ...) return one page per request (often 100 items, sometimes 50) with the next page in `Response.Pages.NextPage`. Read lists through `SmugMugClient::get_all_pages(url, locator)`, which follows every page; a single `get_with_auth` on a list endpoint silently drops everything after the first page. (The `!children` lookups in `albums.rs` page by hand so they can stop as soon as they find a match; `list_children` reads them all.) `auth_user()` caches the signed-in user's nickname and root node per client.

SmugMug's Library ("All Media") would be the natural default destination, but its endpoints (`upload.smugmug.com/api/v2/library`, `/api/v2/library!assets`) return 404 to OAuth API keys as of 2026-09; `api::upload::upload_to_library` and the hidden `debug` commands are kept for when that changes.

### Dated uploads and `backup`
`dated::run` makes one pass: scan (stat only) → files whose index record matches size+mtime are skipped unread → the rest are read on `read_threads` blocking threads (SHA-256+MD5 in one 1 MiB-buffered pass; RAWs read whole, rendered only if needed) → `upload_threads` futures on one task (no `Send` bounds needed) link, collect, replace or upload. Per file: same SHA as the record → re-record; SHA in the hash store → link if in the day's albums, else collect (batched per day); rendered RAW whose JPEG MD5 equals the record's → re-record; record has an image used by no other path → replace; else upload (name reserved in the day's map). Identical copies read concurrently wait on the first upload (`in_flight`). SmugMug 400/413/415/422 refusals are recorded (`failed`) and not retried until the file changes. A dry run only dates files and makes no API calls. Uploads stream from disk (`api::upload::upload_file`) through the client's shared connection pool.

`backup` repeats `dated::run` every interval (measured from the end of a run). Ctrl-C/SIGTERM sets a stop flag: readers stop, in-flight uploads finish, sled flushes.

### Configuration Priority
1. Environment variables (`SMUGMUG_API_KEY`, `SMUGMUG_API_SECRET`, `SMUGMUG_ACCESS_TOKEN`, `SMUGMUG_ACCESS_TOKEN_SECRET`) override the config file's credentials; with the key and token set, no config file is needed (containers)
2. Config file at `~/.config/smugmug-cli/config.toml` (`[auth]`, `[upload]`, `[deduplication]`, `[backup]`)
3. A `.env` file is only read by `init`/`auth` as defaults

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
The workers skip any file in the cache, so a file uploaded to album A would never appear in album B. SmugMug doesn't deduplicate uploads (uploading again stores a second, separate image), but an image can be *collected* into more albums: `POST album/<key>!collectimages` with JSON body `{"CollectUris": "<uri>,<uri>"}` (a query string gets a 400). Before the workers start, `uploader::collect::plan_collects` (single-album uploads; dated uploads collect per day through `collect_batch`; not `--structure`; not with `--no-cache`) collects cache hits whose `album_key` isn't one of the series' albums, from the cached URI, with no lookups.
- Collects go in batches of 100 per album and take series slots like uploads. One refused URI makes the whole request a 400 listing it under `UriProblems` (the others may still have been collected); refused cache entries are removed, the rest are collected again (it's idempotent) and refused files are uploaded. A collect that fails outright (e.g. a 503) counts its files as failed: they're not uploaded (that would duplicate them) and their cache entries stay, so the next run retries.
- Finding files *not* in the cache account-wide was tried and dropped as not worth the complexity (SmugMug storage is unlimited). For the record: `image!search?Scope=<user>&DateTakenStart&DateTakenEnd` works with exact second ranges (100 results per page, a few minutes' indexing lag), and SmugMug stores EXIF `DateTimeOriginal` as US Pacific time (DST-aware), ignoring `OffsetTimeOriginal` and the account's time zone.

## Common Development Patterns

### Adding a New API Endpoint
1. Add method to `SmugMugClient` in appropriate module (albums, images, upload)
2. Use `get_with_auth()` or `post_with_auth()` for authenticated requests (they retry 429s and failed connections, GETs also 5xx, honoring `Retry-After`; requests sent with `self.client` directly don't)
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
