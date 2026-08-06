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

# The toolchain is whatever rust-toolchain.toml asks for, installed from that file rather
# than named again here. Naming it twice is how this broke: the image installed components
# and the Windows target against `stable`, the repository later pinned a numbered channel,
# and rustup fetched a second toolchain at run time without any of them.
#
# `rustup show` reads the file and installs what it names, so the pin, its components and
# its targets have exactly one definition. Bumping the channel needs no change here.
#
# The Windows target is a cross-*check* only, nothing Windows runs in this image. The
# release matrix builds `*-pc-windows-msvc` on real runners, but waiting for a tag to find
# out that a `#[cfg(windows)]` block does not compile is too slow a loop; the GNU target
# shares every one of those code paths and the same `windows-sys` bindings.
COPY rust-toolchain.toml /toolchain/rust-toolchain.toml
RUN cd /toolchain && rustup show

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
