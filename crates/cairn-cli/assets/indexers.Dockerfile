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

# Both tools are PINNED, and that is the whole lesson of this stage.
#
# They were `@latest`, and on 2026-11-08 that stopped building: gopls v0.24.0 began
# requiring Go 1.27 while this stage pins 1.26 with GOTOOLCHAIN=local, so `cairn index`
# died at image build on every machine that had not built the image before. `@latest`
# makes the image a function of the day it was built, which is the opposite of what the
# version tag on it promises. Bumping these is a commit, as it should be.
#
# gopls gets its own stage because it is the one thing here that wants a newer toolchain
# than the indexers do: scip-go has to type-check the repository with the toolchain the
# repository expects, and that is the 1.26 below.
FROM golang:1.27-bookworm AS goplsbuild
ENV CGO_ENABLED=0
RUN go install golang.org/x/tools/gopls@v0.24.0

FROM golang:1.26-bookworm AS gobuild
ENV CGO_ENABLED=0
RUN go install github.com/scip-code/scip-go/cmd/scip-go@v0.2.9

# The flow provider: `cairn flow` asks it for the control flow inside one Go function.
#
# Compiled here, at image build time, because that is the only moment there is a network:
# a `go build` at query time would need one and D15 says a query has none. Its source is
# embedded in the cairn binary and written into this build context, so there is nothing
# to fetch and nothing to install on the host.
COPY cairnflow /src/cairnflow
RUN cd /src/cairnflow && go build -buildvcs=false -o /go/bin/cairnflow .

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
#
# Pinned, like the Go tools above and for the reason they had to be: `@latest` makes the
# image a function of the day it was built, and the version tag on it then promises
# something it cannot keep. These are the versions the image was last verified with -
# against a real monorepo - so bumping any of them is a commit someone can point at when
# an indexer starts behaving differently.
#
# The package managers too. cairn installs a project's declared dependencies itself, from
# its own lockfile into a volume cairn owns - reading the user's `node_modules` is not an
# option, because a team that builds in Docker has none and a team that has them today can
# stop tomorrow. Which manager runs is decided by the lockfile in the repository, so all of
# them have to be here.
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && npm install -g @sourcegraph/scip-python@0.6.8 pyright@1.1.414 \
      @sourcegraph/scip-typescript@0.6.1 typescript@7.0.2 \
      typescript-language-server@6.0.3 \
      pnpm@12.18.0 yarn@1.22.22 bun@1.4.4

# The Go toolchain as well as the indexer: scip-go runs `go list` against the module it is
# indexing, so the compiler has to be here too. gopls is the Go half of the live overlay.
COPY --from=gobuild /go/bin/cairnflow /usr/local/bin/cairnflow
COPY --from=gobuild /go/bin/scip-go /usr/local/bin/scip-go
COPY --from=goplsbuild /go/bin/gopls /usr/local/bin/gopls
COPY --from=golang:1.26-bookworm /usr/local/go /usr/local/go

# Caches under /tmp because the container runs as the calling user, who owns nothing else
# in here. Without this both toolchains fail trying to write to a home directory that is
# not theirs.
# No `GOFLAGS=-mod=mod`, and that is deliberate twice over.
#
# Go refuses it outright in workspace mode - "-mod may only be set to readonly or vendor
# when in workspace mode" - so on any repository with a `go.work` the Go indexer died
# before it read a line. Measured on a 23-module workspace: scip-go exited on the first
# package load.
#
# And it should not be set anyway: `-mod=mod` lets the toolchain rewrite the `go.mod` and
# `go.sum` of the repository being indexed. An indexer that edits the tree it is reading
# is a tool nobody can point at a checkout they care about. The default, `readonly`, is
# the right posture: a repository whose module graph does not resolve fails to index and
# says so, which is the same thing it would do to a build.
ENV PATH="/usr/local/go/bin:${PATH}" \
    GOTOOLCHAIN=local \
    GOCACHE=/tmp/go-build \
    GOMODCACHE=/tmp/go-mod \
    XDG_CACHE_HOME=/tmp/cache \
    HOME=/tmp

WORKDIR /repo
