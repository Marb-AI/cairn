# Cairn — architecture

**Status:** design v0.4 · 2026-07-30 · **D3 confirmed by measurement, phase 0 closed**
**Input:** the "Code Knowledge MCP" brainstorming · calibrated against an internal repository (§16)
**Decided:** CLI + skill instead of MCP · Python and Go together · portable artefacts from the start · everything in Docker

Companion documents:
[coverage-analysis.md](coverage-analysis.md) — confirmation by reading code that the described techniques are enough ·
[spike-0-results.md](spike-0-results.md) — the measured numbers from phase 0 (**verdict: GO**)

---

## 0. The thesis in one sentence

Cairn is a **local daemon with a CLI front end** that holds a persistent, content-keyed graph
of a codebase's structure and answers an agent's navigational questions deterministically,
compactly, and with its uncertainty stated — so the agent does not have to grep for twelve
rounds.

It is not an agent, not an IDE plugin, not a replacement for an LLM. It is an **orientation
layer underneath the LLM**.

**Hard invariant (D15):** the entire index is built **without a single LLM call**. Parsing,
symbols, references, the call graph, topology, entrypoints, routes, git signals — all of it is
deterministic. An LLM may only *enrich* the knowledge (summaries, roles, invariants), never
establish it. The practical test: **`cairn index` must complete offline, with no API key, and
every L0/L1 query must answer the same as it does with one.** See §3.1.

---

## 1. Boundary decisions (which determine everything else)

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Interface | **A CLI binary + a skill. No MCP.** | An agent knows `gh`, `rg`, `jq` — a CLI is the tool's native shape, not a substitute for one. It removes the protocol, the authorisation and the token budget for schemas. MCP is later a thin shell over the same query engine, not a rewrite. See §6.0. |
| D2 | Process model | **Thin CLI front end + a persistent daemon** | Every CLI invocation is a fresh process, and LSP servers take seconds to minutes to start. State has to outlive both the invocation and the session — which is also the main differentiator against Serena. |
| D3 | Source of L0 facts | **SCIP indexers (bulk) + LSP (hot path)** | Do not write parsers. SCIP also comes with a ready-made scheme of stable symbol IDs. See §4. |
| D4 | Fact schema | **SCIP as the internal model** | Symbol IDs independent of position in a file → a per-blob cache is correct and the artefacts are portable. |
| D5 | Cache key | **`blob_id` + `deps_api_hash`** | Changing a function body does not invalidate dependent files. See §5.2 — the most important detail in the whole design. |
| D6 | Storage | **CAS on disk (shareable) + SQLite (local projection)** | A shared cache means transferring files, not replicating a database. Bazel remote cache, not Postgres. |
| D7 | Latency | **A query has a deadline and never blocks** | When fresh facts are unavailable, answer from cache and admit the age. The agent must not wait for indexing. |
| D8 | Answer contract | **Every answer carries `unknown:` and `stale:`** | An imprecise answer stops the agent from looking. An admitted gap does not. |
| D9 | Roots of the graph | **Deployment topology (compose + Dockerfile) is a first-class source of facts** | A call graph with no roots is soup. Compose is the only machine-readable description of the system as a whole, and it is necessarily maintained. See §8. |
| D10 | Index size | **Interning + varint + zstd in the CAS; uncompressed projections in SQLite** | Size is mostly a question of transfer (a cold start is a download). The serialisation scheme is a day-one decision, because it does not get migrated afterwards. See §5.5. |
| D11 | Index vs. git | **The index is not committed to the repository. Only a small textual summary of the topology goes into git.** | Derived data in git is a classic trap (`node_modules`, build outputs). Conflicts are not a problem to solve but a symptom. See §5.6. |
| D12 | Comments | **Extract them, index them for full text, but never present them as fact** | Comments are the best bridge there is between a feature's name and a symbol. They are also frequently out of date. See §4.5. |
| D13 | Runtime environment | **Everything in Docker — the daemon, the language servers, the indexers, the build.** The host must have no `cargo`, no Node and no Go toolchain | A project requirement. It has architectural consequences for paths and for the watcher, not just for the build. See §2.1. |
| D14 | Codegen | **Some code is produced by the build. Detect it and admit the degradation — never run it.** | This holds across ecosystems (protobuf, GraphQL, Prisma, OpenAPI). Running it would break read-only, and it is unnecessary: in a repository with CI the artefacts exist after the first build. See §4.6. |
| D15 | The LLM's role | **The index is built entirely without an LLM. An LLM may only enrich knowledge, never establish it.** | Determinism is the whole pitch — the moment the structure depended on a model, the reason the tool exists would be gone. Testable: `cairn index` runs offline without a key. See §3.1. |
| D16 | Extensibility | **The core knows no language and no framework. Ecosystem knowledge lives in declarative rules, not in the core's code.** | The test repository is proof that it works, not a specification. The next target is JS/TS and that must not mean a rewrite. See §1.1. |

### 1.1 What is core and what is ecosystem knowledge (D16)

The easiest way to ruin this project is to write a tool that handles one repository. The test
repository (§16) exists **to confirm that the general solution works** — not as a
specification. Specific findings from it are marked in this document as *evidence*, not as
specification.

The split into three layers, which decides where things belong and what adding another
ecosystem costs:

```
  ┌──────────────────────────────────────────────────────────────┐
  │ A · CORE — knows no language and no framework                │
  │   snapshot · CAS · deps_api_hash · SCIP schema · graph ·     │
  │   ranking · handles · formatting · git L3 · daemon · CLI     │
  │   Adding a language DOES NOT TOUCH this.                     │
  ├──────────────────────────────────────────────────────────────┤
  │ B · RULES — data, not code                                   │
  │   rules/python.toml · go.toml · typescript.toml              │
  │   entrypoints · routes · service registration · naming       │
  │   conventions for generated code · generated-file detection  │
  │   Adding a framework = a rule, not a commit to Rust.         │
  ├──────────────────────────────────────────────────────────────┤
  │ C · ADAPTERS — a little code, rarely                         │
  │   a language provider (indexer + LSP + comment grammar)      │
  │   a binder a rule cannot carry (.proto parser, tsconfig)     │
  └──────────────────────────────────────────────────────────────┘
```

### Layer A is most of the value

Symbols, references, the call graph, comments, blast radius, co-change from git — none of it
knows what language the code is written in. It rests on the SCIP schema, which is
language-neutral by definition. **This layer works on JS/TS the moment `scip-typescript`
exists** — and it does.

### Layer B: a closed set of shapes, not a general DSL

The temptation is to write a query language over the AST. That is a trap — it ends as your own
parser in a different disguise. The real cases from Python, Go, TS and JS are, however, made of
a small closed set of **shapes**:

| shape | example |
|---|---|
| `call_pattern` | `Register{Service}Server(s, $impl)` · `app.get($path, $handler)` |
| `decorator` | `@$router.get($path)` · `@shared_task` |
| `inherits` | `class $X($pkg.{Service}Base)` |
| `collection_literal` | `urlpatterns = [ path($p, $view), … ]` |
| `command_string` | `python -m $mod` · `next start` · `/bin/$binary` |
| `path_convention` | `app/api/**/route.ts` → `$method /api/**` |

Six shapes, not a general language. **A new shape is added only when at least two independent
real cases demand it** — that is the safeguard against bloat.

A rule is then data:

```toml
[[rule]]
id    = "grpc-go.register"
lang  = "go"
shape = "call_pattern"
match = { name = "Register(?<service>\\w+)Server", args = ["$server", "$impl"] }
emit  = { edge = "implements", from = "$impl", to = "proto:{service}" }
```

Rules ship in packs (`rules/*.toml`), but **a repository may override or extend them** in
`.cairn/rules.toml`. An internal framework nobody else has is therefore solvable without a
fork.

### Layer C: when code is the right answer

When a shape is not enough. A `.proto` parser, reading `paths` out of `tsconfig.json`, a
resolver for a multi-stage Dockerfile. Keep it small and rare; every new adapter is a
maintenance commitment.

### The test condition for D16

**Adding JS/TS may mean: one language provider (layer C) plus one rule pack (layer B). Zero
changes in layer A.** If it does not work out that way, the design is wrong. A concrete
walk-through of that exercise is in §17.

---

## 2. Process topology

```
  agent (coding agent / …) or a human or CI
            │  runs a command, reads stdout
            ▼
     ┌──────────────┐   starts the daemon if it is not running
     │ cairn refs a4│   stateless, ~5 MB RSS, starts in <30 ms
     └──────┬───────┘
            │  unix socket (Windows: named pipe), length-prefixed msgpack
            ▼
     ┌──────────────────────────────────────────────────────┐
     │  cairnd  — one process per machine, N workspaces      │
     │                                                       │
     │   query engine  ──►  store (CAS + SQLite)            │
     │        ▲                    ▲                         │
     │        │                    │                         │
     │   scheduler ──┬── LSP pool ─┤   pyright-langserver    │
     │               │             │   gopls                 │
     │               ├── SCIP runs ┤   scip-python, scip-go  │
     │               ├── watcher ──┤   notify(2)             │
     │               └── git ──────┘   gix                   │
     └──────────────────────────────────────────────────────┘
                        │ (phase 6, optional)
                        ▼  GET/PUT /cas/{blake3}
                 the team's shared cache
```

**Why a thin front end:** an agent calls `cairn` ten times a minute and every one of those is
a fresh process. If each started an LSP pool, not even the first query would finish. The front
end is a dumb pipe; all state and all subprocesses belong to the daemon.

**The budget for CLI startup is 30 ms.** That is a hard requirement following from D1 — with
MCP the startup was paid once per session, with a CLI it is paid on every query. It means: no
configuration parsing beyond what is needed, no filesystem scanning, connect to the socket and
ask immediately.

**Daemon lifetime:** auto-started from the front end (like `gopls`/`tmux`), idle timeout ~30
minutes with no client attached, but **the index stays on disk** — restarting the daemon is a
cold start of a process, not a cold start of knowledge.

### 2.1 Everything runs in Docker (D13)

The host must have no `cargo`, no `rustup`, no Node and no Go toolchain. This is not merely a
build policy — it changes three things in the architecture.

```
Coding agent
   │  stdio
   ▼
docker compose run --rm cairn refs a4    ← front end, one-shot, stateless
   │  unix socket in a shared volume
   ▼
the `cairn-daemon` service               ← long-running compose service
   ├── /workspace   ← bind mount of the repository, read-only
   ├── /cache       ← named volume: CAS + SQLite, survives a restart
   └── in the image: pyright-langserver, gopls, scip-python, scip-go
```

The agent runs commands, so from its point of view `docker compose run --rm` is the same thing
as a binary. In practice it hides behind a shell wrapper called `cairn` on `PATH`, so the agent
writes it naturally.

**Consequence 1 — paths.** Inside the container the repository is `/workspace/srcpy/…`, on the
host `/home/user/backend/srcpy/…`. If answers carried container paths, the agent could not open
the files. This is solved by a rule already in the design for another reason: §5.1 point 1
forbids absolute paths for the sake of portable artefacts. **Everything is relative to the
workspace root**, so `srcpy/domains/orders/grpc/server.py:42` works both in the container and on
the host. The two decisions meet by coincidence, but pleasantly.

**Consequence 2 — the watcher.** `inotify` over a bind mount works natively on Linux. On Docker
Desktop (macOS/Windows) it is unreliable — fall back to polling on a longer interval, or let the
front end send an explicit "this file changed". The risk is in §14.

**Consequence 3 — image size.** The daemon image has to contain Node (pyright), the Go toolchain
(gopls, scip-go) and Python (scip-python). That is not small, but it is one-off, and it is
exactly what the test repository already does with its `pbgen` and `go-compiler` images.

The spike tooling (§13, phase 0) runs the same way — neither `scip-python` nor `scip-go` is
installed on the host.

---

## 3. Layers of knowledge

Unchanged from the brainstorming, only with an explicit contract for precision:

| | Contents | Source | Contract | Invalidation |
|---|---|---|---|---|
| **L0** Structural facts | definitions, occurrences, imports, types | SCIP indexer / LSP | **100% recall, otherwise return nothing** | per blob, ms |
| **L0-C** Comments | docstrings, comments, TODOs, markdown | SCIP + tree-sitter (§4.5) | **exactly extracted, semantically untrustworthy** — for search only, never as an assertion | per blob, ms |
| **L0-D** Deployment facts | services, entrypoints, ports, env, routes, the container↔repo map | compose, Dockerfile, urls.py (§8) | exact where it parses; `unknown` otherwise | per file, ms |
| **L1** Derived structure | references, call graph, blast radius, reverse deps, reachability from a service | a join over L0 + L0-D, pure code | 100% with respect to L0 | incremental, ms |
| **L2** Semantics | summaries, roles, invariants, concepts | LLM, lazy | may go stale, carries `confidence` + `age` | loose, in the background |
| **L3** Execution | what changes together, test impact, runtime call graph | git log, coverage | statistical, returns a score | per commit / per test run |

**The key point:** never mix L0 and L1 with L2/L3 in one unqualified answer. When `cairn blast`
returns 4 static callers and 3 co-change candidates, they have to be visually separated in the
answer — otherwise the agent takes the statistic for a fact.

### 3.1 The dividing line: L2 is the only layer with an LLM (D15)

```
  ┌─────────────────────────────────────────────────────────┐
  │  L0 · L0-C · L0-D · L1 · L3                             │
  │  deterministic · offline · no API key · 100% recall     │
  │  ── builds itself, completely, repeatably ──            │
  └─────────────────────────────────────────────────────────┘
                            ▲
                            │  may add, never establishes
  ┌─────────────────────────┴───────────────────────────────┐
  │  L2 — summaries, roles, invariants, concepts             │
  │  optional · lazy · with confidence · always removable    │
  └─────────────────────────────────────────────────────────┘
```

Three rules follow from this, and all three are testable:

1. **`cairn index` runs offline.** No network, no key, no model. In CI that is one test.
2. **Deleting all of L2 must not change a single L0/L1/L3 answer.** A regression test: run a set
   of queries, empty L2, run them again, compare. Any difference is a bug.
3. **L2 never enters a computation.** It must not affect ranking, reachability, blast radius or
   seeding. It may only be displayed — always labelled, as with comments (§4.5).

**Who produces L2, given that cairn has no LLM and no MCP sampling (D1).** The cheapest source
is **the calling agent itself**: a model that has just read `TokenValidator` can describe it for
free, because it has already done the work. Hence:

```
cairn note <handle> --summary "…" [--confidence high|low]
```

This is not a violation of "read-only" (§6.1) — cairn does not write to the repository, only to
its own cache. Writing to source stays forbidden.

Bulk enrichment with your own key (`cairn enrich --model …`) is a distant second option and is
strictly opt-in. A default installation calls out to nothing.

---

## 4. Obtaining L0: three speeds

The commonest mistake with tools of this kind is to build everything on LSP. LSP is a **query**
protocol, not an indexing one. "Give me the references of every symbol" = O(n) round trips =
hours.

### 4.1 The cold / batch path — SCIP indexers

`scip-python` (Sourcegraph's, built on pyright) and `scip-go` run over the whole project and
emit a SCIP index: for every document a list of **occurrences** (range + symbol ID + a
definition/reference/write role) and **symbol information** (documentation, relationships).

What that gives us for free:
- stable, position-independent symbol IDs (`scip-python python . . auth/oauth.py/TokenValidator#validate().`)
- a ready-made model that is *designed* for cross-repo and cross-language linking
- an ecosystem of further indexers for when a language is added (TS, Java, Rust, Ruby)

The drawback: it runs over the whole project, not incrementally. Hence:

### 4.2 The hot path — LSP for dirty files

A file being worked on, or unsaved, goes through `pyright-langserver` or `gopls`:
`documentSymbol`, `references`, `definition`, `implementation`, `callHierarchy`. The result is
remapped into the same SCIP schema and **overlays** the base.

Latency: 10–100 ms per file. That is exactly the query that matters most ("I have just changed
a signature, what did I break").

### 4.2b The LSP pool — the other half of the overlay

A batch indexer cannot answer about a file that has changed without a full run. A warm language
server can, and **measured**: after an edit, `documentSymbol` costs 4–5 ms with pyright and
3.6–7.3 ms with gopls, `references` 94–115 ms and 23–27 ms (spike-0-results §4.2c).

The whole of `cairn live <file>` — process start, socket, LSP query, the index query and
formatting — comes out at **11 ms median**.

Three things the measurement produced that went into the code:

- **The client has to answer the server's requests.** During startup pyright asks for
  `workspace/configuration` and until it gets an answer it serves nothing. The first version of
  the benchmark ignored those and "measured" a 180s timeout on every query.
- **The first query is a different category.** pyright's first `references` took 1,353 ms even
  after warm-up, against 130 ms warm. Hence the pool warms servers in the background at startup,
  and hence the client has **different timeouts per kind of request**: `dirty` is asked on every
  CLI call and needs a tight ceiling so a stuck daemon does not delay an ordinary query; an LSP
  query is given room for the cold case.
- **The languages are not symmetric.** pyright is roughly 4× slower than gopls on the hot path —
  the reverse of the batch path, where it is Go that has no cheap partial reindex.

#### What the overlay shows

Not a live listing but a **comparison**: what the server sees now against what the index holds.

```
$ cairn live srcpy/domains/orders/mcp/middleware.py
+ …:136-138  BrandNewMiddleware
+ …:137-138  BrandNewMiddleware.on_call_tool
stale: the index is behind for this file: 2 new, 0 moved, 1 gone
```

The comparison has to be on **qualified names**. Two classes in one file can have a method of
the same name, and comparing bare names pairs them up and invents a move that never happened.

The remaining imprecision, admitted: the index knows about `__init__`, which `documentSymbol`
does not list, so it is reported as `gone`. It is one entry and it is honestly labelled, not
hidden.

### 4.3b The daemon holds live state, not queries

The naive version would have the daemon proxy every query. **Rejected after measurement:**
SQLite in WAL mode handles concurrent readers and CLI startup is ~1 ms, so a proxy would add
latency and buy nothing.

The daemon exists for what **a one-shot process cannot have: live state.** Today the watcher,
which is already running before anybody asks; tomorrow warm language servers. It therefore
answers a single question — *what has changed since indexing* — and the CLI folds that into the
`stale:` section. When the LSP pool arrives it attaches for the same reason and the protocol
grows by one request instead of changing shape.

Three things that turned out to matter:

- **Dirtiness is measured against the index, not against the last event.** A file is dirty when
  its contents differ from what was indexed — so an edit and its reversal leave nothing behind.
  If events were counted, a `git checkout` would mark half the tree without a single real change.
- **Empty and unknown are not the same set.** Without a daemon the report is not "clean" but
  `stale: not tracked`. Merging those two is exactly the silent staleness D8 forbids.
- **What is marked is the answer, not the index.** A query about a symbol in a changed file
  admits it; a query next door stays clean. A blanket "the index is old" would be learned and
  ignored.

### 4.3 The overlay is not a special mechanism

Because everything is content-keyed, a "dirty file" is just a file with a different `blob_id`.
The only mutable thing in the system is the **snapshot**:

```
snapshot = { relative path → blob_id }
```

- `head_snapshot` — read from the git tree (gix), free
- `working_snapshot` — filesystem plus the watcher, the default for queries

Switching branches = swapping the snapshot ≈ no work, because the facts underneath are
unchanged. Rebase / amend / squash / force-push = no effect at all; the system does not care
about commit hashes.

> **The architectural spine:** the snapshot is the only mutable thing; everything under it is
> immutable, content-addressed facts.

### 4.4 A risk that has to be checked within 2 weeks

`scip-python` is a Sourcegraph project with uneven maintenance, and the Django ORM is exactly
what pyright stumbles on. **The first task after creating the repository: run scip-python over
the test repository and measure how many symbols stay unresolved.** If it is more than ~15%, the
plan changes (fallback: an LSP bulk crawl restricted to exported symbols, with a slower cold
start).

For Django the assumption was that the stub packages `django-types` / `django-stubs` configured
for pyright would solve it cheaply and cover 90%.

**Measured, and it does not hold.** `django-types` installed itself automatically into the
indexing copy and the index grew by 2.6% of occurrences — but `LedgerEntry.ledger_category` went
from 0 resolved use sites to 5 across two files, while that name occurs in **33 files**.

The reason: the problem is not the field's type but the *holder's* type. `for tx in transactions`
where `transactions` came out of a queryset is not typed as `LedgerEntry` without a mypy plugin
(which pyright cannot run), so `tx.ledger_category` has nothing to resolve against. Stubs
describe the model, not what a queryset returns from it.

**Consequence for the design:** the Python side has a **structural ceiling** on ORM-heavy code
that configuration will not remove. Three routes remain and all of them are more expensive than
§4.4 assumed:

| route | who has to do it |
|---|---|
| annotations in the repository (`tx: LedgerEntry`) | the repository's owner — it changes source |
| a runtime trace (§9) — real types from a test run | cairn, but it is a whole L3 layer |
| admit the limit in the answer | **done** — an attribute on a type carries the caveat that this is a lower bound |

Until then the last of those holds: it resolves not one extra reference, but it turns a silently
wrong answer into an admitted one.

### 4.5 The third speed: comments and documentation

Comments are **the best bridge that exists between a feature's name and a symbol**. "OAuth"
often appears in no identifier at all, but it is right there in the first line of a docstring.
Without them `cairn context` rests on fuzzy matching of names and paths, which is the weaker
half of §6.4.

#### Where from

| source | how | cost |
|---|---|---|
| Symbol docstrings | `SymbolInformation.documentation` — **SCIP already carries it**, just do not throw it away | nothing |
| Inline and block comments, module headers | tree-sitter | ms/file |
| Markdown in the repository (README, ADRs, `docs/`) | a plain parser plus headings as sections | ms |

#### Here tree-sitter is the right tool

The brainstorming rejects tree-sitter for L0 — correctly, it gives a parse tree, not name
resolution, and on C# or Django it fails silently. **Comments are the exact exception: there is
nothing to resolve.** It is purely lexical and positional extraction. No overloads, no generics,
no partial classes. Tree-sitter is cheap here, accurate and language-universal — and adding
another language costs one grammar, not a whole indexer.

#### Attached to a symbol, not text soup

A comment binds to the nearest following definition (a leading block) or to the enclosing symbol
(inline). That makes the full text **scoped**: a match in a comment returns a symbol with a
handle, not "a file where this appears somewhere". Unattached comments (module headers) bind
to the file.

#### The truth contract — different from the rest of L0

A comment is extracted **exactly** (text is text), but its *assertion* is unverified and is
frequently out of date. Therefore:

- comments are used to **find candidates**, never as fact in an answer
- when a comment is quoted in an answer it is labelled `[comment, unverified]`
- in `symbols_fts` they get their own column with a **lower weight** than the symbol's name: a
  match on the name > a match in a docstring > a match in an inline comment
- **commented-out code is detected and down-ranked** (a line that parses as code) — otherwise it
  is the largest source of noise in the full text

This distinction has to be kept: it is the only part of L0 that is exactly extracted but
semantically untrustworthy. Mixing it with references would break the contract "L0 = 100% or
`unknown`".

#### Free by-products

`TODO` / `FIXME` / `HACK` / `XXX` as an edge `kind` of their own. For the audit domain — which
the brainstorming names as the target market — "show me every FIXME in code reachable from a
public endpoint" (§8.7) is a question nothing answers today.

Invalidation is unchanged: comments are per file, content-keyed like everything else, and do not
depend on `deps_api_hash` (they have no dependencies).

### 4.6 Missing codegen — indexing is possible, silence is not

**The general phenomenon:** part of the code need not exist in the working tree, because the
build produces it. The assumption "what is in the repository is all the code" fails for
protobuf/gRPC, GraphQL codegen, OpenAPI clients, Thrift, ORM stub generators, .NET source
generators, the Prisma client and `next build` types. Across languages, not in one.

The consequence is not "a few unresolved references". When inheritance or a type hangs off a
generated symbol, the missing artefact takes away **the entire surface** that runs through it.

#### Behaviour: detect, index, admit

A rule (layer B, §1.1) describes, for a given ecosystem, a pair of *inputs → expected outputs*:

```toml
[[codegen]]
id       = "protobuf.python"
inputs   = ["**/*.proto"]
produces = ["**/*_pb2.py", "**/*_pb2_grpc.py"]
hint     = "run your protobuf generation step"
```

| state | behaviour |
|---|---|
| the outputs exist and are not older than the inputs | indexed normally |
| missing, or stale | **still indexed, but the index is `degraded:`** |

The second row is the whole point. Without it the tool would claim "3 references" where there
are 200 — exactly the silent failure D8 avoids. The flag goes into `cairn status` **and into
every answer**:

```
degraded: generated sources missing or stale (protobuf.python).
          References crossing that boundary are incomplete.
          hint: run your protobuf generation step
```

#### cairn runs nothing

An earlier version of the design had a "prepare step" here that would run codegen itself.
Dropped, for two reasons. First, it would break the read-only contract (§6.1) — codegen writes
into the working tree. Second, it is unnecessary: **in a repository with CI that guards
generated code, the artefacts exist in every checkout where somebody has built or run the tests
once.** The "missing" state is transient and mostly concerns a fresh clone.

So all that remains is detection and an honest admission. Cheap, universal, no side effects.

*A note on evidence: an earlier version cited the test repository here as an example of missing
Python stubs. That measurement was wrong — the stubs are committed, betterproto2 simply dumps
them into `__init__.py` instead of `*_pb2.py`. The mechanism holds in general, but this
repository is not an example of it. See [spike-0-results.md](spike-0-results.md) §5.*

---

## 5. Storage and cache

### 5.1 Two kinds of data

```
~/.cache/cairn/
  cas/                      immutable, content-addressed, SHAREABLE
    blake3/ab/cd/abcd…      a FileFacts record (msgpack, deterministic serialisation)
    blake3/…                a whole SCIP index for a tree_hash (coarse granularity)
  ws/<workspace-id>/
    index.sqlite            a LOCAL projection, recomputable from the CAS at any time
    snapshot.bin
```

**The CAS is the truth and the shared artefact. SQLite is a materialised view.**

The consequence for a shared cache (decided: yes) is that the sync layer is a dumb file
transfer — `GET /cas/{hash}`, `PUT /cas/{hash}`. Immutable, no invalidation, no conflicts, no
database replication. The semantics of a Bazel remote cache or a Nix binary cache.

**What that costs today** (and it is the entire price of not having to rewrite the design
later):
1. no absolute paths in CAS records — everything relative to the workspace root
2. deterministic serialisation — no `HashMap` ordering, sorted collections
3. every record carries a `schema_version` plus an `indexer_id@version` (e.g. `scip-python@0.6.0`, `pyright@1.1.403`)
4. no local IDs (rowids, pointers) in portable structures — interning yes, but **locally within
   one record** (§5.5), never as a reference into a global table
5. **the hash addresses the uncompressed content, the storage is compressed** — compression is
   then purely a storage detail, and changing the compression dictionary does not churn the whole
   CAS

### 5.2 Keying — the most important detail

The naive `key = blob_id` is **incorrect**: facts about a file depend on its dependencies
(`from .models import User` does not resolve without `models.py`).

The naive `key = (blob_id, hash of the whole dependency closure)` is **useless**: changing one
leaf invalidates the entire tree above it.

The choice:

```
key = (blob_id, deps_api_hash, indexer_version, schema_version)

deps_api_hash = hash( for each imported module: its sorted set of
                      exported symbols + their signatures )
```

That is, **a hash of the dependencies' public interface, not of their contents.** Change a
function body in `models.py` → `deps_api_hash` does not change → every dependent file stays in
cache. Change a signature → exactly what should be invalidated is invalidated.

It is the same trick as header jars in Bazel or interface hashes in Rust's incremental
compilation. It converges, because `deps_api_hash` is computed from the already-cached facts of
the imported modules — not from fresh parsing.

*(Import cycles: an SCC is hashed as a whole. In Python that is rare and small; in Go the
compiler forbids it.)*

### 5.3 Two granularities of sharing

| Granularity | Key | When it helps |
|---|---|---|
| A whole project index | `(git tree_hash, indexer_versions)` | A new team member / CI / a fresh clone → **a cold start is a download, not an indexing run** |
| Per-file facts | `(blob_id, deps_api_hash, …)` | Daily work, sharing between branches and between developers |

### 5.4 The SQLite schema (a sketch)

```sql
-- interning: a string lives exactly once
strings(id INTEGER PK, s TEXT UNIQUE)          -- paths, names, descriptors
symbols(id INTEGER PK,
        parent_id INTEGER REFERENCES symbols,  -- prefix sharing: class → method
        desc_id   INTEGER REFERENCES strings,  -- the last descriptor only
        lang, kind, flags)
files(id INTEGER PK, path_id INTEGER REFERENCES strings, blob_id BLOB, lang, generated BOOL)

occurrences(file_id, symbol_id, line, col_start, col_end, role)
   INDEX (symbol_id, role)          -- cairn refs
   INDEX (file_id, line)            -- "what is on this line"

edges(src_symbol, dst_symbol, kind, confidence, source)
   -- kind:   calls | implements | overrides | binds | tests | co_changes
   --         | entrypoint | routes_to | reads_env | talks_to
   -- source: scip | lsp | proto | compose | dockerfile | route | env | git | trace
services(id INTEGER PK, name_id, lang, kind)   -- kind: built | external
comments(file_id, symbol_id NULL, line, kind, text)   -- §4.5
   -- kind: docstring | leading | inline | module | todo | commented_out
handles(symbol_id, handle TEXT UNIQUE)
unknowns(file_id, line, reason, hint)

-- FTS5, columns in descending weight; powers `cairn symbol` and the seed for `cairn context` (§6.4)
search_fts(name, path, docstring, comment, commit_msg, doc_md)
```

Two things in the schema that are done on day one, because introducing them later is a
migration:

- **`strings` interning.** Paths and descriptor names repeat in every occurrence.
- **`symbols.parent_id`.** A SCIP symbol is a hierarchical string
  (`… auth/oauth.py/TokenValidator#validate().`). Storing the whole string on every symbol means
  repeating the path and the class name 30 times for a class with 30 methods. A parent pointer
  plus the last descriptor assembles it at run time and gives "all members of this class" for
  free at the same time.

A single `edges` table with `kind` + `source` + `confidence` is deliberate: L1 (static,
confidence 1.0), L0-D (deployment, §8), L3 (statistical, confidence < 1) and binders (§7) all
live in the same table, and the answering layer separates them by `source`.

**Writes:** a single writer task (SQLite WAL), reads from a read pool. A query never waits on a
write.

### 5.5 Index size and serialisation

Size is not cosmetics: **a cold start for a new team member is a download of the index.** The
budget is therefore defined by transfer, not by disk.

**The target:** a full index for a 500k-line repository ≤ 50 MB compressed, so that a cold start
over the shared cache comes in under 10 s on an ordinary connection. *(To be confirmed in phase
0 — a raw SCIP index for a repository that size tends to be hundreds of MB, so a factor of 5–10
is needed.)*

#### A tension that has to be resolved explicitly

Interning to int32 and portable artefacts pull against each other: a globally allocated ID is by
definition local and not portable. The solution is to have **two representations**, not a
compromise:

| | CAS record (durable, shared) | SQLite projection (for querying) |
|---|---|---|
| Optimises for | size | latency |
| Interning | **a local string table inside the record** — the record is self-describing | a global `strings` table |
| References | an int32 index into the local table | a global rowid |
| Compression | zstd, addressed by the uncompressed hash | none |
| Read | when filling the cache, not when querying | on every query |

That dissolves the tension: a CAS record is independently decodable on any machine and still
does not repeat a single string inside itself. Decision D6 (two stores) was right for exactly
this reason.

#### Concrete techniques, in descending order of return

1. **A local symbol and string table in every document.** A document typically references tens
   to hundreds of symbols but has thousands of occurrences → an int16/int32 index instead of a
   string. *(SCIP already does this in its own format — we adopt it rather than inventing it.)*
2. **Delta plus varint on positions.** Sort occurrences by position and store the differences in
   lines and columns. Most deltas fit in one byte.
3. **Prefix decomposition of symbols.** The same thing as `parent_id` in SQLite, only in
   serialised form: `(parent_index, suffix)`.
4. **Roles as a bitfield**, not an enum string.
5. **zstd with a trained dictionary.** The CAS is many small, mutually very similar records —
   exactly the case where compressing a small file on its own fails and a shared dictionary gives
   multiples. The dictionary is a versioned artefact in the CAS like any other; because the
   uncompressed content is what is addressed, replacing it re-addresses nothing.
6. **Store generated code, but separately.** `*_pb2.py` and `*.pb.go` tend to be most of the
   index's bytes and are almost never in an answer (§7.3). A CAS namespace of their own → a
   shared cache can skip them and fetch them lazily.

Incrementality is an ally here: a record is compressed once and read many times, so we can
afford more expensive compression than if the whole index were being rewritten.

#### Where the line is

Interning, varint and zstd are **schema and serialisation** — cheap, permanent, and painful to
migrate to later. A custom storage engine, mmap and a B+ tree are something else and stay on the
"never" list in §13. That difference is easy to blur: both can be described as "storage
optimisation". They are not the same thing — one is the shape of the data, the other is a
database of your own.

### 5.6 The index and git

The question is whether to commit the index into the repository, and what to do about
conflicts. The answer has three levels and the first of them changes the question.

#### Conflicts are caused by the monolith, not by being binary

A single file containing the whole index will conflict on every merge **regardless of format**.
A textual format does not give a resolvable conflict, only an unreadable one — ten thousand
lines of shuffled records where "resolve by hand" makes no sense. A custom text format does not
solve this problem, it only dresses it up.

By contrast, **content-addressed records cannot conflict by definition.** §5.1 point 2 requires
deterministic serialisation — so two developers who index the same blob produce a **byte-for-byte
identical file**. Merging two CASes is a union of sets, not a merge. There is nothing to resolve.

That is no accident: **the git object store is exactly the same idea.** Immutable objects named
by the hash of their content. Nobody resolves conflicts in `.git/objects`.

#### But the index does not belong in the repository anyway

The killer is not conflicts, it is **bloat**. Git remembers every version forever, the index
changes on practically every commit, and binary content does not delta well. After a few hundred
commits the clone is unusable — and cleaning it up afterwards means rewriting history.

On top of that it is **derived data**. The same category as `node_modules`, build outputs and
generated code: everybody who committed one of those regretted it. The index is by definition
recomputable at any time from the contents of the repository (§5.1) — that is the whole point of
content addressing.

Plus the noise: every PR would carry megabytes of diff nobody reads.

#### Readability is a property of the CLI, not of the format

The objection "unreadable, undiffable" has its right answer in the tool, not in the format:

```
cairn inspect <hash>        → a readable dump of a record
cairn diff <hash> <hash>    → the difference between two versions of a file's facts
```

Exactly like `git cat-file -p`. Nobody makes git objects textual for the sake of readability.
And diffing two CAS records is as rare an operation as diffing two git blob objects — it
interests you once in a while when debugging an indexer, not in ordinary work.

#### So how to share between developers

| option | extra infrastructure | repository bloat | when |
|---|---|---|---|
| **Do not share** — everyone indexes locally | none | none | **phases 1–4.** A ~60 s cold start is bearable |
| **Git as transport on a ref of its own** | none | yes, but prunable | the cheapest sharing without a server |
| **A CI artefact** — CI indexes `main`, everyone else downloads | a CI job | none | a team that already has CI |
| **A CAS server** | a server | none | phase 5, monetisation |

On the second option, because it is the most interesting: CAS objects are stored under a ref of
their own (`refs/cairn/cache`), which **is not a branch and is not in the working tree**. It is
never merged, never checked out, and does not appear in `git log`. The objects are immutable and
hash-named, so `git push`/`fetch` on that ref is a union — a conflict cannot arise. The ref can
be discarded and force-pushed again at any time, because it is purely a cache. Fetching it is
optional.

That makes a "shared cache" buildable **without a single server**, on nothing but what the team
already has. The growth of the object database remains, but it is controlled and separated from
the history of the code.

#### What does belong in the repository: a textual summary of the topology

The analogy with migrations is right — it just has to be applied to the right thing. What does
not belong in git is `node_modules`; what does is the **lockfile**. Here the lockfile is the
topology (§8.8):

```
.cairn/
  topology.txt      ← COMMIT THIS: ~300 lines, textual, readable, diffable
  cache/            ← .gitignore
```

The properties that make it the opposite of the index: it is small, semantic, changes rarely
(only when the shape of the system genuinely changes) and **a conflict in it is meaningful** —
two people added a service — and is resolved by regenerating.

The extra value, which nobody has today: **an architectural diff in code review.** When a PR
adds a service, opens a port, adds a cross-service call or an endpoint, it shows up in the diff
as five lines instead of having to be found in the code.

```
 services (6)
   gateway   go    cmd/gateway/main.go:22          :8080 → public
+  billing   go    cmd/billing/main.go:14          :50052 grpc
 edges
+  gateway → billing    grpc BillingService     [proto + env BILLING_ADDR]
+  billing → postgres   env DATABASE_URL
 public surface
-  :8080  gateway  14 HTTP routes
+  :8080  gateway  17 HTTP routes
```

And in CI, `cairn topology --check` fails when the committed summary does not match the
generated one — the same mechanic as `go mod tidy -diff` or `cargo fmt --check`.

---

## 6. The CLI interface

### 6.0 Why a CLI and not MCP (D1)

The original version of the design was built on MCP. It is a needless extra step.

An agent can run commands, and uses `gh`, `rg`, `jq` or `docker` fluently without any protocol.
A CLI **is not a substitute for MCP, it is the tool's native shape**; MCP is a wrapper that, for
a local read-only tool, solves no problem that would otherwise exist.

What goes away:

- implementing the protocol and the server lifecycle
- **the budget for tool definitions.** That was hard under MCP, because schemas travel in every
  request. With a CLI the description is in a skill that is loaded only when it is relevant — the
  "at most 6 tools" constraint simply disappears
- authorisation, transport, a remote variant

What is gained:

- **testability** — per §6.3 the answer format is the product itself, and in a terminal it is
  visible immediately; under MCP you need a running agent to evaluate it
- usability in CI, in a Makefile and in scripts, where MCP does not reach
- trivial iteration

What is lost — honestly, two things:

1. **Discoverability.** An MCP host always sees the tool schemas; a CLI has to be introduced to
   the agent by somebody. A skill, or two lines in `AGENTS.md`. Installing a skill is about as
   easy as installing an MCP server, though, so it is more of a relocation than a loss.
2. **MCP sampling.** The option of running an LLM step on the host's model disappears (§6.4). It
   turns out to be an improvement — see there.

**This is not a bet.** The product is a query engine plus a formatting layer; the CLI, and any
later MCP, are thin front ends over `cairn-daemon`. Adding MCP later costs one crate, not a
rewrite.

### 6.1 The command set

```
cairn symbol <query> [--lang] [--limit]   entry point by name / pattern
cairn context <query>                     entry point by concept  (§6.4)
cairn refs <handle> [--kind]              callers | impls | overrides | writes | all
cairn tests <handle>                      tests covering a symbol (L0 + L3)
cairn blast <handle> [--depth]            what I break by changing it  (L1 + L3, separated)
cairn expand <handle> <what> [--depth]    body | doc | neighbors | file_skeleton
cairn topology                            a map of services and their links  (§8.8)
cairn status                              what is indexed, what is stale, what is degraded
cairn note <handle> --summary …           write an L2 note into the cache  (§3.1, D15)
```

The budget is no longer hard, but **restraint remains** — the agent has to be able to pick the
right command, and eight memorable ones are better than thirty. A new subcommand only when no
combination of the existing ones gives the answer.

Considered and rejected: a separate `implementations` (it is `refs --kind=impls`), `definition`
(that is the output of `symbol`), anything that writes — cairn is read-only, deliberately (§4.6).

### 6.1.1 A CLI for an agent, not for a human

The ergonomics differ and a choice has to be made in the agent's favour:

- **no interactivity.** Never a prompt, never a pager, never waiting on `stdin`.
- **stable output.** No TTY detection, no colours, no spinner artefacts in `stdout`.
  Diagnostics go to `stderr`.
- **exit codes mean something:** `0` found, `1` nothing found, `2` a bad query, `3` the index is
  degraded (§4.6) — so an agent can tell "there is nothing there" from "I cannot see there".
- **text is the default and `--json` is the escape hatch** for scripts. Not the other way round:
  the text is the product (§6.3).
- **no state between calls** except handles, which are persistent (§6.5).

### 6.2 The skill is product work

Under MCP this was the tool descriptions; with a CLI it is the skill — and it is more room, not
less. An agent knows grep and reaches for it reflexively; the skill has to say **when cairn is
better**, not what it does:

> **Finding a symbol's uses.** Use `cairn refs <handle>` instead of grep. Grep finds comments,
> strings and same-named symbols from other modules — and it does not find a call through an
> import alias or across a gRPC boundary between Python and Go. `cairn refs` returns a compact
> list with handles that can be expanded with `cairn expand`.
>
> **Getting oriented in an unfamiliar part of the system.** Start with `cairn topology`, not by
> reading files.

The advantage of a skill over tool descriptions: it can carry a whole workflow ("start here,
then expand, do not use grep for finding references") and it costs nothing until it is relevant.

The quality signal stands: **if the agent reaches for `cairn` even without the skill** — because
it sees it in `AGENTS.md` or in its history — the tool is clearly better than grep. If you have
to push it, either it is not, or you cannot demonstrate it fast enough.

### 6.3 The answer format is the product

Not JSON. Compact, line-oriented, ASCII.

```
$ cairn symbol validate
3 matches (2 suppressed: generated)
[a4] TokenValidator.validate(token: str) -> Claims    py  auth/oauth.py:142
[a7] SessionValidator.Validate(tok string) (*Claims, error)
                                                      go  internal/auth/session.go:88
[b1] validate(schema, payload)                        py  utils/schema.py:31
```

```
$ cairn blast a4 --depth 2

static callers (4)                                        [L1, exact]
  [c1] LoginHandler.post           py  api/login.py:55
  [c2] RefreshHandler.post         py  api/refresh.py:31
  [c3] AuthInterceptor.Intercept   go  internal/grpc/auth.go:44   via proto AuthService.Verify
  [c4] worker.session_gc           py  workers/gc.py:12
transitive depth 2: 11 more in api/ (7), workers/ (3), internal/grpc/ (1)

tests covering (3)                                        [L0+L3]
  tests/test_oauth.py::test_expired_token
  tests/test_oauth.py::test_clock_skew
  internal/auth/session_test.go::TestValidateExpiry

co-changed (git, 200 commits)                             [L3, statistical]
  auth/keys.py 0.72 · api/login.py 0.61 · proto/auth.proto 0.44

unknown (1)
  plugins/loader.py:22 — dynamic dispatch via getattr(mod, name); name from config,
  not statically resolvable. Candidates: plugins/*.py (7 files). Grep suggested.

stale: none
```

Notes on the format:
- **`unknown:` is a mandatory section of every answer.** Empty means `unknown: none`. When it is
  missing, the agent assumes completeness — and that is the silent error that stops the search.
- **`suppressed:` likewise.** How much we dropped and how to ask for it. Silent truncation reads
  as "everything is covered".
- Every block's layer is labelled (`[L1, exact]` vs `[L3, statistical]`).
- The handle `[a4]` — 2–4 characters, see §6.5.

### 6.4 `cairn context` — the entry point by concept

"Give me context on OAuth" is not a symbol query. The seed is obtained cheaply, then expanded
deterministically. In order of cost:

0. **Deployment topology** — when the term matches the name of a compose service, its build
   directory or a route, that is an incomparably better seed than a fuzzy match on names.
   "OAuth" in a project with an `auth` service is a solved query, not a heuristic. See §8.
1. **Lexically** — FTS5 over symbol names and paths (`*Auth*`, `*Token*`, `/auth/`). ~60% of the
   rest.
2. **Comments and docstrings** (§4.5) — often the only place a feature's name appears at all. A
   match returns a symbol with a handle, not a file, because comments are attached to symbols.
3. **Tests** — test names are the best documentation of a concept in a project.
4. **Git** — FTS5 over commit messages and PR titles; files changed together in commits
   mentioning the term.
5. **Documents** — README, ADRs, `docs/`.
6. **None of that worked** — return a weak seed and **say so**.

**Built.** Docstrings are free: SCIP carries them for **77.7% of Python symbols** and 10.5% of Go
symbols on the test repository (4.1 MB of text), so all that is needed is not to throw them away
during ingest. Confirmed to work on terms that appear in no identifier at all — `cairn context
"fail-closed"` finds symbols purely through prose.

Two things that decided usability and were not obvious in advance:

- **Generated code has to fall down the ranking.** The first version, asked for "quota", returned
  protobuf fields named `quota` and buried `QuotaModule`, whose own documentation says it is the
  quota client. Suppress, do not exclude — a term that lives only in generated code should still
  return something.
- **Weighting by kind of symbol.** A type or a function *can be* "the part of the system I am
  asking about"; a field cannot. Without that, name matches on attributes win.

Every seed carries **a label saying where it came from** (`[concept+name+doc]`). "Somebody named
this" and "this fuzzy-matched a name" deserve very different degrees of trust, and the agent has
no way of telling unless we say so.

Point 6 is simpler thanks to D1 than it was. The original design wanted an LLM step here through
MCP sampling. With a CLI there is no sampling — and it turns out to be an improvement: **the
calling agent is an LLM itself.** cairn should not do a worse version of what it can ask for one
line up. So:

```
$ cairn context "oauth"
low confidence — no strong seed for this term
best guesses (5)
  [k2] domains/orders/grpc/handlers/auth.py :: AuthServiceHandler   [name]
  [k7] proto/orders_api/auth.proto :: AuthService                   [name]
  …
hint: no compose service, route prefix or test name matched "oauth".
      Try `cairn topology`, or grep for the domain term this project uses.
```

No API key, no cost of our own, no dependence on host support. When a seed does fit, it is
cached as an L2 artefact.

Then: expand one hop through the call graph, rank (§6.6), and return **a skeleton of 10–15 nodes
with no bodies**.

> A trap to watch for: if `cairn context oauth` returns 40 files with their contents, you have
> burned the same tokens as exploration would, just all at once. The saving does not come from
> having a graph — it comes from returning little, and precisely.

### 6.5 Handles

A short code for progressive disclosure. Requirements: short (token cost), deterministic, stable
across sessions.

The solution: **the shortest unique prefix of the symbol's hash, with a persisted assignment
table.** `blake3(scip_symbol)` → base32 → truncate to 2 characters, extend to 3, 4… on a
collision. The assignment is stored in `handles`, so it stays stable even after symbols are
added. Typically 2–4 characters.

A handle has to work after a daemon restart and in the next session — an agent may write it down
in its own notes.

### 6.6 Ranking — where quality is decided

`cairn symbol` may find 200 matches. We return 15. Which selection that is, is where the whole
thesis of "return little, and precisely" lives. The signals:

1. an exact name match > prefix > substring > fuzzy
2. **not generated code** (§7.3) — a hard down-rank
3. not a test (unless the query is about tests)
4. **reachable from an entrypoint** (§8.7) — dead code goes down
5. in-degree in the call graph (centrality)
6. recency of change (git, the last 90 days)
7. proximity to handles already mentioned in this session (session affinity)

Ranking is a testable component — it belongs in the measurement harness (§10), not in "we will
tune it later".

---

## 7. Cross-language and binders

Real systems are almost always more than one language, and the boundary between them is exactly
where every single-language tool goes blind. Cross-language is therefore not a late phase but a
basic capability.

**The general shape of the problem:** there is a *shared contract* — an IDL, a schema, a
convention — and several language sides that implement or consume it. The contract itself is in
the repository as an artefact (`.proto`, `.graphql`, an OpenAPI document, a shared type package).
The binder's job is to connect the contract's node with the symbols on both sides.

That shape is the same for gRPC, GraphQL, OpenAPI and shared TS types between a front end and a
BFF. All that differs is the rule for recognising which symbol belongs to which piece of the
contract.

### 7.1 A binder is a small plugin that produces edges between symbol IDs

Conceptually the signature is `fn bind(snapshot) -> Vec<Edge>`. Nothing more. Binders write into
the same `edges` table with `source = binder_name`.

### 7.2 The proto binder — the first instance of the general shape

```
proto/auth.proto
  service AuthService { rpc Verify(VerifyReq) returns (VerifyResp); }
        │                                    │
        ├── generates ──► auth_pb2_grpc.py ──► AuthServiceServicer.Verify   (py)
        └── generates ──► auth_grpc.pb.go  ──► AuthServiceClient.Verify     (go)
```

The binder reads the `.proto` (through a `protobuf` descriptor set or by plain parsing — here a
parser of one's own is exceptionally defensible, the grammar is trivial) and creates edges:

- `proto:AuthService.Verify` → `py:AuthServiceServicer.Verify` (implements)
- `proto:AuthService.Verify` → `go:AuthServiceClient.Verify` (calls)
- and thereby, transitively: the `go` caller → the `py` handler

**That is the jump neither grep nor any single-language tool makes:** "who calls this Python
handler" has its correct answer in Go code.

#### The implementation ↔ contract binding is carried by a rule, not by the binder's code

The binder extracts the nodes from the contract (`proto:AuthService.Verify`). **How an
implementation is recognised is a layer-B rule (§1.1)** — because it differs not only between
languages but between libraries within one language:

| stack | shape | rule |
|---|---|---|
| Python / grpclib | `inherits` | `class $X(…, $pkg.{Service}Base)` |
| Python / grpcio | `call_pattern` | `add_{Service}Servicer_to_server($impl, $srv)` |
| Go / protoc-gen-go-grpc | `call_pattern` | `Register{Service}Server($srv, $impl)` |
| TS / connect-es, nice-grpc | `collection_literal` | a map of methods → handlers |

It is worth noting that with inheritance **the binder needs to do nothing extra** — L0 gives the
`implements` edge for free and all that remains is mapping the generated base's name back to the
contract. The naming convention is also a rule, not code.

*Evidence (§16): in the test repository the first three rows of that table are real —
`class ChatServiceHandler(…, orders_api.ChatServiceBase)` on the Python side,
`regions_api.RegisterAreaQueryServiceServer(server, area.NewHandler(app))` on the Go side. Two
different shapes in one repository are exactly why this must not be hard-wired.*

#### The contract exists even without generated code

The `contract → expected symbol` edge can be built even when the generated artefact is missing,
because the naming is fixed by convention. The target is then marked `expected` rather than
`resolved`. Code that *imports* the generated types will never resolve without them — and that
is the expensive part, hence the degraded mode in §4.6.

Do not turn this into an ambition to generate anything ourselves. A convention is enough for an
edge; a body needs a build.

### 7.3 Generated-code detection (a small feature with an enormous effect)

Generated code tends to be most of a repository by volume and almost nobody wants it in an
answer. Without suppression every query drowns.

Detection is language-neutral and rests on three signals, in this order:

1. **a header marker** — `Code generated by … DO NOT EDIT.` (Go), `@generated` (common in the
   JS/TS ecosystem and elsewhere), `# Generated by …`
2. **`.gitattributes linguist-generated`** — the repository marks it itself
3. **path patterns from rules** (layer B) — `**/*_pb2.py`, `**/*.pb.go`, `**/generated/**`,
   `.next/**`, `dist/**`

The effect: collapse it into one line —
`+ 47 refs in generated code (suppressed; rerun with --include-generated)`. Plus a CAS namespace
of its own, so generated code can be skipped in the shared cache (§5.5).

*Evidence of scale (§16): 103,176 of the 158,874 lines of Go in the test repository are
generated — 65%. In a front-end repository the proportion will differ, but the problem is the
same.*

### 7.4 Further binders (later, the same mechanism)

A GraphQL schema ↔ resolvers ↔ client queries · OpenAPI ↔ server ↔ generated client · an ORM
model ↔ a table ↔ migrations · an env var ↔ reading configuration · shared types between
repositories · SQL in raw queries.

---

## 8. Deployment topology — entrypoints and service boundaries

### 8.1 Why this is not an add-on

A call graph with no roots is soup. Without entrypoints you cannot answer the questions that come
up first on a web project:

- Is this code reachable at all?
- Which service does this symbol run in?
- Which endpoint leads here?
- What has to be deployed when I change this?

`docker-compose.yml` gives the graph two things that never fall out of language servers:
**roots** (entrypoints) and **partitions** (services). Only then do reachability and blast radius
become meaningful.

And above all: compose is **the only file in the repository that describes the system as a
whole** — it is machine-readable, it is necessarily maintained (otherwise `docker compose up`
does not run) and it is in practice the best existing documentation of how the components talk to
each other. Ignoring it and looking for the topology in the code is extra work for a worse
result.

### 8.2 The chain: service → process → symbol → route

Both examples are **verified against the test repository** (§16), not illustrations.

**Python — the map comes from the volume mount, not from the Dockerfile:**

```
compose.yaml
  x-build-py: &build-py
    context: srcpy
  services.orders-grpc:
    <<: *base-service                          ← anchor: init, env_file
    build: *build-py                           ← anchor: context srcpy
    command: python3 -m domains.orders.grpc.server
    environment: [DJANGO_SETTINGS_MODULE=domains.orders.grpc.settings]
    volumes: ["./srcpy:/app/"]                 ← the container ↔ repo map
         │
         ▼  launcher resolver  (§8.4)
  srcpy/domains/orders/grpc/server.py :: __main__
         │
         ▼  route binder  (§8.6) / proto binder (§7.2)
  grpc OrderService.* → handlers
```

**Go — two hops through a multi-stage build:**

```
  services.scoring-grpc:
    build: *build-go                           ← anchor: context srcgo
    command: /bin/grpcserver
         │
         ▼  srcgo/Dockerfile, runtime stage
  COPY --from=builder /out/grpcserver /bin/grpcserver
         │
         ▼  srcgo/Dockerfile, builder stage
  RUN CGO_ENABLED=0 xx-go build -o /out/grpcserver \
        ./domains/orders/cmd/grpcserver/server.go
         │
         ▼  launcher resolver  (§8.4)
  srcgo/domains/orders/cmd/grpcserver/server.go :: main
```

Every arrow is deterministic and cheap. No LLM, no heuristics — just parsing and a table of known
patterns. But there are more arrows than it first appears: in Go the path runs through two
`COPY --from` / `-o` mappings and through the `xx-go` wrapper (buildx cross-compilation), not
through a bare `go build`.

### 8.3 The deployment descriptor — in general, and then compose

Compose is **one instance of a more general notion: a descriptor that names processes and their
start commands.** From any such descriptor the core always wants the same things:

| what the core needs | why |
|---|---|
| a list of deployable units | the graph's partitions |
| the start command of each | input for the launcher resolver (§8.4) |
| a bridge from unit to source directory | where that code actually lives |
| a map from runtime path to repository path | stack-trace translation, runtime traces (§9) |
| what is reachable from outside | the system's public surface |
| configuration and links between units | cross-service edges (§8.5) |

Known descriptors and their coverage of those six items:

| descriptor | covers | note |
|---|---|---|
| **Docker Compose + Dockerfile** | everything | the first implementation |
| `package.json` `scripts` | units, commands | often the only source in a JS/TS repository; monorepos via workspaces |
| Procfile / systemd unit | units, commands | a trivial parser |
| Kubernetes / Helm | everything, but through templating | §8.9 — once compose is proven |
| `Makefile` targets | commands | a last resort, unreliable |

The core works with that unified shape; every descriptor is a layer-C adapter (§1.1). **A
repository without Docker is therefore not out of scope** — it simply gets fewer fields filled in
and says so in `unknown:`.

#### compose (`docker-compose.yml`, `compose.yaml`, override files, `profiles`)

| field | what it gives |
|---|---|
| `services.*` | the list of deployable units = **the graph's partitions** |
| `build.context` / `build.dockerfile` | the bridge compose → Dockerfile → source directory |
| `image` without `build` | an external dependency (postgres, redis, nats) — a node, but not code |
| `depends_on` | edges between services |
| `ports` / `expose` | what is reachable from outside = **the system's public surface** |
| `networks` | who can reach whom at all |
| `environment` / `env_file` | input for the env binder (§8.5) |
| `command` | an override — it takes precedence over `CMD` in the Dockerfile |
| `healthcheck` | often the most accurate indicator of where a service actually listens |
| `volumes` | **the container ↔ repo map for local development** — it overrides `COPY` from the Dockerfile (§8.7) |
| `networks.*.aliases` | further DNS names for a service — without them the env binder loses edges (§8.5) |

**A correction after checking against the test repository:** an earlier version of this design
deferred `x-` extensions as marginal. That is wrong, and in practice it means parsing nothing:

- **`x-` blocks carry the anchors.** In the test repository every shared definition lives in
  `x-base-service`, `x-build-go`, `x-build-py` and `x-healthcheck-*`; services pull them in
  through the merge key `<<: *base-service`. Without resolving anchors and aliases you get neither
  the build context nor `env_file`.
- **Interpolation can be nested.** `${IMAGE_PREFIX:-${COMPOSE_PROJECT_NAME:-platform}}` is a real
  line. Compose interpolation has to be implemented, including `:-` defaults and `.env`.
- **`name:` at file level** determines the project name and therefore the DNS names.

The YAML parser must therefore preserve anchors and merge keys, not merely load them into a map.
Where not to go further: templating of build args, `.dockerignore` semantics, `profiles`
combinatorics.

**Dockerfile:**

| directive | what it gives |
|---|---|
| `WORKDIR` + `COPY`/`ADD` | **the container path ↔ repository path map** — needed for the L3 runtime trace too (§9) |
| `ENTRYPOINT` + `CMD` | the actual start command (distinguish shell from exec form) |
| `FROM … AS build` + `COPY --from=build` | which stage produces the runtime artefact |
| `RUN go build -o /app/server ./cmd/server` | the binary ↔ package bridge |

Respect compose's merge semantics (override files, `extends`). **Do not go into** `x-` extension
templating of build args or `.dockerignore` semantics — that is where the rabbit hole starts and
the value drops off steeply.

### 8.4 The launcher resolver — command → symbol

The input is **the start command as a string**, wherever it came from (§8.3). The output is a
symbol, or an honest `unknown`. It is not a shell interpreter but **the `command_string` shape
from §1.1** — rules in data, the resolver in the core.

```toml
[[launcher]]
id = "python.module"; lang = "python"
match = "python3? -m (?<mod>[\\w.]+)"
emit  = { module = "{mod}", symbol = "__main__" }
```

Sample rules across ecosystems — the point is to see that only the data differs:

| ecosystem | command | root |
|---|---|---|
| Python | `python -m pkg.server` | `pkg/server/__main__.py` |
| Python | `gunicorn pkg.wsgi:application` | the `application` symbol |
| Python | `celery -A proj worker` | **every `@shared_task` is a root of its own** |
| Go | `/bin/srv` | backwards through `build -o` → `cmd/srv/main.go::main` |
| Node | `node dist/server.js` | through a source map / build config back into `src/` |
| Node | `next start` | **by convention: `app/**/route.ts`, `pages/api/**`** (§8.6) |
| Node | `npm run start` | unfold through `package.json` `scripts` and resolve again |
| JVM | `java -jar app.jar` | the manifest's `Main-Class` |

Two things a rule cannot carry, which belong in layer C:

- **recursion through indirection.** `npm run start` → `scripts.start` → another command. The
  resolver has to be able to call itself with a depth limit.
- **a build artefact is not the source.** In Go the `-o` mapping from the Dockerfile solves it; in
  Node it is the bundler (`dist/`, `.next/`) and there the bridge is either a source map or the
  bundler's configuration. This is considerably worse in JS/TS than in Go and it is the main risk
  in §17.

Wrapper scripts (`entrypoint.sh`, `docker-entrypoint.sh`) are common in practice: read them and
look for the final `exec …`.

When any of that fails to resolve, it goes into **`unknown:`** — not a silent failure. That is a
direct consequence of D8 and it matters here more than anywhere else, because **a missing root
silently declares live code dead** (§8.7).

### 8.5 The env binder

`environment:` and `env_file:` give a set of variables. In the code we look for reads:
`os.environ[…]`, `os.getenv`, `os.Getenv`, `settings.X` in Django, `envconfig`/`viper` in Go.

Two kinds of edge arise and the second is the more valuable:

- `auth.env.DATABASE_URL` → `db/session.py:14` — **who reads it**
- `gateway.env.AUTH_GRPC_ADDR = auth:50051` → **the `auth` service** — who the target is

The second edge is a documented runtime link between services: the host in the URL matches the
name of another compose service. Together with the proto binder (§7.2) a closed picture emerges —
`gateway` (Go) calls `AuthService.Verify`, `auth` (Python) holds the implementation, and compose
confirms that these are two separate processes talking over `auth:50051`. Not one of those three
sources knows it alone.

**A correction: matching on the service name is not enough.** In the test repository `orders-grpc`
also has `orders-api_grpc-python` and `orders_grpc-python` in `networks.default.aliases`. The URL
in another service's env variable points at an alias, not at the service name — naive matching
would not see that edge at all. The binder therefore builds **a table of all DNS names** (the
service name + `container_name` + every alias across all networks) and matches against that.

`DJANGO_SETTINGS_MODULE` deserves a special mention: it is an env variable, but it is also **a
pointer to a module** (`domains.orders.grpc.settings`). It gives Django's per-service
configuration and is at the same time the cheapest way to configure `django-stubs` correctly for
each service separately (§4.4).

### 8.6 The route binder — request-level entrypoints

Compose gives roots at the level of processes. A web project needs roots at the level of
requests.

The worry that "we would have to know every framework for this" is understandable, but it falls
apart on inspection of real frameworks: **a route is declared by one of four shapes from §1.1**
and the core always assembles the path from them the same way.

| shape | frameworks | rule |
|---|---|---|
| `decorator` | FastAPI, Flask, NestJS, Spring | `@$router.{method}($path)` |
| `call_pattern` | Express, chi, gin, stdlib `ServeMux` | `$app.{method}($path, $handler)` |
| `collection_literal` | Django `urlpatterns`, Vue/React Router | a list of `($path, $handler)` |
| `path_convention` | **Next.js, Remix, SvelteKit, Nuxt** | `app/**/route.ts` → the path from the directory structure |

Assembling the path is then textual: the router/mount prefix + the path from the rule + nesting.

**`path_convention` is the shape that arrives with JS/TS** and has no analogue in Python or Go —
the route is declared nowhere, it is encoded in the *file path*. That is why it is in the list of
six shapes (§1.1) from the start, rather than being added later.

**Two extra pieces of information worth extracting when a framework offers them:**

- **a stable identity for the route.** FastAPI's `operation_id`, NestJS's method name, Next.js's
  file path. It is more stable than the URL and a better primary key than the path.
- **authentication.** When middleware/a guard/a dependency is attached declaratively
  (`dependencies=[Depends(auth)]`, `@UseGuards(...)`, `middleware.ts`), which routes are public
  can be derived statically. For the audit domain that is a saleable output on its own.

*Evidence (§16): three of those four shapes are real in the test repository — FastAPI decorators
(122 endpoints, including statically visible authentication), Django `urlpatterns`, and a single
stdlib `ServeMux` in Go. No chi, gin or echo. Details in
[coverage-analysis.md](coverage-analysis.md).*

**A cheap universal escape hatch, if the patterns were not enough.** Most web frameworks can print
their own routing table: FastAPI `app.openapi()`, Django `get_resolver().url_patterns`, Flask
`app.url_map`. The price is that the application has to be imported — so it is a runtime probe,
not static analysis, and it belongs in L3 (§9), not in L0. It has exactly the same duality as the
rest of the design: **static analysis is complete but approximate, runtime is exact but only over
the part it reaches.** When those two sources disagree, that is a finding, not a fault.

Recommendation: static patterns now, a runtime dump as an opt-in booster in the same phase as
coverage (§9). Definitely not the other way round — a runtime probe would turn a read-only tool
into one that executes someone else's code.

The output is an edge `route:POST /onboarding/signup` → the handler symbol. That answers "which
endpoint leads to this code", which in audit work and code review is the most frequent question
of all.

### 8.7 What follows for the other layers

**Service attribution.** Reachability from the roots gives every symbol a set of service labels.
Cheap, and it changes the shape of answers:

```
$ cairn blast a4 --depth 2
…
services affected (2)                                     [L0-D + L1]
  auth      py   direct
  gateway   go   via proto AuthService.Verify
externally reachable via
  POST /oauth/token · POST /oauth/refresh                  [route]
```

**Dead code.** A symbol unreachable from any root and not covered by a test is a candidate.
Return it as a signal with a confidence, never as a fact — reflection, dynamic imports and an
unresolved entrypoint can all get round it.

**A better seed for `cairn context`** — listed as source 0 in §6.4.

**A prerequisite for the L3 runtime trace.** A stack trace from a running container says
`/app/domains/orders/grpc/server.py`, the repository says
`srcpy/domains/orders/grpc/server.py`. Without the map a runtime trace is useless. That is why
this section precedes §9.

**A correction about where that map comes from.** An earlier version took it from `WORKDIR` +
`COPY` in the Dockerfile. On the test repository that is not enough: `orders-grpc` has
`volumes: ["./srcpy:/app/"]`, which **overrides** the `COPY . .` from the build — in the running
container what is under `/app` is a bind mount, not copied content. The order of priority is
therefore:

1. a `volumes` bind mount in compose *(authoritative for local development — our case)*
2. `WORKDIR` + `COPY`/`ADD` in the Dockerfile *(applies to a production image without mounts)*
3. `build.context` as a last resort

**A service is not an image.** In the test repository 15 services run from **two** build images
(`x-build-py`, `x-build-go`) — `orders-grpc`, `orders-api`, `catalog-pipeline` and others share
the same `srcpy` tree. **Which service a module belongs to therefore cannot be determined from
the filesystem**; only reachability from an entrypoint says so. That is not an edge case, it is
the strongest argument for this section existing at all.

### 8.8 `cairn topology` — a map of the system in ~400 tokens

The ideal first command for an agent to run in an unfamiliar repository. The skill says so
explicitly: *"Before you start reading files, run `cairn topology`."* Service attribution then
propagates as an annotation into the answers of the other commands.

The original design made this an MCP resource in order to save the tool budget. With D1 that
reason disappeared — it is simply a subcommand, and a human can run it too.

The real shape for the test repository (§16), abbreviated:

```
$ cairn topology

services (16, from compose.yaml + compose.local.yaml)
  orders-api          py   domains/orders/api/app.py            :8000  → public
  orders-grpc         py   domains/orders/grpc/server.py        :50051 grpc
  orders-proxy        go   cmd/resttransform/server.go             :8081
  orders-admin   py   manage.py runserver (django admin)      :8002
  orders-tools          py   domains/orders/mcp/…                 :8003
  scoring-grpc           go   cmd/grpcserver/server.go                :50052 grpc
  regions-grpc       go   cmd/server/server.go                    :50053 grpc
  catalog-pipeline  py   domains/catalog/…                  —
  media-grpc             go   cmd/server.go                           :50054 grpc
  postgres               ext  postgres:16                             :5432
  … 6 more

edges
  orders-proxy → orders-grpc   grpc orders_fe.*   [proto + net alias]
  orders-api   → orders-grpc   grpc orders_api.*  [proto]
  orders-grpc  → postgres         env DATABASE_URL
  …

public surface
  :8000  orders-api  122 HTTP routes (20 routers, 12 unauthenticated)
  :50051 orders-grpc 24 grpc services / 71 proto services total

unknown (0)
stale: none
```

The `12 unauthenticated` line is not cosmetics — it follows from the fact that
`app.include_router(x, dependencies=[Depends(get_authenticator())])` carries the authentication
information statically (§8.6). For the audit domain that is a saleable output in itself.

### 8.9 What not to do now

Kubernetes / Helm (the same mechanism, a different parser — once compose is proven), Terraform,
build-arg templating, Go routers in general, service mesh configuration. Compose + Dockerfile is
90% of the value for 10% of the work.

---

## 9. L3 — execution knowledge

The best value-for-cost ratio in the whole document. Without an LLM.

**From git history** (gix, no shelling out):
- a **co-change matrix** from `git log --name-only` over the last N commits; the score is PMI /
  lift, not a plain count (otherwise `README.md` and `go.mod` win)
- a **test impact heuristic** — tests changed together with the source
- **recency and ownership** for ranking

**From execution** (phase 2+):
- Python: `coverage.py` with **contexts** (`--context=test`) → a map from test to lines. That is
  real test impact, not a heuristic. One pytest plugin.
- Go: `go test -coverprofile` per package, or per test.
- Dynamic imports / a real call graph: `sys.settrace`. Precedent: MonkeyType (Instagram).

L3 artefacts go into the same CAS and the same `edges` table with `confidence < 1.0` and
`source = git|coverage|trace`. In an answer they are always visually separated from L1.

---

## 10. Measurement — a component, not an appendix

Without this the whole project is a hypothesis. `cairn-eval` is a crate, not a script.

20 real tasks from the test repository, run against a baseline agent and against a cairn agent:

| Metric | Target | Note |
|---|---|---|
| Tokens per task | −50% | the main thesis |
| Rounds to the first edit | −50% | shortening the exploration loop |
| Wall clock | ≤ baseline | must not be slower |
| Recall on L0/L1 queries | **100%** | below 100% the product is dangerous |
| Query latency p95 | ≤ 20 ms | on 500k lines |
| Cold start | ≤ 60 s | without a shared cache; with one ≤ 10 s |

**The baseline must have prompt caching switched on.** Without that you are comparing against a
straw man.

Recall is measured against a gold standard generated independently (brute force: an LSP crawl
over every symbol, once, offline).

---

## 11. Code layout

```
cairn/
  crates/
    cairn-cli       the binary: `cairn symbol|refs|blast|topology|status|daemon|index|eval`
    cairn-proto     shared types, msgpack, the front end↔daemon socket protocol
    cairn-skill     the skill for the agent (§6.2) — text, not code, but versioned with the CLI
    cairn-fmt       the renderer for compact answers  ← product surface, tested with snapshots
    cairn-daemon    supervisor, socket server, scheduler, deadlines
    cairn-store     CAS, the SQLite projection, snapshots, cache keying
    cairn-index     SCIP ingest, the LSP client pool, fact extraction
    cairn-rules     the engine for the six shapes (§1.1) + loading rules/*.toml and .cairn/rules.toml
    cairn-lang      language providers (layer C): python · go · typescript
    cairn-binders   adapters a rule cannot carry: proto, deployment descriptors
    cairn-graph     L1 derivation: references, call graph, blast radius, reachability, ranking
    cairn-git       gix, co-change, test impact, snapshots from a tree
    cairn-eval      the measurement harness
  docs/
    architecture.md
    adr/
```

The split follows the layers from §1.1. `cairn-rules` and `cairn-lang` are the boundary through
which ecosystems are added; `cairn-graph`, `cairn-store` and `cairn-fmt` know about no language
and **are not touched when a language is added** — that is the testable condition of D16, not a
pious hope.

Rule packs (`rules/*.toml`) are built into the binary but can be overridden by a
`.cairn/rules.toml` file in the repository.

**Runtime:** tokio. **Key crates:** `gix`, `rusqlite` (bundled, WAL), `notify`, `blake3`,
`rmp-serde`, `zstd` (with a trained dictionary, §5.5), `tower-lsp` or a thin LSP client of our
own, `scip` (the protobuf schema), `tree-sitter` plus grammars **for comments only** (§4.5), and a
YAML parser that preserves compose's merge semantics.

**Watcher:** `notify` with a 50 ms debounce, ignoring `.git`, `node_modules`, `__pycache__`,
`target`, `vendor`, and respecting `.gitignore`.

---

## 12. Latency and scheduling

The D7 principle: **a query has a deadline (200 ms by default) and never blocks on indexing.**

```
a query arrives
  ├─ everything fresh in SQLite?     → answer  (~2–20 ms)
  ├─ the file involved is dirty?     → LSP re-resolve of that file only (10–100 ms)
  │                                    made the deadline? → answer
  │                                    missed it? → answer from the old base + `stale: auth/oauth.py`
  └─ not indexed at all?             → answer what you know + `stale: cold index in progress (37%)`
```

A background queue, in priority order: dirty files > their direct dependents > the rest of the
project > L3 > L2.

---

## 13. Roadmap

| Phase | Contents | Output |
|---|---|---|
| **0** — week 1 | A spike on the test repository (§16), **entirely in a container** (D13): `make pbgen` → `scip-python` + `scip-go` → the proportion of unresolved symbols, the time, **the raw index size** (input for §5.5). Measure **twice: with the generated stubs and without** — the difference is the cost of §4.6. | Go/no-go on D3, calibration of D10 |
| **1** — weeks 2–6 | Daemon + store + CAS + snapshot + the dirty overlay. Comment extraction (§4.5) — it is free and the FTS schema has to have it from the start. `cairn symbol|refs|expand`. The CLI front end + the skill. | A usable product |
| **2a** — week 7 | The compose + Dockerfile binder, the launcher resolver, `cairn topology`, service attribution, a committable `.cairn/topology.txt` + `topology --check`. | A map of the system; the cheapest piece in the whole plan |
| **2b** — weeks 8–9 | The proto binder + the route binder + generated-code detection → cross-language and cross-service edges. `cairn blast`. | **The differentiator nobody else has** |
| **3** — weeks 10–12 | L3 from git (co-change, test impact). `cairn tests`. `cairn-eval` and the first measurement against a baseline. | **This is where it is decided whether the thesis holds** |
| **4** | `cairn context`, ranking, progressive-disclosure tuning. The skill. | The product layer |
| **4b** | **A second ecosystem: JS/TS** (§17). A provider plus a rule pack, zero changes in the core. | D16 confirmed in practice |
| **5a** | Sharing through `refs/cairn/cache` — needs no infrastructure at all (§5.6). | A shared cache in a few days |
| **5b** | A CAS server (a sync layer over the finished CAS). | Monetisation |
| **6** | L2 semantics. Coverage contexts. | Only on top of a finished structure |

**Never** (until it demonstrably hurts): parsers of our own, a storage engine of our own, event
sourcing of our own, mmap + a B+ tree.

Git is already an append-only event log and `git log` is the replay. An event store of our own is
months of work duplicating something that is free.

---

## 14. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| ~~`scip-python` cannot cope with Django~~ | — | **Settled by measurement: 0.11% unresolved.** Phase 0 closed |
| A cold start of ~3 min instead of 60 s | A worse first impression | Measured. The shared cache (§5.6) thereby moves from "monetisation" to "the thing that makes the first run bearable" |
| scip-python has no per-file incrementality | The LSP hot path is mandatory, not optional | Confirmed by measurement — §4.2 belongs to phase 1, not later |
| The package field in a SCIP symbol is not a project boundary | Third parties silently mix into answers | scip-python misattributes ~37k references to the project. Derive ownership from the set of indexed files |
| The agent does not use the tool and reaches for grep | The product does not exist | The skill as product work (§6.2). Measure the share of `cairn` calls against grep, even without the skill |
| `deps_api_hash` does not converge on real code | The cache is useless | Measure the hit rate in phase 1; fall back to a coarser key |
| Serena / a competitor arrives first | The delta disappears | The delta is persistence + cross-language binders + L3, not "we have a graph". Focus on those |
| Recall < 100% on L0 | **The product is dangerous** | A gold standard plus a regression in CI. Better to return `unknown` than to guess |
| An unresolved entrypoint → live code marked dead | A silent and very damaging error | An unresolved launcher is always `unknown`; the dead-code signal is **never** returned while even one unresolved root remains in the project |
| The index misses its size target | The shared cache loses its point | Measure in phase 0 on the raw SCIP index. The §5.5 techniques are incremental and can be added |
| Binders bloat (K8s, Terraform, every Go router) | Scope creep by the back door | §8.9 is a binding list. A new binder only with a documented occurrence in the test repository |
| Comments flood the full text with noise | `cairn context` gets worse, not better | Weighted FTS columns, detection of commented-out code, measure seed precision separately (§10) |
| The agent takes a stale comment as fact | Exactly the kind of silent error the whole design avoids | A comment in an answer is always `[comment, unverified]`; it never enters an L0/L1 assertion |
| `refs/cairn/cache` inflates the object database | Slow clone/fetch | The ref is prunable and force-pushable; fetching is optional. When it hurts, move to a CI artefact |
| Indexing over ungenerated codegen | A silent drop in recall that looks like an indexer failure | §4.6: detect, degrade, admit it in every answer. Never index blind |
| `inotify` over a bind mount on Docker Desktop (D13) | The dirty overlay stops working and the agent gets stale data | Fall back to polling; the front end can send an explicit invalidation. Native and fine on Linux |
| Container paths leak into answers | The agent cannot open a file cairn is telling it about | Absolute paths are already forbidden by §5.1; `cairn-fmt` snapshot tests guard it |
| The design overfits the test repository | Porting to JS/TS = a rewrite | D16 and §17. Rules in data; adding a language must not touch layer A |
| The rule language bloats into a DSL | Our own parser in a different disguise | The closed set of six shapes (§1.1). A new shape only on two independent real cases |
| JS/TS bundling breaks the command → symbol chain | Entrypoints in a front-end repository become untraceable | §17.4: skip the bundler through a source convention; where that fails, an honest `unknown` |
| Scope creep | A year with no product | There is one product in this document. Hold to phases 0–3 |

---

## 15. Open for decision

1. **Name and licence** — `cairn` is a working title. Open core (the server MIT, the shared cache
   paid)?
2. **Non-code knowledge** — PR discussions, issues, ADRs. "Why is this like this" is more often
   there than in the AST. It fits as another binder plus L2, but it is a product of its own. Out
   of scope for now.
3. **Multi-repo** — SCIP handles it by its nature. Once one repository is finished.
4. **Windows** — named pipes instead of a unix socket; otherwise unchanged. When?
5. **`orders-tools`** — the test repository already runs one MCP server. It is worth finding out
   what it does before we build a second: either it is an unrelated domain, or it is a signal
   about how the team uses MCP.

---

## 16. Calibration against the test repository

An internal repository, measured 2026-07-30.

**This section is evidence, not a specification.** The numbers below serve to calibrate phase 0
and to show that the mechanisms in §7 and §8 are not hypotheses. None of it may be hard-wired
into the core — see D16 and §1.1. The test in the other direction, on JS/TS, is in §17.

| | |
|---|---|
| Working tree size | 34 MB |
| **Measured in phase 0** | [spike-0-results.md](spike-0-results.md) |
| Python | 218,193 lines / 1,184 files · Django 5.2.6, pytest-django |
| Go | 158,874 lines / 516 files |
| Proto | 9,768 lines / 139 files |
| **Generated Go** | **103,176 lines = 65% of the Go code** (220 × `.pb.go`) |
| Generated Python | 13 files / **48,952 lines**, 51 `*ServiceBase` (betterproto2 dumps into `__init__.py`, not `*_pb2.py` — easy to miss) |
| Compose services | 16 (15 our own + postgres) from **2 build images** |
| Compose files | `compose.yaml`, `compose.local.yaml`, `compose.test.yaml` |
| Dockerfiles | 7 (`srcpy`, `srcgo`, their interpreter/compiler bases, `pbgen`, sentinel, postgres) |
| Go binaries from one Dockerfile | 8, through `xx-go build -o` + `COPY --from=builder` |

What fed directly from this into the design: §2.1 (D13), §4.6 (D14), and the corrections in §7.2,
§7.3, §8.2, §8.3, §8.5 and §8.7.

What still has to be measured in phase 0:

1. the proportion of symbols `scip-python` fails to resolve — **separately with `make pbgen` and
   without it**
2. the raw SCIP index size for both parts → calibration against the 50 MB target (§5.5)
3. whether the launcher resolver (§8.4) hits all 15 services, or how many end up in `unknown`
4. how many `.proto` services have both sides (a Go client and a Python servicer) — the size of
   the §7.2 delta
5. whether `orders-admin` / `catalog-admin` are Django admin, and therefore whether a route binder
   is needed for admin URLs as well

---

## 17. Testing the abstraction: what it costs to add JS/TS

The condition from D16 reads: **adding an ecosystem = one language provider (layer C) plus one
rule pack (layer B), zero changes in layer A.** Here is the walk-through for JS/TS, because that
is realistically the next target (the front-end repository). It also serves as a check on whether
the design holds — if it turned out that the core had to be touched, the design is wrong **now**,
not in a year.

### 17.1 Layer A — unchanged

Snapshot, CAS, `deps_api_hash`, the graph, ranking, handles, formatting, git L3, the daemon, the
CLI. None of it knows what language it is indexing. ✅

### 17.2 Layer C — one provider

| slot | for JS/TS |
|---|---|
| batch indexer | `scip-typescript` (Sourcegraph's, covers both JS and TS) |
| LSP for dirty files | `typescript-language-server` / `tsserver` |
| comment grammar | `tree-sitter-typescript`, `tree-sitter-tsx` |
| configuration resolver | **`tsconfig.json` `paths` / `baseUrl`** — necessary, otherwise `@/lib/x` does not resolve |

The last row is the only genuinely new work in layer C. It is the analogue of what
`DJANGO_SETTINGS_MODULE` does for Python and `go.mod` for Go — every ecosystem has its own place
where the module mapping is stored, and the core must not assume it.

### 17.3 Layer B — a rule pack

```
rules/typescript.toml
  launcher:  node dist/*.js · next start · vite preview · nest start · npm run <script>
  routes:    express call_pattern · nest decorator · next/remix path_convention
  codegen:   prisma client · graphql-codegen · next build types · openapi clients
  generated: **/dist/** · **/.next/** · **/*.generated.ts · @generated marker
  contract:  tRPC router · GraphQL schema · shared TS types
```

None of that requires a new shape except `path_convention` — and that has been among the six from
the start precisely because it was known it would arrive with JS/TS.

### 17.4 Where it will be unpleasant (and it is better to know now)

| problem | why it is worse than in Python/Go | what to do |
|---|---|---|
| **Bundling** | What is deployed is `dist/` or `.next/`, not the source. The "command → symbol" chain breaks on an artefact that is not even in the repository. | Skip the bundler: a rule maps `next start` straight onto a source convention rather than onto the build output. Where that does not work, `unknown:`. |
| **Monorepos** | pnpm/yarn workspaces, turbo. A "service" may be a package, not a container. | The `package.json` workspaces descriptor (§8.3) as a source of units alongside compose. |
| **Router fragmentation** | Express, Fastify, Nest, Next, Remix, SvelteKit side by side. | Precisely why the rules are data. Adding a pattern is a line of TOML. |
| **`node_modules`** | Enormous, and `scip-typescript` can pull it into the index. | A hard exclusion plus a CAS namespace of its own, as with generated code (§7.3). |
| **Types shared between repositories** | The front end imports types generated from a backend contract. | That is multi-repo (§15 point 3), out of scope for now. It stays `unknown` at the boundary. |

### 17.5 Verdict

The D16 condition holds: **zero changes in layer A, one provider, one rule pack.** The only
structural novelty is the `path_convention` shape and that is already in the design.

Bundling is a real risk and it is specific to JS/TS. It is not a hole in the architecture,
though — it is a place where the `unknown:` section will be longer, which is exactly what it is
for.

---

## 18. Controlling context — the tool does not know how much the agent wants

Until now the design has tacitly assumed that cairn itself knows what to return. That is wrong:
**how much context is needed, and in what shape, is known only to the caller**, and it changes
from query to query. An audit wants breadth, fixing a specific bug wants depth, and an agent with
twenty thousand free tokens wants something different from an agent with two hundred.

The interface therefore needs **controls**, not only queries. Not twenty switches, but four
orthogonal axes.

### 18.1 Four axes

| axis | what it says | example |
|---|---|---|
| **detail** | how much of each node | `--detail skeleton\|signature\|doc\|body` |
| **breadth × depth** | how far to go | `--depth 2 --fanout 8` |
| **aspect** | which edges are walked | `--aspect callers,impls,tests,routes,services` |
| **budget** | a hard ceiling | `--budget 2000` (tokens) |
| **view** | how it is rendered | `--view list\|tree\|path\|skeleton` |

The first three are obvious. The fourth is the interesting one. The fifth is the one that decides
whether the code falls apart.

#### Detail applies to a walk, not just to one symbol

The detail axis is not about displaying one symbol — it is about **how much code is printed from
every node we pass through**. That is the shape an audit needs: "walk this function's callers and
show me their bodies", because hunting for edge cases, broken conventions, security holes and
performance problems cannot be done from a list of names.

It is off by default, because it is the most expensive thing the tool can emit — and precisely
therefore it is also where `--budget` (§18.2) matters most. Two safeguards: one symbol's body has
its own ceiling in lines, so that a single long function cannot swallow the whole budget and end
the walk after the first node; and when the indexer does not know the extent of a body, only the
definition line is printed **and that is said**, rather than guessing where the body ends.

#### The view is separate from the selection

As soon as a call graph exists, so do ways of displaying the same knowledge — a flat list, a
tree, an A→B path, a file skeleton, a graph of edges. The combination of **axes × views** grows
multiplicatively, and if every query carried its own formatting it would end as N×M copies.

Hence the hard split: **cairn-store selects** (what, how deep, along which edges) and returns a
neutral `Walk`; **cairn-fmt renders** and knows nothing about queries. A new view is then one
`match` arm, not a new query, and a new query immediately supports every view.

That is also why `--view` is not merely cosmetic: `tree` and `list` have very different token
costs for the same information, so it is in fact another budget lever.

### 18.2 The budget as a first-class input

Today's agent has to guess the size of an answer through `--limit` and then regret it. Turn it
round: **the caller states a ceiling, and the tool is responsible for filling it with the most
valuable things** — and must report what it left out to do so.

```
$ cairn blast a4 --budget 1500

static callers (4 of 11 shown, ranked)            [L1, exact]
  [c1] LoginHandler.post           api/login.py:55
  …
suppressed: 7 callers below the budget cut
            (expand: cairn blast a4 --budget 4000, or --aspect callers --detail skeleton)
```

Why this fits the product's thesis: the whole pitch is "cheaper context". Letting the agent guess
a limit means it either overspends or asks for a second round — and a second round is exactly the
exploration loop we are here to remove. **The budget is the only place where the tool can
optimise better than the caller**, because it alone knows everything it has available and how it
is ranked.

Estimating tokens: `cairn-fmt` counts its own output, so the ceiling applies to the real answer
rather than to an estimate. An approximate count (characters / 3.7) is enough — there is no point
pulling in a tokeniser.

### 18.3 Writing: not only summaries

`cairn note` (§3.1) writes L2 summaries. But the caller also knows things worth storing that are
not summaries:

| what | why the agent knows it and cairn does not |
|---|---|
| **confirming or refuting a weak edge** (§18.4) | the agent read the code and saw whether that call really exists |
| **a symbol's role** ("this is the only entry point into payments") | it follows from the task, not from the AST |
| **negative knowledge** ("I looked here, it is not here") | it saves the next session a whole round |
| **a domain alias** ("what they call *order* here is *listing* elsewhere") | a bridge between jargon and names |

All of it goes into L2, that is, with a `confidence`, removable, and **never entering an L0/L1
computation** (D15). Negative knowledge is the cheapest and most underrated of these: "it is not
here" is information every session rediscovers today.

### 18.4 Hidden links — what static analysis cannot see and can still be found

You ask whether there are links we have not uncovered. There are, and some are cheap. What they
have in common is being **uncertain** — so they must not go into L1 among the exact edges, but
belong in a layer of their own, **L1-W (weak)**, with a confidence and a mandatory label in the
answer.

| detector | mechanism | cost |
|---|---|---|
| **String literals matching a symbol's name** | an index of literals × symbol names | trivial, high recall |
| A name in configuration / env / a feature flag | the same over the values from §8.5 | trivial |
| Django `"app.Model"`, Celery task names, DI keys | the `call_pattern` shape with a literal | a rule |
| A table name in SQL ↔ an ORM model | lexical matching | cheap |
| A URL literal in one service ↔ a route in another | the intersection of §8.6 and literals | cheap, cross-service |
| A pytest fixture's name ↔ a test's parameter | lexical, per framework | a rule |
| What changes together | git (§9) | already in the design |

The first row is worth expanding on, because it is the best cost/return ratio in the whole table:
**every string literal in the repository that exactly matches the name of some symbol is a
candidate dynamic reference.** It covers `getattr`, `importlib`, plugin registries, string-keyed
routing and serialisation maps — that is, a large share of those 123 dynamic sites from the
coverage analysis. It is purely deterministic (D15), it is one join, and the result comes back
as:

```
weak links (2)                                    [L1-W, unverified]
  plugins/loader.py:22   literal "TokenValidator" matches [a4]   (getattr call nearby)
  config/services.yaml:8 literal "auth.TokenValidator" matches [a4]
```

The agent either confirms it by reading the code or rejects it — and through §18.3 it can write
that back, so next time it does not have to ask.

### 18.4b Hand-written links and their expiry

The weak edges from §18.4 are machine candidates. Alongside them it must be possible to **simply
write a link down**: an agent or a human read the code and knows the connection exists even
though static analysis will never see it — dispatch through configuration, a contract held by
convention, a runtime dependency.

```
cairn link <from> <to> --note "why" --by agent|human
```

What matters is what happens to such a link on reindexing. **It is anchored to a place in the
code** (the file plus the line of the source symbol's definition) and that place is hashed. When
it changes:

- **it must not be silently discarded** — that would lose work static analysis cannot repeat
- **it must not be silently kept** — a stale assertion would be presented as fact

So it is **marked `needs_review` and honestly reported**: a hole has opened here that the static
pass cannot close, and a model needs to be run at it again. The flag is never cleared
automatically — only a fresh judgement clears it.

Provenance is part of the edge's type (`L2, agent-asserted` / `L2, human-asserted`), so a
hand-written link never mixes with an exact one.

### 18.6 One graph, not two

An option considered: let the agent build its own graph alongside ours, with primitive graph
tools. **Rejected**, and it is worth writing down why, because the need behind it is real.

**Two truths are worse than one incomplete one.** The product's thesis is that there is a layer
the agent trusts and stops double-checking. With a graph of its own, without provenance and
without invalidation, it would have to reconcile them on every query — and that is the
exploration loop, just one floor up.

**Invalidation does not carry over.** Our graph is invalidated by content hashes; a hand-written
link works only because it is anchored in the code (§18.4b). A free-floating graph has nowhere to
be anchored, so it rots silently.

**And it would be a different company.** A general graph store is a memory product, not code
navigation — it would belong on the "never" list right next to our own parsers and our own
storage engine.

#### What the need actually was: nodes that are not symbols

Exactly one kind of node was missing. "The OAuth flow", "the billing domain" are not symbols and
no indexer will emit them — so an agent that learns something has nowhere to put it. The solution
is not a second graph but **concepts inside ours**, plus concept ↔ symbol edges. Incidentally
that is also what `cairn context` (§6.4) needs.

Three conditions that keep this from being a graph database:

| condition | why |
|---|---|
| **An anchor** to a place in the code, with a hash | otherwise an assertion cannot be invalidated and rots silently |
| **Namespaces** | one session's guesses can be filtered or discarded wholesale, without affecting shared ones |
| **No properties, no query language** | a concept has a name, a note and links; more than that is a graph DB |

The relation type (`part-of`, `entry-point`, `owns`) is by contrast free text — the vocabulary
belongs to whoever is asserting, and a closed enumeration would only drive them back to a store
of their own.

#### Authored knowledge lives in a file of its own

`index.sqlite` is a projection and is discarded on reindexing and on a schema change. Authored
knowledge is the one thing that **cannot be re-derived**, so it must not share that fate — it
lives in `index-knowledge.sqlite` beside it and is attached with `ATTACH`.

A second thing follows: **authored rows reference symbols by hash, not by rowid.** A rowid is
reassigned on every indexing run and after a rebuild would point into nothing. A hash is stable
across rebuilds and across machines by its nature (§5.1).

When a symbol disappears from the index entirely — the code was renamed or deleted — the link is
**neither discarded nor presented as valid**, but reported as `symbol gone`.

#### What this supports at the same time

We have not cut off the "I will build my own view" scenario — `path:start-end` on every line and
the reference graph mean the agent **can** build its own graph whenever it wants. We support that
by being a good *source*, not by being its database.

### 18.5 What we do not want from this

Not an agent inside the tool. The axes in §18.1 are parameters of a deterministic selection, not
a planner. And not an unbounded set of detectors: every weak detector with low precision that
nobody confirms only inflates `weak links` and teaches the agent to ignore that section — which
would kill the useful ones too.
