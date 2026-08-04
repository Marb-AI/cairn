---
name: cairn
description: Code context for an agent - what a change breaks, who calls what, how services reach each other, and where any text lives with whose line it is. Say what you are doing with `cairn for`; the mechanisms are underneath.
---

# cairn

Pre-alpha. The **call graph** covers Python and Go only. Text search (`for find`) covers
the whole tree, every file type. Expect the command surface to change.

## Start here: say what you are doing

```
cairn for change <symbol>    I am going to modify this. What breaks, and how far
                             does it reach? One call: the call sites you would have
                             to edit (tests included) and every deployed service the
                             change touches.
cairn for find "<text>"      Where is this text - a value, a key, a header, a name -
                             and whose line is each hit? Searches the **working tree**,
                             so it covers YAML, .env, proto, SQL, markdown, anything,
                             and it is never stale. Each hit carries what the index
                             knows about it: the enclosing function and its handle,
                             the markdown section and its range, the deployed service.
```

Measured: agents state their purpose reliably and pick the mechanism badly. One run spent
eight calls across four commands on a question about a compose variable that no symbol
command answers. `for` takes the purpose and picks; every block of its answer names the
command behind it, so going one level down is a copy-paste.

`for change` on something that is not a symbol will say so and point you at `for find`
rather than guessing.

## Stop when you have the answer

This is the tool's main cost, measured three times. An agent gets the answer in one or two
calls and keeps going — a `graph` to see it another way, a `weaklinks` to be sure, an
`affects` nobody asked for. In one round that habit spent every round trip the tool saved.
In the next, moving this section further down the page cost two scenarios their win, which
is why it now sits above everything else.

- **One command per question, then answer it.** `for change`, `for find`, `affects`,
  `unreached`, `reaches`, `outline` and `entrypoints` are already the whole answer.
- **A second query has to be able to change your answer.** If you cannot say what result
  would make you write something different, do not run it.
- **Confirm only what the envelope calls uncertain.** `[L1, convention]`, `[L1-W,
  unverified]` and a non-empty `unknown:` are worth a second look. `[L0, exact]` with
  `unknown: none` is not.
- **A handle in the output is for your *next* question, not for re-asking this one.**
  `for find` hands back `[mjd]` so you can ask what calls it — not so you can look up the
  line you were just given.
- **When the tool says it cannot see something, that sentence is the answer.** Write it
  down as a gap. Probing around a stated limit does not lift it, and a miss that reports
  what *is* indexed is not an invitation to try three more spellings.

**This is about repetition, not depth.** Measured after the rule was first added: an agent
asked where a chain lands stopped at the first hop, which is a *worse* answer than before
the rule existed. A chain question is not answered until the chain ends, and a branching
handler is not followed until every branch is. `path <a> <b> --detail body` gives the whole
chain and every hop's source in one call — depth without round trips. What you must not do
is ask the *same* question a second way.

## Which one to reach for

- **how code connects** — who calls this, what breaks, what production never reaches,
  which tests cover it, how A gets to B → `for change`, or the mechanisms below
- **where any text is** — a value, a key, a header, a port, a literal, in any file type →
  `for find`. Not grep: it carries the attribution grep cannot
- **the map of a document** → `cairn docs`. Grep gives you a line in a seven-thousand-word
  file and no idea how much around it to read
- **a regex** → grep. `for find` is substring only
- **one file you already know** → just read it

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

## The mechanisms underneath

Reach for these when `for` gave the wrong shape, or when you already know exactly which
relation you want.

```
cairn context "<feature>"        entry point when you have no symbol name
cairn symbol <name>              search by part of a name -> [handles]

Everything below takes a `<h>`: a handle, or the exact name of a symbol when only one
symbol has that name. So `cairn usage Registry` works in one call. When a name is shared
you get the candidates and exit 2 — ask again with a handle. Handles are still worth
using where you have one: they are short and they do not become ambiguous later.

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
cairn entrypoints [--reaches <h>]
                                 every way code gets STARTED - start commands, cron
                                 entries, on-demand runners - each ending in the file it
                                 lands in, so `outline <that path>` is the way down. Use
                                 it when you do not know how a codebase is run. With
                                 --reaches it turns around: which of them can actually
                                 run this symbol. That is the audit question, and it is
                                 not the same as callers - a handler with no caller at
                                 all is still run nightly if a cron entry loads its
                                 module
cairn runs <h>                   which services actually run this code. The filesystem
                                 cannot say: many services share one source tree
cairn outline <path>             what a module holds, and what is actually used
cairn unreached <path>           what production never calls (one call, not one per symbol)
cairn usage <h>                  use sites grouped by file
cairn weaklinks <h>              string literals naming it - candidate dynamic calls
cairn literal "<text>"           string literals in indexed Python and Go, with the code
                                 around them. Narrower than `for find`, which reads the
                                 tree and covers every file type - prefer `for find`
                                 unless you specifically want literals only
```

Prefer the set-shaped commands (`outline`, `unreached`, `usage`, `entrypoints`) when the
question is about a package, a change's blast radius, or how the thing is run. They
answer in one call what the per-symbol commands answer in dozens.

## Budget

`--budget <tokens>` is a ceiling: the tool fills it with the highest-ranked rows and
reports what it dropped. Better than guessing `--limit` and asking twice.

## Trust

- `stale: not tracked yet` means the file watcher is still coming up — it starts itself on
  first use, so this clears on its own. Until it does, the index may be behind the tree.
- Asked about an attribute on a type (`Model.field`), the tool will say **USE GREP FOR
  THIS ONE** and give you the command. Take it: attribute access resolves only where the
  holder's type is known, and for ORM instances it usually is not. Do not treat the short
  list it still prints as a blast radius.
- `ALL ... ARE IN ONE FILE: <path>` means stop querying and read that file.
- `[L1-W, unverified]` is a lexical guess, not a fact.
- A miss from a graph command reports what *is* indexed. If your target is outside that,
  try `for find` - it reads the tree rather than the index - and do not keep probing the
  graph.
- Exit codes: `0` found, `1` nothing, `2` bad query, `3` degraded index.

## Documentation

```
cairn docs                       every markdown file: title, size in words, and its
                                 top-level headings. The map - which one to open
cairn docs <path.md>             that document's sections, each with a line range and
                                 what it costs to read. Descend, do not open the file
cairn docs --about "<words>"     sections that name it (`about`) and sections that
                                 mention it (`3x`), each as a range
```

Every answer is `path:start-end`, so you read that range and nothing else. `about` beats
a mention: a heading naming the thing means the section IS the answer.

Headings and spans are indexed; the prose never is. `--about` is a case-insensitive
substring over headings and bodies — it finds where a subject is written about, not what
is said about it, and it does not match synonyms.

## When the index cannot check itself

```
cairn llm verify                 claims cairn asserts and cannot settle: what a
                                 deployment really starts, and whether a service
                                 boundary recovered from a naming convention is one
                                 boundary. Each names its evidence and what would
                                 falsify it
cairn llm verify --check <id> --holds
cairn llm verify --check <id> --broken --note "..."
```

You are the check. Nothing here calls a model; it asks the one already reading. Go and
look, then record — `cairn status` lifts an area from `indexed` to `verified`, and back
to `verify stale` once the tree moves under it.

Advisory throughout: no verdict changes an exit code or blocks anything, and recording
none is the ordinary case.

## Writing back

Findings survive reindexing and are re-checked when the code moves:

```
cairn note <h> --summary "..."          cairn link <a> <b> --note "why"
cairn concept add <name> --note "..."   cairn concept link <name> <h> --rel entry-point
```
