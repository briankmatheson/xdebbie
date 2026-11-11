# Dockerfile for CAPX (Cluster API Provider for XDebbie)
# Multi-stage build for minimal final image

# Stage 1: Build. Pin the builder to the same Debian release as the
# runtime: the binary links the builder's glibc/OpenSSL and must find the
# same (or newer) ones at run time.
FROM rust:1-trixie AS builder

WORKDIR /build

# Copy workspace files
COPY Cargo.toml ./
COPY crates ./crates

# Build CAPX
RUN cargo build --release --package capx

# Stage 2: Runtime
FROM debian:trixie-slim

# Install runtime dependencies (reqwest uses native-tls -> libssl)
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3t64 \
        && \
    rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /build/target/release/capx /usr/local/bin/capx

# Create non-root user
RUN useradd -r -u 1000 -s /bin/false capx

# Switch to non-root user
USER capx

# Run the controller
ENTRYPOINT ["/usr/local/bin/capx"]
