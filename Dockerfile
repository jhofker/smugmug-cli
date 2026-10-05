# Multi-stage build for minimal image size
FROM rust:1.89-alpine AS builder

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

# Install runtime dependencies: su-exec for privilege dropping, tzdata so TZ
# sets the time zone videos are dated in, util-linux for ionice
RUN apk add --no-cache ca-certificates libgcc su-exec tzdata util-linux

# Copy binary from builder
COPY --from=builder /usr/src/smugmug-cli/target/release/smugmug-cli /usr/local/bin/smugmug-cli

# License notice for rawler (LGPL-2.1), which the binary includes
COPY THIRD-PARTY-NOTICES.md /usr/share/doc/smugmug-cli/
COPY licenses /usr/share/doc/smugmug-cli/licenses

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
