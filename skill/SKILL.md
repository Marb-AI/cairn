---
name: cairn
description: Indexed code navigation - callers, references, call paths, dead code, blast radius, across Python and Go. Use for questions about how code connects; not for prose, config or string literals.
---

# cairn

## Reach for it when

The question is about **how code connects** — who calls this, what breaks if I change
it, what does production never reach, which tests cover it, how does A get to B.

## Do not, and save the round trip

- the answer is in one file you already know → just read it
- prose, comments, config, string literals, docs → grep
- the code is not in this repo → nothing is indexed for it

`cairn status` says what is indexed. Answers end with `unknown:` / `suppressed:` /
`stale:` — read them, they are where the honesty lives.

## Commands

```
cairn context "<feature>"        entry point when you have no symbol name
cairn symbol <name>              -> [handles]; everything else takes one

cairn refs <h> --context auto --repo <dir> --budget N
                                 use sites, each with the enclosing function and its
                                 source. `auto` spends the budget per site: few sites
                                 get a block, many get one line
cairn graph <h> --aspect callers|calls|impls|tests [--depth N] [--exclude-tests]
cairn path <a> <b>               how one reaches the other
cairn expand <h> --detail body --repo <dir>

cairn outline <path>             what a module holds, and what is actually used
cairn unreached <path>           what production never calls (one call, not one per symbol)
cairn usage <h>                  use sites grouped by file
cairn weaklinks <h>              string literals naming it - candidate dynamic calls
```

Prefer the three set-shaped commands (`outline`, `unreached`, `usage`) when the question
is about a package or a change's blast radius. They answer in one call what the
per-symbol commands answer in dozens.

## Budget

`--budget <tokens>` is a ceiling: the tool fills it with the highest-ranked rows and
reports what it dropped. Better than guessing `--limit` and asking twice.

## Trust

- `stale: not tracked` means no daemon is watching; the index may be behind.
- An attribute on a type (`Model.field`) is a **lower bound** — attribute access only
  resolves where the holder's type is known, which for ORM instances it often is not.
  The tool says so when it applies. Cross-check with grep.
- `[L1-W, unverified]` is a lexical guess, not a fact.
- Exit codes: `0` found, `1` nothing, `2` bad query, `3` degraded index.

## Writing back

Findings survive reindexing and are re-checked when the code moves:

```
cairn note <h> --summary "..."          cairn link <a> <b> --note "why"
cairn concept add <name> --note "..."   cairn concept link <name> <h> --rel entry-point
```
