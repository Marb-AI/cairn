# Phase 0 spike - tooling to verify D3 (SCIP indexers can handle the target repo).
# Everything runs in here; nothing is installed on the host (D13, architecture 2.1).

# -- Go tooling: scip-go (indexer) + scip (CLI, used to cross-check our stats) --
#
# Pinned, not `@latest`, for the same reason as `goplsbuild` below: `@latest` makes the
# image a function of the day it was built, and on 2026-11-08 scip-go's latest started
# requiring Go >= 1.26 while this stage pinned 1.25. Same version this repository's own
# `crates/cairn-cli/assets/indexers.Dockerfile` pins.
FROM golang:1.26-bookworm AS gotools
ENV CGO_ENABLED=0
# NOTE: scip-go moved from sourcegraph/ to scip-code/ (that is how the module
# declares itself in go.mod as of v0.2.x). Evidence for the "SCIP ecosystem
# maintenance is uneven" risk.
RUN go install github.com/scip-code/scip-go/cmd/scip-go@v0.2.9
# The scip CLI has `replace` directives in go.mod, so `go install` refuses it -
# build from a clone. It only cross-checks our own parser, so failure is not fatal.
RUN git clone --depth 1 https://github.com/sourcegraph/scip /tmp/scip \
 && (cd /tmp/scip && go build -o /go/bin/scip ./cmd/scip) \
 || echo "the scip CLI failed to build - continuing without it" > /go/bin/.scip-missing
RUN touch /go/bin/scip

# gopls needs a newer toolchain than scip-go does, so it gets its own stage rather
# than forcing the whole image up a Go version.
#
# Pinned, not `@latest`: on 2026-11-08 gopls v0.24.0 started requiring Go >= 1.27, which
# this stage did not have, and the fixture-regeneration build broke with no change to this
# file. Same fix as `crates/cairn-cli/assets/indexers.Dockerfile` made the same day, for
# the same reason - this image does not even run gopls (index.sh calls scip-go and
# scip-python only), but `spike` still COPYs from this stage, so it has to build.
FROM golang:1.27-bookworm AS goplsbuild
RUN go install golang.org/x/tools/gopls@v0.24.0

# rust-analyzer, pinned the same way: a specific toolchain tag rather than whatever
# `rustup component add` would fetch today. Same version as
# `crates/cairn-cli/assets/indexers.Dockerfile`'s own rust stage. `rust-src` alongside
# it, or rust-analyzer logs "can't load standard library, try installing `rust-src`"
# and resolves nothing that touches `std`.
FROM rust:1.99.1-bookworm AS rustbuild
RUN rustup component add rust-analyzer rust-src

# -- Runtime -----------------------------------------------------------------
# Python 3.13.7 to match `requires-python == 3.13.7` in srcpy/pyproject.toml.
FROM python:3.13.7-slim AS spike

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates git jq protobuf-compiler xz-utils time \
 && rm -rf /var/lib/apt/lists/*

# Full Go toolchain: scip-go shells out to `go list` on the target module.
#
# golang:1.26-bookworm, matching `gotools` above - this was still pinned at 1.25 after
# that stage was bumped to 1.26, which is the exact drift this file's own pinning
# comments exist to prevent: scip-go built expecting >= 1.26 while the copy here still
# shipped 1.25's standard library at runtime.
COPY --from=golang:1.26-bookworm /usr/local/go /usr/local/go
COPY --from=gotools /go/bin/scip-go /usr/local/bin/scip-go
COPY --from=gotools /go/bin/scip    /usr/local/bin/scip
COPY --from=goplsbuild /go/bin/gopls /usr/local/bin/gopls

# The whole toolchain directory, not just the rust-analyzer binary: it shells out to
# `cargo metadata`, the same relation scip-go has to `go list`. Copied from rustup's own
# toolchain path (not `/usr/local/cargo`, which holds only thin proxy binaries that need
# `RUSTUP_HOME` present to work) and put directly on `PATH` below - this one version,
# nothing auto-fetched, the same posture as `GOTOOLCHAIN=local`.
COPY --from=rustbuild /usr/local/rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu /opt/rust

# Node for scip-python (an npm package built on top of pyright).
COPY --from=node:22-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm

ENV PATH="/usr/local/go/bin:/opt/rust/bin:/root/.local/bin:${PATH}" \
    GOFLAGS=-mod=mod \
    GOTOOLCHAIN=local \
    PIP_ROOT_USER_ACTION=ignore

RUN npm install -g @sourcegraph/scip-python@latest pyright@latest \
 && pip install --no-cache-dir uv



WORKDIR /spike
COPY run.sh scipstat.py ./
RUN chmod +x run.sh

# bash explicitly: bind-mounting the script drops its exec bit
ENTRYPOINT ["bash", "/spike/run.sh"]
