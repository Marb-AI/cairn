# Phase 0 spike - tooling to verify D3 (SCIP indexers can handle the target repo).
# Everything runs in here; nothing is installed on the host (D13, architecture 2.1).

# -- Go tooling: scip-go (indexer) + scip (CLI, used to cross-check our stats) --
FROM golang:1.25-bookworm AS gotools
ENV CGO_ENABLED=0
# NOTE: scip-go moved from sourcegraph/ to scip-code/ (that is how the module
# declares itself in go.mod as of v0.2.x). Evidence for the "SCIP ecosystem
# maintenance is uneven" risk. It also requires Go >= 1.25.
RUN go install github.com/scip-code/scip-go/cmd/scip-go@latest
# The scip CLI has `replace` directives in go.mod, so `go install` refuses it -
# build from a clone. It only cross-checks our own parser, so failure is not fatal.
RUN git clone --depth 1 https://github.com/sourcegraph/scip /tmp/scip \
 && (cd /tmp/scip && go build -o /go/bin/scip ./cmd/scip) \
 || echo "scip CLI se nepodařilo postavit — pokračuji bez něj" > /go/bin/.scip-missing
RUN touch /go/bin/scip

# gopls needs a newer toolchain than scip-go does, so it gets its own stage rather
# than forcing the whole image up a Go version.
FROM golang:1.26-bookworm AS goplsbuild
RUN go install golang.org/x/tools/gopls@latest

# -- Runtime -----------------------------------------------------------------
# Python 3.13.7 to match `requires-python == 3.13.7` in srcpy/pyproject.toml.
FROM python:3.13.7-slim AS spike

RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates git jq protobuf-compiler xz-utils time \
 && rm -rf /var/lib/apt/lists/*

# Full Go toolchain: scip-go shells out to `go list` on the target module.
COPY --from=golang:1.25-bookworm /usr/local/go /usr/local/go
COPY --from=gotools /go/bin/scip-go /usr/local/bin/scip-go
COPY --from=gotools /go/bin/scip    /usr/local/bin/scip
COPY --from=goplsbuild /go/bin/gopls /usr/local/bin/gopls

# Node for scip-python (an npm package built on top of pyright).
COPY --from=node:22-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm

ENV PATH="/usr/local/go/bin:/root/.local/bin:${PATH}" \
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
