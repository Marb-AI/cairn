#!/usr/bin/env bash
# The golden walk against a real index, written to a directory instead of compared.
#
# The committed recordings in crates/cairn-cli/tests/golden/ are made from the fixture
# corpus, because a recording that differs per machine cannot be committed. The fixture is
# 16 files; the codebases cairn is for are thousands. This runs the same walk, the same
# commands and the same scrubbing against a real checkout, so a change can be diffed where
# it matters:
#
#   scripts/golden-local.sh ~/work/backend /tmp/golden-before
#   # ... make the change, rebuild ...
#   scripts/golden-local.sh ~/work/backend /tmp/golden-after
#   diff -ru /tmp/golden-before /tmp/golden-after
#
# Not part of CI and not a test: nothing here asserts anything. The diff is the output,
# and reading it is the point.
#
# The repository is mounted read-write because a query opens the index, which writes a
# WAL beside it, and the session log lives there too. Nothing cairn does here changes the
# index itself.
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    cat >&2 <<'USAGE'
usage: scripts/golden-local.sh <repo-root> <output-dir> [sample]

  repo-root   an indexed checkout: <repo-root>/.cairn/index.sqlite must exist
  output-dir  created if absent; one .txt per command is written into it
  sample      how many symbols to walk (default 24, the committed recordings' sample)
USAGE
    exit 2
fi

repo=$(cd "$1" && pwd)
out=$2
sample=${3:-24}

if [[ ! -f "$repo/.cairn/index.sqlite" ]]; then
    echo "no index at $repo/.cairn/index.sqlite - run \`cairn index\` there first" >&2
    exit 2
fi

mkdir -p "$out"
out=$(cd "$out" && pwd)
here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# The build is Docker-only (D13), so the walk runs where the binary does. `ci` is the lean
# image; the language servers in `dev` are not needed because nothing here starts a daemon.
docker compose -f "$here/compose.yaml" run --rm \
    --volume "$repo:/golden-repo" \
    --volume "$out:/golden-out" \
    --env "CAIRN_GOLDEN_DB=/golden-repo/.cairn/index.sqlite" \
    --env "CAIRN_GOLDEN_REPO=/golden-repo" \
    --env "CAIRN_GOLDEN_OUT=/golden-out" \
    --env "CAIRN_GOLDEN_SAMPLE=$sample" \
    ci cargo test --test golden -- --nocapture

echo "recordings in $out"
