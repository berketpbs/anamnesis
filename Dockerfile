# Multi-stage build for anamnesis
#
# The two stages name the same Debian release on purpose. A binary linked
# against the builder's glibc will not start on an older runtime, and nothing
# about that failure appears while building — the image is produced, pushed,
# and only refuses to run. `rust:1.95-slim` alone tracks whichever release the
# Rust image happens to be on, which is how this pair silently came apart:
# trixie's glibc 2.41 above, bookworm's 2.36 below, and
# `version 'GLIBC_2.39' not found` the first time anyone ran it.
#
# Stage 1: Build
FROM rust:1.95-slim-trixie AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    git \
    sqlite3 \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace and source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

# Build the CLI binary
# `--locked`: the lock file is copied in on purpose, and a build that quietly
# resolved different versions would make the image something other than what
# CI tested.
RUN cargo build --release --locked -p anamnesis-cli

# Stage 2: Runtime — the same release as the builder above.
FROM debian:trixie-slim

WORKDIR /root

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    sqlite3 \
    ca-certificates \
    tini \
    git \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/anamnesis /usr/local/bin/

# Create directories
RUN mkdir -p /root/.anamnesis /workspace

# Set environment
ENV ANAMNESIS_DATA_DIR=/root/.anamnesis

# Use tini as init system
ENTRYPOINT ["/usr/bin/tini", "--"]

# Default command.
#
# `--allow-anonymous` because a container binds 0.0.0.0 in order to be reachable
# through a published port at all, not because its memory is meant to be public
# — and whether anyone outside can reach it is decided by `-p` on the host,
# where this process cannot see it. Set ANAMNESIS_TOKEN (or ANAMNESIS_TOKENS)
# and the server requires it regardless of this flag.
CMD ["anamnesis", "serve", "--bind", "0.0.0.0", "--port", "8080", "--allow-anonymous"]

# Expose ports
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD anamnesis status || exit 1

# Labels
LABEL org.opencontainers.image.title="Anamnesis" \
      org.opencontainers.image.description="Long-term memory for AI coding agents" \
      org.opencontainers.image.url="https://github.com/berketpbs/anamnesis" \
      org.opencontainers.image.documentation="https://github.com/berketpbs/anamnesis/tree/main/docs"
