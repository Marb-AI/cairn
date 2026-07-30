# Development and build image. Nothing is installed on the host (D13).
FROM rust:1-bookworm AS dev

RUN apt-get update && apt-get install -y --no-install-recommends \
      protobuf-compiler sqlite3 pkg-config \
 && rm -rf /var/lib/apt/lists/*

RUN rustup component add clippy rustfmt

# Cargo caches live in volumes (see compose.yaml) so rebuilds are incremental.
ENV CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target \
    RUST_BACKTRACE=1

WORKDIR /w
