# cairn

A local CLI that answers structural questions about a codebase for a coding agent —
deterministically, in one call, instead of a dozen rounds of grep.

> **Status: pre-alpha, closer to a proof of concept than a product.**
>
> It indexes **Python and Go only**, and it has been measured against exactly one
> codebase. Nothing here is stable: the CLI surface, the exit codes, the index format and
> the rule pack schema can all change without notice, and an index built by one version
> will not be read by the next.
>
> Other languages are deliberately not next. The plan is to use this in anger on real work
> first and find out what is actually wrong with it; adding JavaScript, TypeScript or Rust
> before that would be widening something whose shape is not settled. Adding a language is
> meant to be a rule pack rather than a patch — whether that holds is one of the things
> the testing period has to answer.
>
> Use it if the two languages match your stack and you want to help find the edges. Do not
> build anything on top of it yet.

```
$ cairn affects <handle>       # every deployed service a change here touches
$ cairn reaches <handle>       # who calls this Python handler from Go, per RPC
$ cairn unreached <path>       # what production never calls
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

## Installing

Using cairn does not require Docker — it is one binary with no runtime dependencies. On
Linux and macOS:

```
curl -fsSL https://raw.githubusercontent.com/Marb-AI/cairn/main/install.sh | sh
```

That drops the binary for your OS and architecture in `~/.cairn/bin` and links it onto
your PATH. Re-run it to upgrade. `CAIRN_VERSION`, `CAIRN_HOME` and `CAIRN_LINK_DIR`
override the tag, the install directory and the link directory.

Windows: take the `.exe` from the [releases page](https://github.com/Marb-AI/cairn/releases).
Published builds are Linux (x86-64, arm64), macOS (Apple Silicon) and Windows (x86-64,
arm64).

## Setting it up

cairn does not parse code itself. You give it a SCIP index, which a language's own indexer
produces — so step one belongs to that indexer, not to cairn:

```
scip-python index . --project-name <name> --output py.scip
scip-go --output go.scip
```

Then build the index. It is written to `.cairn/index.sqlite` inside the repository it
describes, and every later command finds it by searching upward from the working
directory, the way git finds its own repo:

```
cairn index py.scip go.scip --repo .    # offline, no API key
cairn status                            # what is indexed, and how stale it is
```

`--repo` is worth passing: it lets cairn read file headers to recognise generated code by
its marker rather than guessing from filenames, and it is what makes the deployment
topology resolvable at all.

**The first index is slow, and almost all of that is the indexers, not cairn.** On the
codebase this was measured against — roughly 71k symbols across Python and Go — the run
was:

| step | wall time | peak RSS |
|---|---|---|
| `scip-go` | 28.8 s | 558 MB |
| `scip-python` | 2 m 28 s | 2.65 GB |
| `cairn index` over both | ~36 s | bounded, see below |

So about three and a half minutes cold, and the memory is the part worth planning for:
scip-python peaking at 2.65 GB will be killed on a small container. Capping it with
`NODE_OPTIONS=--max-old-space-size=1536` brought that to 1.65 GB for 17 % more wall time
and produced a byte-identical index.

After that it is not a cost you pay again per question — queries run in milliseconds
against the built index, and rebuilds are incremental against a staging file that only
replaces the live index once it is complete, so a rebuild never takes the index away from
whoever is reading it.

From there, everything takes a short handle, and `cairn symbol` is how you get one:

```
cairn symbol UploadReadings     # -> handles
cairn usage <handle>            # where it is used, grouped by file
cairn graph <handle> --aspect callers
cairn affects <handle>          # every deployed service a change here touches
```

Optionally, run the daemon. Without it the index is only as fresh as the last build and
cairn says so in every `stale:` line; with it, edits since the build are tracked live:

```
cairn daemon --repo .
```

The agent-facing guide is [`skill/SKILL.md`](skill/SKILL.md). It leads with when *not* to
reach for the tool, which measurement showed matters more than the command list does.

## Configuring it

Two files, and they answer to different owners.

**Installation settings** live beside the binary, at `~/.cairn/bin/cairn.yaml` (or wherever
`$CAIRN_CONFIG` points). One binary serves every repository on the machine, so whether it
records sessions should not depend on which directory you are standing in. Absent is the
normal state and means the defaults:

```yaml
tracking: false        # append one line per command to <index dir>/sessions/<id>.jsonl
memory_peak: false     # report peak resident memory on stderr when a command finishes
memory_limit_mb:       # ceiling on resident memory; default is a quarter of machine RAM
default_budget:        # ceiling on an answer in tokens, when --budget is not given
max_budget:            # refuse to exceed this whatever --budget says
```

`tracking` is off unless you switch it on, including for internal builds. A tool that
starts recording what you searched for without being asked is one people stop trusting.
The memory ceiling is not decoration: indexing is the only path that can grow without
bound, and exceeding the ceiling aborts the build rather than letting the OS pick a process
to kill. Aborting is safe — a build assembles a staging file and the live index is
untouched until it is promoted.

**Conventions** are per repository, because they describe that codebase. What a start
command looks like, how a protobuf generator names its artefacts, what marks a file as
generated, where tests live — none of that is hardcoded:

```
cairn rules > .cairn/rules.yaml    # dump the conventions in effect, then edit
```

Editing that file changes behaviour without rebuilding cairn. It is also the mechanism a
new language is supposed to arrive through, which is the claim the testing period has to
either confirm or kill.

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

**Where it wins, and why.** Both winning classes have one shape: a *bounded* question
answered from an edge the caller cannot see. A gRPC boundary joins two languages that share
no identifier, so a name search cannot even begin. A deployment topology is not in the
filesystem — a service and its source tree are not the same thing, and several services can
share one. cairn is handed the edge; the baseline has to discover it first.

**Where it does not.** When the answer sits in one file, or in prose, or the question has no
stopping point, an index only adds round trips. The last two rows are exactly that, and they
are in this table on purpose. cairn tries to notice and say so:

```
STOP - every seed is a module __init__ or a test file, so nothing here answers
       your question. Read <the one file it named>; do not keep querying.
```

That one message took a losing task from 13 tool calls to 7.

**How much to trust these numbers.** One repository, one language pair, one model — and a
repository you cannot inspect, which is a fair thing to hold against them. Two of the five
targets were chosen by a fixed rule rather than by hand, and both scored *better* than the
hand-picked ones, which is the opposite of what overfitting would predict. The baseline
itself varies by up to 1.8x run to run on open-ended questions, so anything below about
15 % here is noise and is reported as such.

Numbers from public repositories are the obvious next step and are planned: they would be
reproducible by anyone, which these are not.

The only supported stack today is **Python + Go** (gRPC, Django ORM), deployed with Docker
Compose. That is not a coincidence — it is the stack it was built against, and
[docs/generality-audit.md](docs/generality-audit.md) is an honest account of how much of it
is still shaped by that one codebase.

→ [docs/architecture.md](docs/architecture.md)

## Working on cairn

Building it, what CI enforces, how the tests are layered and how a release is cut are in
[docs/development.md](docs/development.md). None of it is needed to use the tool — cairn
ships as a plain binary and does not require Docker to run.

→ [docs/architecture.md](docs/architecture.md) — how it works and why
→ [docs/generality-audit.md](docs/generality-audit.md) — what is still shaped by the one
codebase it was measured on

## Licence

MIT. See [LICENSE](LICENSE).
