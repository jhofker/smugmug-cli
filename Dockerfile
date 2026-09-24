# Multi-stage build for minimal image size
FROM rust:1.83-alpine AS builder

WORKDIR /usr/src/smugmug-cli

# Install build dependencies for Alpine
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Build the application
RUN cargo build --release --locked

# Runtime stage - use Alpine for minimal size
FROM alpine:latest

# Install runtime dependencies including su-exec for privilege dropping
RUN apk add --no-cache ca-certificates libgcc su-exec

# Copy binary from builder
COPY --from=builder /usr/src/smugmug-cli/target/release/smugmug-cli /usr/local/bin/smugmug-cli

# Copy entrypoint script
COPY docker-entrypoint.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Create default user (will be modified at runtime)
# Use existing 'users' group (GID 100) if available, otherwise create smugmug group
RUN adduser -D -u 99 -G users smugmug && \
    mkdir -p /home/smugmug/.config/smugmug-cli /home/smugmug/.cache/smugmug-cli && \
    chown -R smugmug:users /home/smugmug

# Volumes for configuration and photos
VOLUME ["/home/smugmug/.config/smugmug-cli", "/photos"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh", "smugmug-cli"]
CMD ["--help"]
