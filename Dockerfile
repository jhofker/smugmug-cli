# Multi-stage build for minimal image size
FROM rust:1.75 as builder

WORKDIR /usr/src/smugmug-cli

# Copy manifests
COPY Cargo.toml ./

# Create dummy main.rs to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies (cached layer)
RUN cargo build --release && rm -rf src

# Copy actual source code
COPY src ./src

# Build the actual application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /usr/src/smugmug-cli/target/release/smugmug-cli /usr/local/bin/smugmug-cli

# Create directories for config and cache
RUN mkdir -p /root/.config/smugmug-cli /root/.cache/smugmug-cli

# Volumes for configuration and photos
VOLUME ["/root/.config/smugmug-cli", "/photos"]

ENTRYPOINT ["smugmug-cli"]
CMD ["--help"]
