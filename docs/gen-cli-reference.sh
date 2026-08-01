#!/usr/bin/env bash
# Regenerate docs/cli-reference.md from the binary's own help.
#
#   docker compose run --rm ci bash docs/gen-cli-reference.sh > docs/cli-reference.md
#
# Generated rather than written, because a hand-kept list of twenty-five commands and
# their flags is a list that is wrong within a month. The prose that is worth writing by
# hand lives in the README and in docs/architecture.md.
set -euo pipefail

BIN=${BIN:-${CARGO_TARGET_DIR:-target}/debug/cairn}
[ -x "$BIN" ] || { echo "no binary at $BIN — build it first" >&2; exit 1; }

cat <<'HEADER'
# CLI reference

Every command, every flag, as the binary itself reports them.

**Generated** — do not edit by hand. Rebuild with:

```
docker compose run --rm ci bash docs/gen-cli-reference.sh > docs/cli-reference.md
```

The intended reader is an agent: it is exhaustive rather than friendly, and it assumes the
shape of the tool is already known. For that, start with the [README](../README.md), and
for when *not* to reach for a command at all, [`skill/SKILL.md`](../skill/SKILL.md).

## Two things that apply everywhere

**Exit codes are part of the contract.** `0` found, `1` nothing found, `2` bad query or an
unusable index, `3` degraded — the index is there but cannot be trusted for this answer.
An agent that treats a confident `0` over a broken index as an answer is the failure this
distinction exists to prevent.

**Every answer ends with an envelope**: `suppressed:` what was cut to fit the budget,
`unknown:` what the mechanism cannot see, `stale:` what has changed since indexing. A
section reading `none` is a claim; a missing section is a bug.

HEADER

printf '## cairn\n\n```\n'
"$BIN" --help 2>&1
printf '```\n'

for c in $("$BIN" --help 2>&1 | sed -n '/^Commands:/,/^Options:/p' | grep -oE '^  [a-z-]+' | tr -d ' '); do
	[ "$c" = help ] && continue
	printf '\n## cairn %s\n\n```\n' "$c"
	"$BIN" "$c" --help 2>&1
	printf '```\n'
done
