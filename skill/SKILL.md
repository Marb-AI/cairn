---
name: cairn
description: Indexed code navigation - callers, references, call paths, dead code, blast radius, across Python and Go. Use for questions about how code connects; not for prose, config or string literals.
---

# cairn

Pre-alpha. Indexes Python and Go only — for anything else, nothing is indexed and grep is
the tool. Expect the command surface to change.

## Reach for it when

The question is about **how code connects** — who calls this, what breaks if I change
it, what does production never reach, which tests cover it, how does A get to B.

## Do not, and save the round trip

- the answer is in one file you already know → just read it
- prose, comments, config, string literals, docs → grep
- the code is not in this repo → nothing is indexed for it

## One index per repository, found from where you are

cairn resolves the index from the working directory: it looks upward for the repository
root and reads `.cairn/index.sqlite` there. **Run it inside the repository you are asking
about.** With `repo1/` and `repo2/` checked out side by side, the same command gives
different answers depending on where you stand, and run from neither it reports no index
rather than guessing.

If you get `no index at ...` (exit 3), you are almost certainly in the wrong directory —
check before concluding the tool is not set up.

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
cairn path <a> <b> [--detail body --repo <dir>]
                                 how one reaches the other. With `--detail body` you get
                                 the whole chain AND every hop's source in one call - use
                                 it to answer "trace X to Y and say what happens\" without
                                 opening a single file
cairn reaches <h> [--outgoing]   who reaches it ACROSS a gRPC boundary, in the other
                                 language. grep cannot answer this at all: the two
                                 sides share no identifier. Ask it about the handler
                                 CLASS for every RPC it serves with the callers of each,
                                 or a METHOD for just that one. Chains of services need
                                 one call per hop
cairn expand <h> --detail body --repo <dir>

cairn affects <h>                EVERY deployed service a change here touches: in-process,
                                 then each network hop with the RPC that carries it. One
                                 call - do not assemble this from runs/reaches/topology
cairn topology                   deployed services and what starts each one
cairn runs <h>                   which services actually run this code. The filesystem
                                 cannot say: many services share one source tree
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
- Asked about an attribute on a type (`Model.field`), the tool will say **USE GREP FOR
  THIS ONE** and give you the command. Take it: attribute access resolves only where the
  holder's type is known, and for ORM instances it usually is not. Do not treat the short
  list it still prints as a blast radius.
- `ALL ... ARE IN ONE FILE: <path>` means stop querying and read that file.
- `[L1-W, unverified]` is a lexical guess, not a fact.
- A miss reports what *is* indexed. If your target is outside that, it is not covered
  and grep is the tool — do not keep probing.
- Exit codes: `0` found, `1` nothing, `2` bad query, `3` degraded index.

## Writing back

Findings survive reindexing and are re-checked when the code moves:

```
cairn note <h> --summary "..."          cairn link <a> <b> --note "why"
cairn concept add <name> --note "..."   cairn concept link <name> <h> --rel entry-point
```
