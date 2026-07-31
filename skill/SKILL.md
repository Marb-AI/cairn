---
name: cairn
description: Navigate an indexed codebase - find symbols, callers, call paths, tests and endpoints without grepping. Use when you need to know who calls something, what a change breaks, where a feature lives, or which tests cover code. Works across languages (a Go caller of a Python handler is one query).
---

# cairn

A local index of this codebase. It answers structural questions exactly, in far fewer
tokens than reading files, and **tells you what it does not know** instead of guessing.

Start here when you land in unfamiliar code. Every answer ends with `unknown:`,
`suppressed:` and `stale:` — read them; they are the difference between a fact and a
guess.

## The first move

```
cairn context "<what you are looking for>"     # a feature, a domain, a concept
cairn symbol <name>                            # you already know the name
cairn status                                   # what is indexed, how stale
```

Both return `[handles]` — two to four characters. Everything else takes a handle.

## Answering questions

| question | command |
|---|---|
| who calls this? | `cairn graph <h> --aspect callers` |
| what does it call? | `cairn graph <h> --aspect calls` |
| what implements this interface? | `cairn graph <h> --aspect impls` |
| which tests cover it? | `cairn graph <h> --aspect tests` |
| how does A reach B? | `cairn path <a> <b>` |
| where is it used? | `cairn refs <h>` |
| show me the code | `cairn expand <h> --detail body --repo <dir>` |
| is anything referring to it dynamically? | `cairn weaklinks <h>` |

## Use this instead of grep when

- **finding usages of a symbol.** grep matches comments, strings and same-named symbols
  in unrelated modules, and misses calls through an import alias or across the gRPC
  boundary between languages. `cairn refs` does not.
- **judging the blast radius of a change.** `cairn graph <h> --aspect callers --depth 2`
  answers in one call what grep answers in ten.
- **finding where a feature lives.** `cairn context` searches names, module paths and
  documentation at once, so it finds terms that appear in no identifier.

Keep using grep for: string literals, config values, comments, and anything in a file
the index does not cover. `cairn verify` says what that is.

## Controlling how much you get back

Four independent knobs. Default is a skeleton — names and locations, no code.

```
--detail skeleton|signature|doc|body    how much of each symbol   (body needs --repo)
--depth N --fanout N                    how far the walk goes
--aspect ...                            which relation is followed
--budget <tokens>                       hard ceiling; the tool picks the best rows
                                        and reports what it dropped
--view tree|list                        tree shows how each node was reached
```

`--budget` is usually better than guessing a limit: say what you can afford and the
tool fills it with the highest-ranked rows.

**For audit or review passes**, walk with bodies and a ceiling:

```
cairn graph <h> --aspect callers --detail body --repo <dir> --budget 3000
```

## Trust and staleness

- `stale: none` means the files in that answer match what was indexed.
- `stale: not tracked` means no daemon is running and changes are invisible. Start one
  with `cairn daemon --repo <dir>`, or check once with `cairn verify --repo <dir>`.
- `cairn live <path>` shows what a changed file contains *now*, from the language
  server, against what the index recorded. Use it after editing.
- Exit codes: `0` found, `1` nothing found, `2` bad query, `3` degraded index.

Anything labelled `[L1-W, unverified]` or `weak links` is a lexical guess — a string
literal that happens to spell a symbol name. Read the site before relying on it.

## Writing back what you learn

The index is deterministic and never invents anything, so what you work out yourself is
worth recording. It survives reindexing.

```
cairn note <h> --summary "..."                     what this does
cairn link <a> <b> --note "why"                    a connection the static pass misses
cairn concept add <name> --note "..."              a name for a part of the system
cairn concept link <name> <h> --rel entry-point    attach code to it
```

Claims are anchored to the code they describe. When that code changes they are flagged
for review rather than silently trusted — so recording something is safe, and reading
something back always tells you whether it still holds.
