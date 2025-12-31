# SmugMug CLI - Design Specification

## Overview
A Rust-based command-line tool for uploading photos to SmugMug with deduplication and automatic album organization.

## Core Features

### 1. Simple Upload
- Upload individual files or entire directories
- Support common image formats (JPEG, PNG, HEIF, RAW, etc.)
- Support video formats (MP4, MOV, etc.)
- Progress tracking with multi-threaded uploads
- Retry logic for failed uploads

### 2. Deduplication
- Hash-based detection (SHA256 of file contents)
- Check against SmugMug metadata before uploading
- Local cache of uploaded file hashes for fast lookups
- Skip already-uploaded files

### 3. Album Organization
- Map local folder structure to SmugMug albums/folders
- Create albums automatically based on directory names
- Support nested folder structures
- Configurable album naming strategies

## Architecture

### Project Structure
```
smugmug-cli/
├── src/
│   ├── main.rs              # CLI entry point
│   ├── api/                 # SmugMug API client
│   │   ├── mod.rs
│   │   ├── auth.rs          # OAuth authentication
│   │   ├── albums.rs        # Album management
│   │   └── upload.rs        # Upload operations
│   ├── cache/               # Local state management
│   │   ├── mod.rs
│   │   └── hash_store.rs    # Uploaded file tracking
│   ├── scanner/             # File system scanning
│   │   ├── mod.rs
│   │   └── walker.rs        # Directory traversal
│   ├── uploader/            # Upload orchestration
│   │   ├── mod.rs
│   │   ├── queue.rs         # Upload queue management
│   │   └── worker.rs        # Upload worker threads
│   └── config.rs            # Configuration management
├── Dockerfile
├── Cargo.toml
└── README.md
```

### Key Dependencies (Rust Crates)

#### HTTP & API
- `reqwest` - HTTP client with async support
- `oauth2` - OAuth 2.0 authentication
- `serde` / `serde_json` - JSON serialization

#### CLI & UX
- `clap` - Command-line argument parsing (with derive feature)
- `indicatif` - Progress bars
- `colored` - Terminal colors

#### Concurrency
- `tokio` - Async runtime
- `rayon` - Data parallelism for file scanning

#### File Operations
- `walkdir` - Directory traversal
- `sha2` - SHA256 hashing
- `mime_guess` - MIME type detection

#### Storage
- `sled` - Embedded database for local cache
- `directories` - Platform-specific paths

#### Error Handling
- `anyhow` - Error handling
- `thiserror` - Custom error types

## SmugMug API Integration

### Authentication Flow
1. OAuth 2.0 three-legged authentication
2. Store access token and refresh token securely
3. Automatic token refresh on expiry

### API Endpoints (V2 API)
- `/api/v2/user/{nickname}` - Get user info
- `/api/v2/folder/{id}` - Folder operations
- `/api/v2/node/{id}` - Node (album/folder) operations
- `/api/v2/album/{id}` - Album management
- `/api/v2/album/{id}!images` - Upload images
- Upload endpoint (separate domain)

### Rate Limiting
- Respect SmugMug rate limits
- Implement exponential backoff
- Configurable concurrent uploads (default: 4)

## CLI Interface

### Commands

```bash
# Initialize configuration and authenticate
smugmug-cli init

# Upload a directory
smugmug-cli upload /path/to/photos

# Upload with options
smugmug-cli upload /path/to/photos \
  --threads 8 \
  --dry-run \
  --album "Vacation 2025"

# Check status of local cache
smugmug-cli status

# Clear local cache
smugmug-cli cache clear
```

### Configuration File
Location: `~/.config/smugmug-cli/config.toml`

```toml
[auth]
api_key = "..."
api_secret = "..."
access_token = "..."
refresh_token = "..."

[upload]
threads = 4
retry_attempts = 3
timeout_seconds = 300

[deduplication]
enabled = true
cache_path = "~/.cache/smugmug-cli/hashes.db"

[album]
create_missing = true
naming_strategy = "folder_name"  # or "date", "custom"
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

### Upload Decision Logic
1. Calculate SHA256 of file
2. Check local cache for hash
3. If found in cache, verify image still exists in SmugMug (optional)
4. If not in cache, upload and store hash

## Docker Integration

### Dockerfile Strategy
```dockerfile
# Multi-stage build
FROM rust:1.75 as builder
# Build release binary

FROM debian:bookworm-slim
# Runtime with minimal dependencies
# Copy binary
# Add volume mounts for config and photos
```

### Docker Compose Usage
```yaml
services:
  smugmug-uploader:
    build: .
    volumes:
      - ./config:/root/.config/smugmug-cli
      - /path/to/photos:/photos:ro
    command: upload /photos --threads 8
```

## Development Phases

### Phase 1: Core Infrastructure
- [ ] Project setup with Cargo
- [ ] CLI argument parsing with clap
- [ ] Configuration file management
- [ ] OAuth authentication flow

### Phase 2: API Client
- [ ] SmugMug API client structure
- [ ] Authentication with token refresh
- [ ] Album/folder listing and creation
- [ ] Image upload implementation

### Phase 3: File Processing
- [ ] Directory scanner
- [ ] File hashing
- [ ] MIME type detection
- [ ] Upload queue

### Phase 4: Deduplication
- [ ] Local cache (sled database)
- [ ] Hash-based duplicate detection
- [ ] Cache management commands

### Phase 5: Polish
- [ ] Progress bars and UX
- [ ] Error handling and retries
- [ ] Docker packaging
- [ ] Documentation and README

## Testing Strategy

### Unit Tests
- API client methods
- Hash calculation
- Configuration parsing

### Integration Tests
- Full upload flow (with mock API)
- Cache operations
- Album creation

### Manual Testing
- Real SmugMug account testing
- Large file sets
- Network failure scenarios

## Future Enhancements
- Watch mode for continuous sync
- Selective sync patterns (include/exclude)
- Download/backup from SmugMug
- Web UI for monitoring
- Metrics and logging
