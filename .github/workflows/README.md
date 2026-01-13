# GitHub Actions Workflows

## docker-publish.yml

Builds and publishes multi-architecture Docker images to GitHub Container Registry (ghcr.io).

### Triggers

- **Version tags** (e.g., `0.2.1`): Builds and publishes images (matches binary release workflow)
- **Pull requests**: Builds only for validation (doesn't publish)
- **Manual dispatch**: Can be triggered manually from GitHub Actions UI

### Image Tags

When you push a version tag like `0.2.1`, the workflow creates:

- `latest` - Latest release version
- `0.2.1` - Full semantic version
- `0.2` - Major.minor version
- `0` - Major version only

For pull requests (validation only, not published):
- `pr-123` - Pull request number

### Platforms

Images are built for:
- `linux/amd64` (x86_64)
- `linux/arm64` (Apple Silicon, ARM servers)

### Features

- **Multi-stage build**: Minimal image size (~23MB)
- **Layer caching**: Uses GitHub Actions cache for faster builds
- **Build attestation**: Generates provenance for supply chain security
- **Non-root user**: Runs as UID 99, GID 100 by default (configurable via PUID/PGID)

### Using Published Images

```bash
# Pull latest
docker pull ghcr.io/jhofker/smugmug-cli:latest

# Pull specific version
docker pull ghcr.io/jhofker/smugmug-cli:0.2.1

# Run
docker run ghcr.io/jhofker/smugmug-cli:latest --version
```

### Publishing a New Release

Both the Docker images and binary releases are published when you push a version tag:

```bash
# Tag the release
git tag 0.2.2
git push origin 0.2.2

# Both workflows will run:
# 1. release.yml - Builds binaries and creates GitHub Release
# 2. docker-publish.yml - Builds and publishes Docker images to GHCR
```

### Permissions

The workflow requires:
- `contents: read` - Read repository contents
- `packages: write` - Push to GitHub Container Registry
- `id-token: write` - Generate attestations

## release.yml

Managed by [cargo-dist](https://github.com/axodotdev/cargo-dist). Builds and publishes native binaries for multiple platforms when version tags are pushed.
