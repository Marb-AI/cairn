#!/usr/bin/env bash
# Index the fixture corpus. Runs inside the spike image, which is where the indexers live.
#
#   docker compose -f crates/cairn-cli/tests/fixtures/compose.yaml run --rm scip
#
# Output goes to crates/cairn-cli/tests/fixtures/index/ and is committed alongside the
# sources it was built from. Re-run whenever anything under corpus/ changes.
set -euo pipefail

CORPUS=${CORPUS:-/corpus}
OUT=${OUT:-/out}

log() { printf '\n\033[1m-- %s\033[0m\n' "$*"; }

mkdir -p "$OUT"

log "tool versions"
printf '   scip-go      %s\n' "$(scip-go --version 2>&1 | head -1)"
printf '   scip-python  %s\n' "$(scip-python --version 2>&1 | head -1)"

# The corpus is mounted read-only so indexing cannot edit the tree it describes; both
# indexers want to write next to the sources, so they work on a copy.
WORK=$(mktemp -d)
cp -a "$CORPUS/." "$WORK/"

log "scip-go"
(cd "$WORK/srcgo" && go mod tidy >/dev/null 2>&1 || true)
(cd "$WORK/srcgo" && scip-go --output "$OUT/go.scip" 2>&1 | tail -5)

log "scip-python"
(cd "$WORK/srcpy" && scip-python index . \
    --project-name telemetry-alerting \
    --project-version fixture \
    --output "$OUT/py.scip" 2>&1 | tail -5)

# The container runs as root; without this the committed files belong to root on the host.
if [ -n "${HOST_UID:-}" ]; then
	chown "$HOST_UID:${HOST_GID:-$HOST_UID}" "$OUT"/*.scip
fi

log "done"
ls -l "$OUT"
