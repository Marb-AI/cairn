# Fixture corpus

A small, invented codebase that exists so the contract sweep (`tests/sweep.rs`) has
something to run against in CI. It is not example code and nobody should copy from it.

Everything here is written for this purpose. It borrows no code, names, or domain from any
real project — the domain is a weather-telemetry network precisely because it resembles
nothing we work on.

## What it is shaped to exercise

The sweep runs every read command against a sample of the index, so the corpus has to
contain the things those commands are about:

* **A cross-language gRPC boundary, in both directions.** Go serves `TelemetryService`
  and calls `AlertService`; Python serves `AlertService` and calls `TelemetryService`.
  This is what `reaches` is for, and it is the one edge no name search can find.
* **Generated code, marked by header.** `srcgo/gen/telemetrypb/` and
  `srcpy/schema/telemetry/` carry `Code generated ... DO NOT EDIT.`, which is how the
  indexer is supposed to recognise them — not by filename.
* **A deployment topology.** `compose.yaml` starts one service from a built Go binary and
  one with `python3 -m`, the two command shapes the rule pack parses differently.
* **Dispatched calls.** `srcpy/alerting/dispatch.py` reaches its handlers through a
  registry, so they are called by nothing a call graph can see. Reporting them as dead
  code is the defect this guards.
* **Tests, separable from production code.** `_test.go` files and `srcpy/tests/`, so
  `--exclude-tests` has something to exclude.

## Regenerating the SCIP indexes

The indexes are built from these sources, not written by hand:

    docker compose -f crates/cairn-cli/tests/fixtures/compose.yaml run --rm scip

Do that whenever a source file here changes, and commit the result alongside it. The
indexers live in the container; nothing is installed on the host.
