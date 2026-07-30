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

### 4.2 Python has no per-file incremental path

scip-python indexes a whole project; there is no "reindex this one file". The design
already routes dirty files through the LSP hot path (4.2), so this is covered — but it
means the LSP path is mandatory from day one, not an optimisation. A 2.5-minute
full reindex on every save is not a fallback.

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
4. **LSP hot path latency.** Untested, and 4.2 makes it load-bearing.
5. **Golden standard for recall.** 0.11 % is an internal-consistency measure, not recall.
   Real recall needs an independent reference set.
