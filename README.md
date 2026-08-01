# cairn

A local CLI that answers structural questions about a codebase for a coding agent —
deterministically, in one call, instead of a dozen rounds of grep.

```
$ cairn affects fba          # every deployed service a change here touches
$ cairn reaches j2t          # who calls this Python handler from Go, per RPC
$ cairn unreached lib/pricing  # what production never calls
```

- **Not an agent.** An orientation layer underneath one.
- **No LLM.** The whole index builds offline with no API key. A model may enrich the
  knowledge; it never establishes it.
- **Conventions are data.** The core works on a language-neutral schema; what a start
  command looks like, how a protobuf generator names things, where tests live — all of it
  lives in a rule pack (`cairn rules`). Adding a language should be a pack, not a patch.
- **No parsers of its own.** It builds on SCIP indexers and language servers.
- **Knows the topology, not only the code.** Compose files, Dockerfiles and cron entries
  give the graph its roots, so "which service runs this" has an answer at all.
- **Says what it does not know.** Every answer ends with `unknown:` / `suppressed:` /
  `stale:`. A confident wrong zero is the failure this design exists to prevent.

It introduces itself to an agent as a skill, not an MCP server — the same way `gh` or `rg`
would.

## Does it actually help?

Measured against the same agent doing the same task with grep and file reads, on a real
production codebase (Python + Go, ~71k symbols). Three runs per arm, medians, identical
prompts.

| question | baseline | with cairn | tokens | wall clock |
|---|---|---|---|---|
| which deployed services does a change here touch? | 102 280 | 27 355 | **−73 %** | **−87 %** |
| which Go code reaches this Python gRPC handler? | 59 324 | 28 534 | **−52 %** | **−50 %** |
| the same question, target picked by rule not by hand | 68 702 | 23 600 | **−66 %** | — |
| what in this package is called only from tests? | 62 098 | 66 075 | +6 % | — |
| list the MCP tools and their required scopes | 60 043 | 60 877 | +1 % | — |

**Where it wins, and why.** Both winning classes have one shape: a *bounded* question
answered from an edge the caller cannot see. A gRPC boundary joins two languages that share
no identifier, so a name search cannot even begin. A deployment topology is not in the
filesystem — fifteen services share two source trees. cairn is handed the edge; the baseline
has to discover it first.

**Where it does not.** When the answer sits in one file, or in prose, or the question has no
stopping point, an index only adds round trips. The last two rows are exactly that, and they
are in this table on purpose. cairn tries to notice and say so:

```
STOP - every seed is a module __init__ or a test file, so nothing here answers
       your question. Read srcpy/domains/orders/mcp/server.py; do not keep querying.
```

That one message took a losing task from 13 tool calls to 7.

**How much to trust these numbers.** One repository, one language pair, one model. Two of
the five targets were chosen by a fixed rule rather than by hand, and both scored *better*
than the hand-picked ones — the opposite of what overfitting would predict. The baseline
itself varies by up to 1.8x run to run on open-ended questions, so anything below about 15 %
here is noise and is reported as such. The full record, including a −53 % result that was
withdrawn after re-measurement, is in [`eval/RESULTS.md`](eval/RESULTS.md).

## Using it

```
cairn index <file.scip>... --repo <dir>   # build, offline, no key
cairn status                              # what is indexed, and how stale
cairn symbol <name>                       # -> short handles; everything else takes one
cairn rules                               # the conventions in effect, and where from
```

The agent-facing guide is [`skill/SKILL.md`](skill/SKILL.md). It leads with when *not* to
reach for the tool, which measurement showed matters more than the command list does.

First target stack: **Python + Go** (gRPC, Django ORM), deployed with Docker Compose.

→ [docs/architecture.md](docs/architecture.md)
→ [docs/generality-audit.md](docs/generality-audit.md) — what is still tied to the codebase
it was measured on

## Building

Development is Docker-only; nothing is installed on the host — no `cargo`, Node or Go
toolchain:

```
docker compose run --rm dev cargo build --release
docker compose run --rm dev cargo test --release
```

Distribution is a plain binary — GitHub release targets — so *using* cairn will not require
Docker. The container is for building and testing it.

## Tests

Two layers, and the second exists because the first was not enough.

- **Unit tests** cover the pure functions: symbol parsing, command shapes, naming
  conventions.
- **Corpus cases** (`eval/corpus/cases.yaml`) assert against a real indexed codebase —
  counts, membership, exit codes, and *latency ceilings*. They are data, so adding one
  needs no Rust.

The unit tests caught none of the eight correctness defects found in the first day of
measurement: every one lived in the interaction between the index and a real codebase rather
than in a function that could be tested alone. The corpus cases found two more within minutes
of existing, and then an off-by-one in the fix for the first of those. If you change
behaviour here, the second layer is the one that will tell you.
