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

```bash
docker build -t smugmug-cli .
docker run -v ~/.config/smugmug-cli:/root/.config/smugmug-cli \
           -v /path/to/photos:/photos:ro \
           smugmug-cli upload /photos
```

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

[deduplication]
enabled = true
```

## Commands

- `init` - Initialize configuration and authenticate with SmugMug (interactive)
- `test-auth` - Test that your credentials are working
- `upload <PATH>` - Upload photos from the specified path
- `status` - Show cache statistics and upload history
- `cache clear` - Clear the local deduplication cache

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
