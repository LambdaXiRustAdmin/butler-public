# Multi-stage build for Butler server (MCP/HTTP only - no GUI)
# Focused on the background server binary.

FROM rust:1.88-slim AS builder

WORKDIR /usr/src/app

# Install build dependencies (minimal, no GUI/X11 libs)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./
COPY code_graph/Cargo.toml ./code_graph/
COPY cli/Cargo.toml ./cli/

# Copy source
COPY code_graph ./code_graph
COPY cli ./cli

# Build HTTP/MCP server (process name in btop/ps: butler-server)
RUN cargo build --release -p cli --bin butler-server

# Runtime stage - slim Debian for small image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Binary basename is what btop/ps show (keep name butler-server, not "server")
COPY --from=builder /usr/src/app/target/release/butler-server /app/butler-server

EXPOSE 8002

ENV BUTLER_HOST=0.0.0.0

ENTRYPOINT ["./butler-server"]
