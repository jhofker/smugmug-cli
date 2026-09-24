# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

SmugMug CLI is a Rust-based command-line tool for uploading photos to SmugMug with intelligent deduplication and automatic album organization. It uses OAuth 1.0a authentication and the SmugMug API v2.

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
  - `mod.rs`: Main upload coordination, supports both flat and structured uploads
- **src/downloader/**: Album download functionality
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

`list_album_images` follows `Pages.NextPage` (100 images per page); anything reading album contents relies on that.

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
