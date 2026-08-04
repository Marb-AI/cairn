# Verifying the quality of the index

Written 2026-08-04, all of it built: `cairn entrypoints`, the coverage axis in
`cairn status`, and `cairn llm verify`.

## The problem

Indexing is deterministic and that is its strength — the same input gives the same output
and nothing is guessed at. The price is that the whole index rests on conventions: what a
start command looks like, how a protobuf generator names services, what marks a file as
generated, where tests live. `.cairn/rules.yaml` can override them, but somebody has to
know that.

Where it breaks:

- **Messy code.** Dispatch through a string, registries of callables, a hand-written
  transport instead of the generated client. Static reachability cannot see those edges
  and says nothing about it.
- **Very custom setups.** A bespoke launcher, an unusual layout, a service started by
  something that is not in the repository.
- **Tooling outside the codebase.** A deployment script in another repository, an internal
  CLI, orchestration living alongside. The index only sees the tree it was given.

The point is that **in all three cases the index looks healthy**. It returns an answer, the
envelope says `unknown: none`, the exit code is 0. A missing edge is not an error message,
it is an absence — and an absence is indistinguishable from "there is nothing there".

This is a different class of problem from what `cairn verify` watches for today (known
unknowns: symbols without a definition, references without a caller) and from what
`cairn status` reports (counts and staleness). Both describe what the index knows about
itself. Nothing checked whether what the index claims matches reality.

## `cairn llm verify`

**Built.** An active, sampled check: take the few places where a failure would show up
first, and actually go through them.

### cairn calls no model

The most important decision in the whole piece. Architecture D1 says "a CLI, not an MCP
server: the agent runs commands natively" — and that agent **is** a model. So cairn needs
no LLM, no key, no network and no choice of model. It needs a **protocol**: it states a
claim it cannot settle itself, names the evidence and what would falsify it, and takes a
verdict back.

```
cairn llm verify                                  lists the claims needing a judgement
cairn llm verify --check <id> --holds
cairn llm verify --check <id> --broken --note "…"
```

Every line of the plan carries `claim:` / `look at:` / `wrong if:` / `record:`. The last of
those is deliberate — a plan that leaves the reader to work out the next step gets skimmed
and confirmed wholesale. And `wrong if:` is there because a check with no failure mode
described gets confirmed by default, which is worse than not asking.

### What it asks about

Two areas, not everything that could be checked. A plan long enough to skim is a plan
nobody works through.

- **entrypoints** — the chain of parses (compose command → Dockerfile hop → cron line →
  runner script) is right until some repository does it slightly differently. The file it
  lands in is the one thing a reader can check in a single look.
- **cross-api** — both sides are real code; whether they are two ends of **one** boundary
  is an argument about naming, and no compiler settles arguments about naming.

Per-language checks are not asked for yet: language coverage is deterministically
`indexed`, and putting that to a judgement is a weaker idea than either of these two.

### Three rules that do not bend

- **Advisory, never a gate.** No verdict changes an exit code and none blocks anything. The
  moment a deterministic tool refuses to work because a non-deterministic check disagreed,
  the determinism — the only reason to trust it — is gone.
- **Unverified is cheap and normal.** Most indexes will never be verified and that has to
  read as ordinary, not as a warning. A report that nags stops being read.
- **A verdict says where it came from.** `failing` from counting is a fact; `failing` from a
  judgement is an opinion that may be wrong. They land in the same column, so the text has
  to keep them apart — which is why it carries *"a judgement, not a derivation - re-check
  it before acting"*.

A verdict may only lift an area **up from `indexed`**. A pass that produced nothing cannot
be judged into having produced something; if a judgement overrode a count, an opinion would
stand where a derivation belongs — and the derivation is the thing that has to survive
disagreement.

### A bug found on the way

Authored knowledge **never persisted**. `Store::open` opens the projection with
`SQLITE_OPEN_READ_WRITE` and no `CREATE`, an attached database inherits those flags, so
`ATTACH` to a sidecar that did not exist yet failed, fell through to `:memory:` — and every
note, link, concept and verdict was accepted, acknowledged and discarded. `cairn concept
add` printed "recorded" and stored nothing. Nobody noticed because every test that touched
the authored side used one connection and never reopened it.

Two fixes: the sidecar is created explicitly before `ATTACH`, and when it genuinely cannot
be created (a read-only checkout) that is said on stderr instead of silently standing in.
A test now holds it by writing, closing, and opening again.

Why a judgement and not an assertion: the question is "does this result look like the truth
about this codebase", which is exactly what cannot be written as a rule — if it could, it
would be a rule in `rules.yaml` and the index would apply it directly.

## The coverage axis in `cairn status`

**Built.** It ended up in `status` rather than in a new `index status`: `status` was already
"what is indexed and how stale it is", and the coverage axis is a better-organised version
of exactly the counts it was printing. A fourth command overlapping both `verify` and
`status` would have been worse than extending one of them.

```
coverage   what each mechanism produced, against the tree it was built from
           indexed -> verified -> verify stale -> verified. `indexed` is the pass
           having run and matched the tree, which is counting; the rungs above
           are judgements recorded by `cairn llm verify` and expire with the tree
  entrypoints    indexed       4 entrypoints, 3 resolve to code, 1 idle
  cross-api      indexed       2 gRPC services, 5 serve, 4 call links
  python         indexed       270 symbols from 120 files
! rust           not indexed   88 files in the tree, 0 symbols in the index - ...
  TypeScript     not covered   41 files in the tree, no indexer exists - grep is the tool
  -> answers that rest on rust are incomplete or empty, and will not say so
```

### The ladder: `indexed` → `verified`

The good state is called **`indexed`**, not `verified`, and that is not a choice of
synonym. cairn *is* an index — a pass that did its job has indexed something, and no other
word is needed for it. A side effect: `indexed` / `not indexed` are finally opposites.
`verified` next to `NOT INDEXED` were two different vocabularies, which is why it grated.

`verified` is **the rung above**, reserved for what being indexed does not settle. A gRPC
edge is recovered from a generator's naming convention rather than from a resolver — so
"the pass produced links" and "those links are real" are two different statements, and
counting never reaches the second. Only what exists can be verified, so **without `indexed`
there can be no `verified`**.

### Verification decays like everything else

A verification is not a stamp for good, it is a claim with a date on it:

```
indexed -> verified -> (compose, Dockerfile, cron script or .proto changes)
        -> verify stale -> verified
```

The treacherous part is that **neither side notices on its own**. The evidence a
verification rests on is mostly not source — so reindexing does not renew the claim, and
editing a deployment file does not disturb the index. The index looks healthy, the
verification looks valid, and it no longer holds.

What `llm verify` has to remember: **one value — the commit it was made against.** Nothing
more. A fingerprint of every input is unnecessary, because git already holds what changed,
and a diff is better besides: a fingerprint says only *that* something moved, a diff says
what — and that is the difference between re-verifying one area and re-verifying all of
them.

The division of labour that follows: **cairn detects that the tree moved; the agent pulls
the diff, judges whether it matters, and records that.** Whether an edited Dockerfile
invalidates a claim about an entrypoint is exactly the judgement `llm verify` exists to
make, and comparing hashes would only be impersonating it.

Open: a verification over a dirty tree has no commit to anchor to — either refuse it, or
record the SHA plus a "this was dirty" flag and treat it as expired at the first movement.
And second: this would be a **second staleness mechanism** in one tool — hand-authored
links and concepts are invalidated today through content hashes of their anchor, precisely
so they do not depend on git history (shallow clones, worktrees). Before adding git-based
staleness it is worth deciding whether both should stay.

There are **eight** states in the end, because "empty" has several meanings and merging any
two of them is exactly the mistake this was built for:

| state | what it means | what to do |
|---|---|---|
| `indexed` | the pass ran and its output matches the tree | nothing |
| `verified` | that, and confirmed by a check that ran the query | nothing |
| `verify stale` | verified against a tree that has since moved | run `llm verify` again |
| `partial` | it produced output and something named is missing | read what is missing |
| `failing` | the inputs were there and nothing came out | this is the one worth waking up for |
| `not indexed` | cairn can index this and did not | fixable |
| `not covered` | cairn has no indexer for this | a limit of the tool; reach for grep |
| `n/a` | the question does not arise here | nothing |

`verify stale` is **not** trouble and gets no `!`: the area is still indexed and still
answers, only the extra claim has lapsed. Standing next to `failing` it would mix two
opposite reactions — one wants `llm verify` run again, the other wants the index rebuilt.

The vocabulary is **in one register**, all lowercase. It is a closed set of names in one
column, not sentences in prose — cairn does shout elsewhere (`STOP -`, `USE GREP FOR THIS
ONE`), but there capitals have surrounding text to stand out against. In a column there is
nothing to stand out against, and the only thing achieved would be making the reader decode
identity and severity from the same token. Severity gets its own column: `!` in the gutter,
plus the closing line.

Two things that emerged while building it:

- **Zero gRPC links was comparable to nothing.** `status` used to say "0 — and that is this
  mechanism failing, not an answer", but in a repository without protobuf zero is the right
  answer. The count of `.proto` files is now stored at index time, so `n/a` (no protobuf) can
  be told from `failing` (protobuf is there and nothing parsed).
- **The tree and the index name languages differently** — `python` against `py`. Without
  reconciling them the same language appears twice: once fully indexed and once missing
  entirely, which is worse than either row alone.

The exit code does not change: while the index is readable, `status` returns 0. Adding
another route to a 3 would undo the fix that stopped agents ignoring exit 3. The division is
that `status` informs and `verify` judges (returning 3 when the index is not clean).

## Entrypoints as a queryable set

**Built.** The data was already in the index: `deploy_services` (start command →
`entry_file`) and `deploy_on_demand` (`schedule`, `script`, `command`, `entry_file` — cron,
management commands, backups). But they were only read inside the reachability computation,
and `cairn topology` shows `deploy_services` alone, so there was no way to list them as a
set.

The unit is the **entrypoint, not the service**: a container with three cron jobs is one
service and four ways in. Every row ends in the file it lands in, so the next question is
`cairn outline <that path>` — the point of listing them is having somewhere to descend from.

Two uses, and the second is the more interesting one:

- **Forwards:** `cairn entrypoints`. The way in for somebody who does not know how the thing
  is started.
- **Backwards, for an audit:** `cairn entrypoints --reaches <h>`. The question is not who
  calls `OrderComponent`, but whether it can be reached from a path somebody actually
  starts. That is a different question from `graph --aspect callers` and it is answered from
  the entrypoint down. The fixture corpus shows it on `handle_frost`: `outline` reports it as
  `unused` because it has no static caller at all, and `entrypoints --reaches` shows the
  nightly cron job runs it.

The backward direction is also a cheap way to do the verification from the previous section:
`entrypoints --reaches` and `runs` derive the same fact from opposite ends. When they
disagree, the index has just reported a hole in itself — and needed no model to do it.

Two things found while building it, both fixed:

- **A cron entry using `python -m` never resolved.** On-demand entries were resolved without
  a build context, so `python -m alerting.dispatch` looked for `/alerting/dispatch.py` while
  the index holds `srcpy/alerting/dispatch.py`. A management command is found by suffix and
  never needed the context — which is why this survived: the shape that had been measured
  happened to be immune. The build context of the service the cron line names is now used.
- **The entrypoint's label in `deploy_reach`** was assembled in SQL in two places. A
  difference of one space between the writer and the reader would have joined on nothing and
  reported it as "no entrypoint runs this symbol" — confidently, and wrongly. It is one
  constant now.

Remaining: the fixture only covers cron. `docker exec` without a schedule, and management
commands, still have only unit tests.

## Shell and Makefile

There is no SCIP for shell or Make and there probably never will be — SCIP models symbol
resolution across files, and shell has none of that (no imports, lookup through `PATH`,
dispatch by string). The route would be tree-sitter (grammars exist for both `bash` and
`make`) plus a small extractor.

Half of this knowledge is already recovered without parsing shell, though:
`crates/cairn-store/src/ondemand.rs` reads cron lines and runner scripts and resolves the
chain `cron → docker exec → .sh → its exec line → entry file → call graph`. The header of
that file describes it as a deliberate decision ("Shell is not parsed, only read").

Makefiles are not in it yet and would be the cheapest addition: target → command line →
`entry_file` goes through the same start-command resolver `deploy` already uses on compose
`command:`. The side effect that was the main point — which flags and configurations are
actually used — then shows up in what those targets run.
