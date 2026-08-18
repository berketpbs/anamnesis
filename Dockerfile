# Multi-stage build for anamnesis
# Stage 1: Build
FROM rust:1.95-slim as builder

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
COPY evals ./evals

# Build the CLI binary
RUN cargo build --release -p anamnesis-cli

# Stage 2: Runtime
FROM debian:bookworm-slim

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
ENV ANAMNESIS_DB=/root/.anamnesis

# Use tini as init system
ENTRYPOINT ["/usr/bin/tini", "--"]

# Default command
CMD ["anamnesis", "serve", "--bind", "0.0.0.0", "--port", "8080"]

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
