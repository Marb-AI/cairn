# Development and build image. Nothing is installed on the host (D13).
# gopls needs a newer Go toolchain than anything else here, so it gets its own stage.
FROM golang:1.26-bookworm AS goplsbuild
RUN go install golang.org/x/tools/gopls@latest

FROM rust:1-bookworm AS dev

RUN apt-get update && apt-get install -y --no-install-recommends \
      protobuf-compiler sqlite3 pkg-config curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Language servers for the dirty overlay (architecture 4.2). They live in the image so
# that nothing is installed on the host (D13) and so `cairn daemon` works out of the box.
COPY --from=goplsbuild /go/bin/gopls /usr/local/bin/gopls
COPY --from=node:22-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install -g pyright@latest

RUN rustup component add clippy rustfmt

# Cargo caches live in volumes (see compose.yaml) so rebuilds are incremental.
ENV CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target \
    RUST_BACKTRACE=1

WORKDIR /w
