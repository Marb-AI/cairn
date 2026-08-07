# The indexers cairn drives, and nothing else.
#
# Built on the user's machine by `cairn index`, from this file as it is embedded in the
# binary — there is no registry to reach and nothing to pull. Docker is the single
# dependency; the toolchains the indexers need live in here rather than on the host, so
# nobody has to install Go, Node or Python to index a repository.
#
# Tagged with cairn's own version, so upgrading cairn rebuilds it and an old image can
# never quietly produce indexes a newer cairn does not expect. One image serves every
# repository on the machine.

# scip-go needs a recent Go toolchain to build itself.
FROM golang:1.26-bookworm AS gobuild
ENV CGO_ENABLED=0
RUN go install github.com/scip-code/scip-go/cmd/scip-go@latest \
 && go install golang.org/x/tools/gopls@latest

# Python is the base, not an afterthought: scip-python shells out to `pip` to enumerate the
# environment and fails outright without one. Learned by building this the other way round
# and watching it die with "Could not find valid pip command".
FROM python:3.13-slim

# git: scip-go asks it for repository metadata, and its absence turns into a confusing
# indexer error rather than a clear one.
RUN apt-get update && apt-get install -y --no-install-recommends \
      git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Node, for scip-python, which is an npm package built on pyright.
COPY --from=node:22-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
#
# pyright as well as scip-python: the indexer bundles its own analysis but does not ship
# `pyright-langserver`, and that is what the daemon drives to answer about a file that has
# been edited since the index was built.
#
# scip-typescript and the TypeScript language server come from the same npm tree. The
# indexer needs the *project's* dependencies present to do anything at all — a tsconfig
# that says `extends: "expo/tsconfig.base"` cannot even be read without them — but that is
# the repository's business, not the image's; what belongs here is the indexer itself.
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install -g @sourcegraph/scip-python@latest pyright@latest \
      @sourcegraph/scip-typescript@latest typescript typescript-language-server@latest

# The Go toolchain as well as the indexer: scip-go runs `go list` against the module it is
# indexing, so the compiler has to be here too. gopls is the Go half of the live overlay.
COPY --from=gobuild /go/bin/scip-go /usr/local/bin/scip-go
COPY --from=gobuild /go/bin/gopls /usr/local/bin/gopls
COPY --from=golang:1.26-bookworm /usr/local/go /usr/local/go

# Caches under /tmp because the container runs as the calling user, who owns nothing else
# in here. Without this both toolchains fail trying to write to a home directory that is
# not theirs.
ENV PATH="/usr/local/go/bin:${PATH}" \
    GOFLAGS=-mod=mod \
    GOTOOLCHAIN=local \
    GOCACHE=/tmp/go-build \
    GOMODCACHE=/tmp/go-mod \
    XDG_CACHE_HOME=/tmp/cache \
    HOME=/tmp

WORKDIR /repo
