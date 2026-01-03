# Multi-stage build for minimal image size
FROM rust:1.83-alpine as builder

WORKDIR /usr/src/smugmug-cli

# Install build dependencies for Alpine
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static

# Copy manifests
COPY Cargo.toml ./

# Create dummy main.rs and lib.rs to cache dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs

# Build dependencies (cached layer)
RUN cargo build --release && rm -rf src

# Copy actual source code
COPY src ./src

# Build the actual application
RUN cargo build --release

# Runtime stage - use Alpine for minimal size
FROM alpine:latest

# Install only runtime dependencies
RUN apk add --no-cache ca-certificates libgcc

# Copy binary from builder
COPY --from=builder /usr/src/smugmug-cli/target/release/smugmug-cli /usr/local/bin/smugmug-cli

# Create directories for config and cache
RUN mkdir -p /root/.config/smugmug-cli /root/.cache/smugmug-cli

# Volumes for configuration and photos
VOLUME ["/root/.config/smugmug-cli", "/photos"]

ENTRYPOINT ["smugmug-cli"]
CMD ["--help"]
