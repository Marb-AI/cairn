#!/usr/bin/env bash
# Phase 0 spike. Input: /repo (read-only). Output: /out.
# Steps are ordered cheapest first; each can be run on its own:
#   docker compose run --rm spike go stats
set -uo pipefail

REPO_SRC=${REPO_SRC:-/repo}
WORK=${WORK:-/work}
OUT=${OUT:-/out}
STEPS=${*:-all}

log()  { printf '\n\033[1m-- %s\033[0m\n' "$*"; }
note() { printf '   %s\n' "$*"; }
run()  { note "\$ $*"; "$@"; }

want() { [[ "$STEPS" == "all" ]] || [[ " $STEPS " == *" $1 "* ]]; }

mkdir -p "$OUT"

# -- 0. working copy ----------------------------------------------------------
# The repo is mounted read-only; both indexers and uv need to write.
if [[ ! -d "$WORK/srcpy" ]]; then
  log "copying repo into $WORK (mount is read-only)"
  mkdir -p "$WORK"
  cp -a "$REPO_SRC/." "$WORK/"
fi
cd "$WORK"

note "tool versions:"
note "  scip-python  $(scip-python --version 2>/dev/null || echo '?')"
note "  scip-go      $(scip-go --version 2>&1 | head -1 || echo '?')"
note "  scip         $(scip --version 2>&1 | head -1 || echo '?')"
note "  go           $(go version 2>&1)"
note "  python       $(python3 --version 2>&1)"

# -- 1. Python dependencies ---------------------------------------------------
# Without them pyright cannot resolve django/fastapi/grpclib, and the numbers
# would measure something other than what we are asking about.
if want deps; then
  log "1. installing Python dependencies (uv)"
  cd "$WORK/srcpy"
  if [[ -f uv.lock ]]; then
    run uv sync --frozen --no-install-project 2>&1 | tail -8 \
      || run uv sync --no-install-project 2>&1 | tail -8 \
      || note "!! uv sync failed - continuing, numbers will be inflated"
  else
    run uv venv && run uv pip install -r pyproject.toml 2>&1 | tail -8
  fi
  cd "$WORK"
fi

# -- 2. scip-python -----------------------------------------------------------
# FINDING: scip-python is not zero-config. It needs
#   (a) pyright pointed at the venv, or django/fastapi/grpclib stay unresolved,
#   (b) --environment as a PATH to a JSON file, not inline JSON, and that file
#       must hold per-distribution {name, version, files[]} - `pip list` output
#       is rejected with "t.files is not iterable".
py_index() {
  local out=$1
  cd "$WORK/srcpy"
  if [[ ! -f pyrightconfig.json ]]; then
    cat > pyrightconfig.json <<'JSON'
{ "venvPath": ".", "venv": ".venv", "pythonVersion": "3.13",
  "exclude": ["**/node_modules", "**/.venv", "**/__pycache__"] }
JSON
    note "wrote pyrightconfig.json (into the working COPY, not the repo)"
  fi
  # django-types, not django-stubs: the latter needs a mypy plugin, which pyright
  # cannot run. Without it, attribute access on a model instance is unresolved, and
  # measurement showed the cost - LedgerEntry.ledger_category resolved to 4 sites
  # while its name appears in 33 files (eval/RESULTS.md, task C).
  if [[ "${DJANGO_TYPES:-1}" == "1" ]] && ! .venv/bin/python -c "import django_stubs_ext" 2>/dev/null; then
    note "installing django-types for pyright"
    uv pip install django-types >/dev/null 2>&1 || note "!! django-types install failed"
  fi
  .venv/bin/python - > /tmp/pyenv.json <<'PYENV' || echo '[]' > /tmp/pyenv.json
import json
from importlib.metadata import distributions
out = []
for d in distributions():
    try:
        name = d.metadata["Name"]
        if not name:
            continue
        out.append({"name": name, "version": d.version or "0",
                    "files": [str(f) for f in (d.files or [])]})
    except Exception:
        pass
print(json.dumps(out))
PYENV
  note "environment: $(python3 -c 'import json;print(len(json.load(open("/tmp/pyenv.json"))))' 2>/dev/null || echo 0) distributions"
  /usr/bin/time -f "   time: %E, RSS: %MkB" \
    scip-python index . \
      --project-name orders_api \
      --project-version spike \
      --environment /tmp/pyenv.json \
      --output "$out" 2>&1 | tail -8
  note "-> $(du -h "$out" 2>/dev/null | cut -f1 || echo MISSING)"
  cd "$WORK"
}

if want py; then
  log "2. scip-python"
  py_index "$OUT/py.scip"
fi

# -- 3. regenerate Python protobuf stubs (optional) ---------------------------
# NOTE: the stubs ARE committed in this repo. betterproto2 emits one large
# __init__.py per proto package (48,952 lines, 51 *ServiceBase classes), not
# *_pb2.py files - which is easy to misread as "no generated code present".
#
# Kept because it reproduces the codegen without the project's own pbgen image,
# and a repo whose artifacts really are absent is the case architecture section
# 4.6 is about. Options mirrored from tools/pbgen/pbgen.sh: without
# server_generation=async no *ServiceBase classes are emitted, and those carry
# the whole handler <-> proto binding.
if want pbgen; then
  log "3. regenerating Python protobuf stubs (betterproto2)"
  cd "$WORK/srcpy"
  # grpcio-tools for a protoc that bundles the well-known types; Debian's
  # protobuf-compiler does not ship them and google/protobuf/empty.proto fails.
  # ruff because the betterproto2 plugin shells out to it to format its output.
  run uv pip install betterproto2-compiler==0.7.1 grpcio-tools ruff 2>&1 | tail -3
  # 3rdparty is not generated but must be on the include path (annotations.proto)
  PROTOS=$(cd "$WORK/proto" && find . -type f -name '*.proto' ! -path './3rdparty/*' \
             | sed 's|^\./||' | sort)
  note "protoc: $(echo "$PROTOS" | wc -l) files (3rdparty as -I only)"
  mkdir -p "$WORK/srcpy/schema"
  # shellcheck disable=SC2086
  (cd "$WORK/proto" && PATH="$WORK/srcpy/.venv/bin:$PATH" \
     "$WORK/srcpy/.venv/bin/python" -m grpc_tools.protoc \
      --plugin=protoc-gen-python_betterproto2="$WORK/srcpy/.venv/bin/protoc-gen-python_betterproto2" \
      --python_betterproto2_out="$WORK/srcpy/schema" \
      --python_betterproto2_opt=server_generation=async \
      --python_betterproto2_opt=client_generation=async \
      --python_betterproto2_opt=google_protobuf_descriptors \
      -I . -I ./3rdparty -I /usr/include \
      $PROTOS 2>&1 | tail -10) \
    || note "!! generation failed"
  note "generated: $(find "$WORK/srcpy/schema" -name '*.py' | wc -l) .py files, $(find "$WORK/srcpy/schema" -name '*.py' -exec cat {} + | wc -l) lines"
  cd "$WORK"
fi

# -- 4. scip-go ---------------------------------------------------------------
if want go; then
  log "4. scip-go"
  cd "$WORK/srcgo"
  run go mod download 2>&1 | tail -5 || note "!! go mod download failed"
  /usr/bin/time -f "   time: %E, RSS: %MkB" \
    scip-go --output "$OUT/go.scip" 2>&1 | tail -8
  cd "$WORK"
fi

# -- 5. statistics ------------------------------------------------------------
if want stats; then
  log "5. statistics"
  for f in "$OUT"/*.scip; do
    [[ -e "$f" ]] || continue
    note "--- scip stats (cross-check for the custom parser) $(basename "$f") ---"
    scip stats --from "$f" 2>&1 | head -8 || note "(scip stats unavailable)"
  done
  python3 /spike/scipstat.py "$OUT"/*.scip 2>&1 | tee "$OUT/report.txt"
fi

log "done - output in $OUT"
ls -lh "$OUT" 2>/dev/null || true
