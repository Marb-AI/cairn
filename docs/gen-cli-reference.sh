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

## Machine-readable output

`--format json` prints the same answer as one `cairn.answer/1` object, with the envelope
as fields rather than as lines. Text is the default and stays the primary format for an
agent: braces and repeated keys cost several times the tokens of an aligned table, and
cheaper context is the point. JSON is for a program — a wrapper, an editor, a check in CI.

```
{
  "schema": "cairn.answer/1",
  "command": "refs",
  "rows": 10,
  "truncated": false,
  "unknown": [],
  "suppressed": [],
  "stale": { "tracked": false, "dirty": [], "mentioned": ["srcgo/..."], "notes": ["..."] },
  "body": "...the same text as above the envelope lines...",
  "data": null
}
```

What holds across both formats:

* **Exit codes are identical.** The code is the contract; it cannot depend on how the
  answer was printed.
* **The envelope is always present.** An empty `unknown` means none; a missing one would
  be the same silent error a missing `unknown:` line is. `stale.tracked` is the
  distinction a bare empty list cannot make: `false` means nothing was watching the tree,
  not that nothing had changed.
* **`body` is the text answer** without the three envelope lines, since those are fields
  here. `--budget` cuts a JSON answer exactly as it cuts a text one, and what it cut is
  in `suppressed`.
* **`data`** is the answer as fields. It is `null` for commands that still have only a
  text renderer; the envelope around it is there either way.

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
