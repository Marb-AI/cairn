# Phase 0 spike — results

**Target:** an internal repository · **Date:** 2026-07-30 · **Verdict: GO**

Purpose: settle D3 — can SCIP indexers actually handle this codebase? Everything ran
in containers (D13); nothing was installed on the host. Reproduce with:

```
cd spike && docker compose run --rm spike all
```

---

## 1. Headline numbers

| | scip-go | scip-python |
|---|---|---|
| Version | 0.2.7 | 0.6.6 |
| Documents | 537 | 1,169 |
| Occurrences | 244,990 | 347,218 |
| Raw index | 34.9 MB | 40.3 MB |
| Wall time | **28.8 s** | **2 m 28 s** |
| Peak RSS | 558 MB | **2.65 GB** |
| Project references | 68,461 | 130,650 |
| **Unresolved (project code)** | **0.00 %** | **0.11 %** |

**D3 holds comfortably.** The go/no-go threshold was ~15 % unresolved. Actual is
0.00 % and 0.11 %.

"Unresolved" counts reference occurrences pointing at a symbol that is defined
nowhere in the index. References into stdlib and third-party packages are excluded —
those dangle by construction, because those dependencies are not indexed.

Definitions differ per language and that is deliberate: scip-go puts the Go module
path in the symbol's package field, so an exact match identifies project code.
scip-python puts the project's distribution name there but *also* misattributes
third-party packages to it, so the descriptor's top-level module is matched against
the directories actually indexed instead. See `spike/scipstat.py::make_classifier`.

### What the 150 unresolved Python references are

Long tail, no cluster worth chasing:

```
   4x  `schema.orders_api`/default_scoring_profile#
   3x  `domains.orders.rds.identity.models`/AuthProvider#Protocol#
   2x  `domains.orders.settings`/
   2x  `domains.orders.admin.admin`/
   …
```

Mostly module-level symbols in settings and CLI entry modules. `AuthProvider#Protocol#`
is a nested class inside a Django model — the one recognisable pattern, and it is three
occurrences.

---

## 2. Calibration against the architecture targets

| Target | Source | Measured | Status |
|---|---|---|---|
| 100 % recall on L0/L1 | 3, 10 | 99.89 % / 100 % unresolved-free | close enough to justify the design; the gap must become `unknown:` output, not silence |
| Cold start < 60 s | 10 | 28.8 s + 148 s = **~3 min** | **missed** — see 4.1 |
| Full index ≤ 50 MB compressed | 5.5 | 75 MB raw, uncompressed | plausible after 5.5 techniques; needs measuring |
| Generated code is a large share | 7.3 | **58.3 % of Go occurrences** | confirmed |

---

## 3. Confirmed design decisions

**7.3 — generated-code suppression is not cosmetic.** 220 of 537 Go documents and
142,887 of 244,990 occurrences (58.3 %) are generated. Without suppression the
majority of any answer would be `.pb.go` noise.

**5.5 — the index is big enough that serialisation matters.** 75 MB raw for a
377k-line repo. Interning, varint deltas and zstd are not premature.

**4.6 / D14 — indexers are not zero-config.** scip-python needed a `pyrightconfig.json`
pointing at the venv, plus an `--environment` file. Without dependencies installed it
would resolve neither Django nor grpclib. A daemon has to own this configuration per
language; "just run the indexer" is not a thing.

**14 — the SCIP ecosystem really is in flux.** During this spike: `scip-go` had moved
from `github.com/sourcegraph/scip-go` to `github.com/scip-code/scip-go` and now requires
Go ≥ 1.25; the `scip` CLI cannot be `go install`-ed at all because its `go.mod` carries
`replace` directives. Both were listed as a risk and both materialised within one hour.

---

## 4. New findings

### 4.1 Cold start misses its target, and the fix is already in the design

~3 minutes against a 60 s goal, and scip-python peaks at **2.65 GB RSS** — enough to
matter on a developer laptop running the app at the same time.

This does not invalidate anything, because the design never depended on a fast cold
start: content-addressed artifacts (5.1) mean it happens once per machine, the shared
cache (5.6) means once per team, and the LSP hot path (4.2) handles everything after.
But it does reorder priorities: **the shared cache moves from "monetisation" to
"the thing that makes the first run bearable"**, and `refs/cairn/cache` (phase 5a)
becomes more attractive than it looked.

### 4.2 Partial reindex works — and scales opposite to expectation

Neither bulk indexer has a "reindex this one file" mode, but both can be pointed at a
subset: `scip-python --target-only <path>` and `scip-go index <package-patterns>`.
Measured cost of a partial run (full index for reference: 2 m 28 s / 28.8 s):

| target | documents | time | peak RSS |
|---|---|---|---|
| py `libs/util` (leaf package) | 3 | **1.3 s** | 168 MB |
| py `…/grpc/handlers/settings.py` (one file) | 10 | **6.6 s** | 326 MB |
| py `…/grpc/handlers` (26 files) | 183 | 26 s | 894 MB |
| py `domains/orders` | 916 | 1 m 40 s | 2.67 GB |
| go `./domains/regions/cmd/...` | 4 | **12.9 s** | 497 MB |
| go `./domains/media/...` | 14 | 19.5 s | 575 MB |
| go `./domains/orders/...` | 120 | 23.5 s | 504 MB |

**Python scales down, Go does not.** Cost for Python tracks the transitive closure it has
to pull in, so a leaf file is ~1 s and a dependency-heavy one ~7 s. scip-go pays a fixed
~13 s to load and typecheck the module graph before it indexes anything, so narrowing
from 99 packages to 4 buys only 55 %.

Consequences for the design:

- Python gets a **middle tier** between the LSP hot path and a full reindex: a targeted
  partial run in seconds. Worth using for "this file and its package" after a save burst.
- Go has no such tier. Either the LSP hot path (gopls) answers, or you pay ~13 s minimum.
  This makes the gopls path **load-bearing for Go specifically** — more than for Python.
- Either way the LSP hot path stays mandatory from phase 1 (section 4.2); nothing here
  reaches the 10–100 ms an editor-speed answer needs.

### 4.2b Peak memory can be capped, cheaply

Both indexers honour a memory ceiling, at a modest time cost:

| | limit | peak RSS | time | result |
|---|---|---|---|---|
| scip-python | `NODE_OPTIONS=--max-old-space-size=1536` | 2.65 GB → **1.65 GB** | 2 m 28 s → 2 m 53 s (+17 %) | identical index, exit 0 |
| scip-go | `GOMEMLIMIT=300MiB GOGC=50` | 558 MB → **387 MB** | 28.8 s → 37.9 s (+32 %) | identical index, exit 0 |

So a machine-wide ceiling is a supported configuration, not a hack: the daemon can expose
one knob and translate it per language. A container `mem_limit` should still sit underneath
as a hard backstop — both flags above are soft (V8 heap, Go soft limit) and do not cover
allocations outside them.

### 4.2c LSP hot path: the 10-100 ms assumption holds

Architecture 4.2 assumed a warm language server answers about one changed file in
10-100 ms, and the dirty overlay leans on it. It had never been measured. Measured now
against the real repo, with both servers warmed and the file edited via `didChange`:

| | pyright (Python, 1,169 files) | gopls (Go, 99 packages) |
|---|---|---|
| `initialize` | 205 ms | 62 ms |
| `documentSymbol`, warm | 1.5-1.8 ms | 0.9-1.2 ms |
| `references`, first call | **1,353 ms** | 4.7 ms |
| `references`, warm | 130 ms | 1.1 ms |
| **`didChange` + `documentSymbol`** | **4-5 ms** | **3.6-7.3 ms** |
| **`didChange` + `references`** | **94-115 ms** | **23-27 ms** |

The assumption survives: an edit followed by a structural question costs single-digit
milliseconds, and an edit followed by a reference lookup costs tens of milliseconds — at
the top of the assumed band for Python, comfortably inside it for Go.

Two honest caveats, both of which make these numbers optimistic:

* **The measured symbol had no references.** The harness picks the first symbol with a
  range and it happened to have none, so the reference timings exclude result
  marshalling. A symbol with hundreds of references will cost more.
* **Warm-up was a fixed 40 s wait, not a measurement.** How long after start the pool is
  actually usable is still unknown, and it matters: pyright's *first* cross-file query
  took 1.35 s even after that wait, so the first query following a daemon start is in a
  different class from the rest.

One thing the numbers say that the design did not anticipate: **pyright is roughly 4x
slower than gopls on the hot path**, which inverts the batch picture, where Go's indexer
was the one with no cheap partial mode. The two paths have opposite shapes, so the
daemon should not treat the languages symmetrically.

*Harness: `spike/lsp_bench.py`. Its first version reported a 180 s timeout for every
pyright request — that was the harness, not pyright: a client must answer the server's
`workspace/configuration` request or pyright blocks before serving anything.*

### 4.3 scip-python misattributes third-party packages

Roughly 37,000 reference occurrences point at symbols whose package field says
`orders_api` but whose module is `betterproto2`, `grpclib`, `pytest` or
`django`. The `--environment` file did not fully fix it.

Consequence for cairn: **the package field of a SCIP symbol cannot be trusted as the
project boundary.** Ownership has to be derived from the indexed file set. Cheap to do,
but it has to be done deliberately — code that trusts the package field will silently
mix third-party symbols into project answers.

### 4.4 Empty-looking files

163 Python files have fewer than 5 occurrences. Spot-checking shows these are genuine
`__init__.py` package markers, not silent failures. Worth a second look in phase 1
against a golden standard rather than by eye.

---

## 5. Correction to the coverage analysis

An earlier claim in [coverage-analysis.md](coverage-analysis.md) — that the Python
protobuf stubs are absent from the repo and that this costs the whole gRPC surface —
**was wrong, and has been retracted.**

The stubs are committed. betterproto2 emits one large `__init__.py` per proto package
rather than `*_pb2.py` files: 13 files, **48,952 lines, 51 `*ServiceBase` classes**.
The original check counted files and searched for a filename pattern that this
generator never produces.

Measurement confirms it: the index contains all 51 `*ServiceBase` definitions and all
34 `*ServiceHandler` definitions, and regenerating the stubs changes nothing
(2,183 `*ServiceBase` references before and after).

The general mechanism in architecture 4.6 stands — generated artifacts can be missing,
and other ecosystems have no CI guard — but this repo is not an example of it.

---

## 6. Open for phase 1

1. **Compressed index size.** Only raw size was measured. Section 5.5 targets ≤ 50 MB
   transferred; apply the techniques and measure.
2. **Django ORM depth.** The aggregate says the ORM is not a problem, but `Model.objects`
   and reverse accessors were not probed individually. Needs a targeted query set.
3. **The 123 dynamic call sites** (`getattr`, `importlib`) from the coverage analysis
   were not classified. That number sizes the `unknown:` sections.
4. **LSP hot path latency.** Untested, and 4.2 makes it load-bearing — especially for Go,
   which has no cheap partial-reindex tier.
5. **Golden standard for recall.** 0.11 % is an internal-consistency measure, not recall.
   Real recall needs an independent reference set.
