# Build stage
FROM rust:1-slim-bookworm AS builder

WORKDIR /app

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    git \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY demo ./demo
COPY static ./static
COPY tests ./tests
COPY benches ./benches
COPY config.example.toml ./

# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    git \
    libssl3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 sentryclaw

# Copy binary from builder
COPY --from=builder /app/target/release/sentryclaw /usr/local/bin/sentryclaw
COPY --from=builder /app/config.example.toml ./config.example.toml

# Set ownership
RUN chown -R sentryclaw:sentryclaw /app

USER sentryclaw

EXPOSE 3000

ENV RUST_LOG=info
ENV CONFIG_PATH=/app/config.toml

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

CMD ["sentryclaw"]
