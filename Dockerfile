# Development and build image. Nothing is installed on the host (D13).
# gopls needs a newer Go toolchain than anything else here, so it gets its own stage.
FROM golang:1.26-bookworm AS goplsbuild
RUN go install golang.org/x/tools/gopls@latest

# Everything needed to compile and test cairn, and nothing else.
#
# Split out from `dev` so CI does not have to build a Go toolchain and an npm tree to run
# `cargo test`. The language servers belong to the daemon's dirty overlay, not to the
# build, and pulling them in would roughly triple the cost of an image that gets rebuilt
# on every push.
FROM rust:1-bookworm AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
      protobuf-compiler sqlite3 pkg-config curl ca-certificates \
      gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/*

# Components and targets go on `stable` by name, not on the image's default toolchain.
# The base image pins a numbered toolchain, while rust-toolchain.toml asks for `stable`,
# so rustup fetches a second one at first run — anything installed here under the default
# would simply not be there when the build actually runs.
#
# The Windows target is a cross-*check* only, nothing Windows runs in this image. The
# release matrix builds `*-pc-windows-msvc` on real runners, but waiting for a tag to find
# out that a `#[cfg(windows)]` block does not compile is too slow a loop; the GNU target
# shares every one of those code paths and the same `windows-sys` bindings.
RUN rustup toolchain install stable --profile minimal \
      --component clippy --component rustfmt \
      --target x86_64-pc-windows-gnu \
 && rustup default stable

# Cargo caches live in volumes (see compose.yaml) so rebuilds are incremental.
ENV CARGO_HOME=/cargo \
    CARGO_TARGET_DIR=/target \
    RUST_BACKTRACE=1

WORKDIR /w

FROM build AS dev

# Language servers for the dirty overlay (architecture 4.2). They live in the image so
# that nothing is installed on the host (D13) and so `cairn daemon` works out of the box.
COPY --from=goplsbuild /go/bin/gopls /usr/local/bin/gopls
COPY --from=node:22-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install -g pyright@latest
