# Working on cairn

Everything here is about changing cairn itself. Installing and using it is in the
[README](../README.md); nothing on this page is needed to run the tool.

## Building

Development is Docker-only; nothing is installed on the host — no `cargo`, Node or Go
toolchain:

```
docker compose run --rm dev cargo build --release
docker compose run --rm dev cargo test --release
```

Two images, and the difference matters when you are waiting on one:

* `dev` also carries the language servers the daemon drives — gopls, pyright, node. Use it
  when you are working on the dirty overlay or want `cairn daemon` to actually answer.
* `ci` is the lean one: the Rust toolchain and nothing else. It is what CI builds, and it
  is enough for `cargo build`, `cargo test`, `cargo clippy` and `cargo fmt`.

Distribution is a plain binary — GitHub release targets — so *using* cairn will not require
Docker. The container is for building and testing it.

### Checking the Windows build without Windows

The release matrix builds the MSVC targets on real runners, but waiting for a tag to find
out that a `#[cfg(windows)]` block does not compile is too slow a loop. The `ci` image
carries the GNU Windows target, which shares every one of those code paths and the same
`windows-sys` bindings:

```
docker compose run --rm ci cargo clippy --workspace --target x86_64-pc-windows-gnu -- -D warnings
```

macOS has no equivalent — nothing here can compile for Apple Silicon, and the first tag is
where those code paths are exercised for the first time.

## What CI enforces

Every push and pull request runs the lot, and all of it is a hard gate:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target x86_64-pc-windows-gnu -- -D warnings
cargo test --workspace
```

The full five-platform matrix runs only on a `v*` tag. macOS runners bill at ten times the
rate and Windows at twice, so running all five per commit would spend a month's minutes in
a week.

## Tests

Three layers, and each exists because the one before it was not enough.

- **Unit tests** cover the pure functions: symbol parsing, command shapes, naming
  conventions.
- **Corpus cases** assert against a real indexed codebase — counts, membership, exit codes,
  and *latency ceilings*. They are data, so adding one needs no Rust.
- **The contract sweep** (`crates/cairn-cli/tests/sweep.rs`) stops choosing what to look
  at: it walks the index at a fixed stride and runs every read command against each symbol
  it lands on, asserting only what must hold for *any* symbol — no panic, an exit code from
  the contract, an envelope on every answer, nothing over a time ceiling.

The last two need an index, and a fresh clone has none, so both fall back to a fixture
corpus in `crates/cairn-cli/tests/fixtures/` — an invented Go and Python codebase shaped
around the things that are hard: a gRPC boundary crossed in both directions, generated code,
handlers reached through a registry, two shapes of deployment. It exists so that CI running
green means the cases ran, rather than that they skipped.

The fixture's SCIP indexes are committed, so CI never runs an indexer. Regenerate them
whenever a source file under `tests/fixtures/corpus/` changes:

```
docker compose -f crates/cairn-cli/tests/fixtures/compose.yaml run --rm scip
```

A private checkout with its own indexed codebase can add a second case file at
`eval/corpus/cases.yaml`; it is not in this repository, and its absence is the normal case.

The unit tests caught none of the eight correctness defects found in the first day of
measurement: every one lived in the interaction between the index and a real codebase rather
than in a function that could be tested alone. The corpus cases found two more within minutes
of existing, and then an off-by-one in the fix for the first of those. If you change
behaviour here, the second layer is the one that will tell you.

## Releasing

Push a tag and the rest is automatic:

```
git tag v0.1.0 && git push origin v0.1.0
```

That builds five binaries — Linux x86-64 and arm64, macOS arm64, Windows x86-64 and arm64 —
and publishes a GitHub release with generated notes. Wait for CI to be green on the branch
first: the tag workflow is the expensive one, and a failure there costs the whole matrix.
