# cairn

**Your coding agent spends most of its budget finding the code, not changing it.** It
greps, opens the wrong file, greps again, and twelve tool calls later it has assembled
something you could have told it in one line.

cairn answers the structural questions directly. Who calls this. What breaks if I change
it. Which deployed services run this code. What does production never reach. One call,
deterministic, no API key, no model.

```
$ cairn affects ez5
[ez5] Service.UploadReadings — affects 2 deployed service(s)        [L1 + L0-D]

in-process
  telemetry-collector  /bin/collector

over the network, by hop - every route below reaches this symbol
  alert-worker       -> telemetry-collector telemetry.TelemetryService  (1)
    upload_readings in srcpy/alerting/worker.py
suppressed: none
unknown (3):
  1 service(s) start nothing and run code on demand instead - cron, a management
    command, `docker exec` (metrics-sidecar). Where one appears above it is on a
    path; how it is triggered is not something reachability can see
  hops are calls through a generated gRPC client, and are exact. A service that
    reaches this some other way - a hand-written transport, a queue, an HTTP call
    - is not here
  ...
stale: none
```

Note what the second half is doing. Every reply says what it left out, what the mechanism
cannot see, and what has changed since it looked. **A confident wrong zero is the failure
this exists to prevent** — an agent cannot tell "nothing calls this" from "I could not
tell", and it will act on the first.

That answer crosses a language boundary, by the way: the caller is Python, the callee is
Go, and nothing in either file names the other.

> ### Status: pre-alpha, closer to a proof of concept than a product
>
> **Python and Go only.** Measured against exactly one codebase. Nothing is stable — the
> commands, the exit codes, the index format and the rule pack can all change, and an
> index built by one version will not be read by the next.
>
> Other languages are deliberately not next. The plan is to use this on real work first
> and find out what is actually wrong with it. Adding a language is meant to be a rule
> pack rather than a patch; whether that holds is one of the things this period has to
> answer.
>
> Use it if those two languages are your stack and you want to help find the edges. Do not
> build on it yet.

## Install

```
curl -fsSL https://raw.githubusercontent.com/Marb-AI/cairn/main/install.sh | sh
```

Linux and macOS. Drops a binary in `~/.cairn/bin` and links it onto your PATH; re-run to
upgrade. On Windows, take the `.exe` from the
[releases page](https://github.com/Marb-AI/cairn/releases).

Builds are published for Linux (x86-64, arm64), macOS (Apple Silicon) and Windows
(x86-64, arm64). No runtime dependencies, no Docker.

Indexing needs the indexer for each language you have. cairn runs them for you but does
not ship them:

```
go install github.com/scip-code/scip-go/cmd/scip-go@latest   # for Go
npm install -g @sourcegraph/scip-python                      # for Python
```

## Set up

```
cairn config memory_peak=on     # print peak memory when a command finishes
cairn config                    # show everything in effect
```

Settings belong to the installation, not to a repository — one binary serves every
checkout on the machine. `cairn config` lists what exists and where it is stored; the
one worth knowing about is the memory ceiling, since indexing is the only thing here that
can grow without bound. It defaults to a quarter of the machine's RAM, and exceeding it
aborts the build rather than letting the OS pick a process to kill. Aborting is safe: the
live index is untouched until a rebuild finishes.

## Index a repository

Stand in the root of the codebase and run:

```
cairn index
```

That is the whole command. It walks the tree, sees which languages are actually there —
by what the files are, not by whether someone left a `go.mod` in the root — runs the
matching indexers, and builds the index into `.cairn/`, which it also makes uncommittable.
There is no flag for the repository: where you are standing is what you meant.

**The first run is slow, and nearly all of it is the language indexers.** Measured on a
codebase of roughly 71k symbols:

| step | wall time | peak memory |
|---|---|---|
| Go | 29 s | 0.6 GB |
| Python | 2 m 28 s | 2.6 GB |
| building the index | ~36 s | bounded by the setting above |

About three and a half minutes cold. The memory is the part to plan for — 2.6 GB will be
killed in a small container. After that, queries are milliseconds, and a rebuild never
takes the index away from whoever is reading it.

If an indexer is missing, cairn says which, tells you how to install it, and indexes the
rest — then states plainly that answers about the missing language will be incomplete
rather than empty.

Then just ask:

```
cairn symbol RateLimiter       # -> a short handle
cairn usage <handle>           # where it is used, grouped by file
cairn affects <handle>         # which deployed services a change reaches
cairn unreached srcpy/billing  # what production never calls
```

Nothing else to run. A file watcher starts itself the first time you use cairn in a
repository, so answers stay honest about edits made since the index was built; you never
have to know it exists.

## Does it actually help?

Measured against the same agent doing the same task with grep and file reads, on a sample
of our own codebase — Python and Go, roughly 71k symbols, closed source. Three runs per
arm, medians, identical prompts.

| question | baseline | with cairn | tokens | wall clock |
|---|---|---|---|---|
| which deployed services does a change here touch? | 102 280 | 27 355 | **−73 %** | **−87 %** |
| which Go code reaches this Python gRPC handler? | 59 324 | 28 534 | **−52 %** | **−50 %** |
| the same question, target picked by rule not by hand | 68 702 | 23 600 | **−66 %** | — |
| what in this package is called only from tests? | 62 098 | 66 075 | +6 % | — |
| enumerate a module's exported surface and its constraints | 60 043 | 60 877 | +1 % | — |

**Where it wins.** Both winning classes have one shape: a *bounded* question answered from
an edge the caller cannot see. A gRPC boundary joins two languages that share no
identifier, so a name search cannot even begin. A deployment topology is not in the
filesystem — a service and its source tree are not the same thing, and several services
can share one. cairn is handed the edge; the baseline has to discover it first.

**Where it does not.** When the answer sits in one file, or in prose, or the question has
no stopping point, an index only adds round trips. The last two rows are exactly that, and
they are in the table on purpose. cairn tries to notice and say so:

```
STOP - every seed is a module __init__ or a test file, so nothing here answers
       your question. Read <the one file it named>; do not keep querying.
```

That one message took a losing task from 13 tool calls to 7.

**How much to trust this.** One repository, one language pair, one model — and a
repository you cannot inspect, which is fair to hold against it. Two of the five targets
were chosen by a fixed rule rather than by hand, and both scored *better* than the
hand-picked ones, which is the opposite of what overfitting predicts. The baseline itself
varies by up to 1.8x run to run on open questions, so anything under about 15 % here is
noise and is reported as such.

Numbers from public repositories are the obvious next step and are planned: they would be
reproducible by anyone, and these are not.

## More

- [`skill/SKILL.md`](skill/SKILL.md) — the agent-facing guide. Leads with when *not* to
  reach for the tool, which measurement showed matters more than the command list.
- [docs/cli-reference.md](docs/cli-reference.md) — every command and flag.
- [docs/architecture.md](docs/architecture.md) — how it works, and why it is shaped this way.
- [docs/generality-audit.md](docs/generality-audit.md) — what is still tied to the one
  codebase it was measured on.
- [docs/development.md](docs/development.md) — building, testing, releasing.

MIT licensed. See [LICENSE](LICENSE).
