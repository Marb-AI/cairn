//! Correctness cases run against a real indexed corpus.
//!
//! The unit tests in this workspace check pure functions — how a symbol name parses, what
//! a start command resolves to. They are worth having and they caught none of the eight
//! defects a day of measurement turned up, because every one of those lived in the
//! interaction between the index and an actual codebase: a dispatched method reported as
//! run by nothing, a handlers package reported as entirely dead, a truncated list that
//! looked complete, a correctness fix that cost two orders of magnitude of latency.
//!
//! The cases are data, so that adding one needs no Rust.
//!
//! Two corpora, two case files. The fixture corpus in `tests/fixtures/` is the one that
//! ships: it is invented, it is committed, and its cases in `tests/fixtures/cases.yaml`
//! run everywhere, including CI. A private checkout can add a second set at
//! `eval/corpus/cases.yaml` asserting facts about that codebase; those cases quote real
//! names and counts, so they are not in this repository and their absence is normal.
//!
//! The point of shipping the first set is that "there is no corpus here" must not quietly
//! mean "correctness is not checked here".
//!
//! Skipped, loudly, only when even the fixture SCIP is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

mod common;
use common::build_fixture_index;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    args: Vec<String>,
    /// Which index to run against. Absent means the real one.
    ///
    /// The degraded states are where a tool is most tempting to leave untested and most
    /// dangerous when wrong: an agent reads the exit code and a confident `0` over a
    /// broken index is worse than any wrong answer, because nothing downstream doubts it.
    #[serde(default)]
    db: Option<String>,
    /// Environment for the run, so precedence between `--db`, `$CAIRN_DB` and discovery
    /// can be asserted rather than assumed.
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    /// Working directory. The index is found by searching upward, and the only way to
    /// test that is to run from somewhere else.
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    exit: Option<i32>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    not_contains: Vec<String>,
    /// A ceiling, not a benchmark. Set well above the observed time so it catches a
    /// regression of the kind that turned a four-second command into a five-minute one,
    /// without failing because a machine was busy.
    #[serde(default)]
    max_seconds: Option<u64>,
}

/// Broken indexes to run against, built once.
///
/// Deliberately made here rather than committed: a corrupt SQLite file in the repository
/// is a thing someone will one day try to open.
fn make_fixtures(real: &Path) -> std::collections::HashMap<String, PathBuf> {
    let dir = std::env::temp_dir().join("cairn-corpus-fixtures");
    let _ = std::fs::create_dir_all(&dir);
    let mut out = std::collections::HashMap::new();

    out.insert("missing".to_string(), dir.join("does-not-exist.sqlite"));
    let _ = std::fs::remove_file(dir.join("does-not-exist.sqlite"));

    let empty = dir.join("empty.sqlite");
    let _ = std::fs::write(&empty, b"");
    out.insert("empty".to_string(), empty);

    let garbage = dir.join("garbage.sqlite");
    let _ = std::fs::write(
        &garbage,
        b"this is not a database, it is a sentence.\n".repeat(64),
    );
    out.insert("garbage".to_string(), garbage);

    // A structurally valid index whose deployment layer resolved nothing: the state of
    // every repository without a compose file cairn can read, which is most of them. The
    // corpus itself resolves two entrypoints, so without this fixture the branch that says
    // "UNCHECKED rather than empty" has nothing to exercise it - and that branch exists
    // because `topology` used to answer "0 services" there with `unknown: none`.
    let no_deploy = dir.join("no-deploy.sqlite");
    std::fs::copy(real, &no_deploy).expect("copying the index to make a no-deploy fixture");
    {
        let conn = rusqlite::Connection::open(&no_deploy).expect("opening the fixture");
        conn.execute("UPDATE deploy_services SET entry_file = NULL", [])
            .expect("clearing the fixture's resolved entrypoints");
    }
    out.insert("no-deploy".to_string(), no_deploy);

    // An index written by an older build: the binary must refuse it rather than read
    // whatever the old layout happens to put where it now expects something else.
    let old = dir.join("old-schema.sqlite");
    std::fs::copy(real, &old).expect("copying the index to make an old-schema fixture");
    let conn = rusqlite::Connection::open(&old).expect("opening the old-schema fixture");
    let changed = conn
        .execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .expect("ageing the fixture's schema version");
    // Asserted, not hoped for: a fixture that silently fails to be broken makes the case
    // pass for the wrong reason, which is worse than the case not existing.
    assert_eq!(changed, 1, "meta.schema_version not found in the fixture");
    drop(conn);
    out.insert("old_schema".to_string(), old);

    // An index moved or copied without the sidecar beside it. Authored knowledge is
    // optional; its absence used to make every command fail.
    let lone = dir.join("no-sidecar.sqlite");
    std::fs::copy(real, &lone).expect("copying the index without a sidecar");
    let _ = std::fs::remove_file(dir.join("no-sidecar-knowledge.sqlite"));
    out.insert("no_sidecar".to_string(), lone);

    out
}

/// The repository the corpus index describes. `/repo` inside the build container, a
/// sibling checkout on a host.
fn indexed_repo(root: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("CAIRN_TEST_REPO") {
        return PathBuf::from(p);
    }
    if Path::new("/repo").exists() {
        return PathBuf::from("/repo");
    }
    root.join("../repos/backend")
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/cairn-cli.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn binary() -> PathBuf {
    // The test binary lives in target/<profile>/deps; the CLI is two levels up.
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cairn")
}

#[test]
fn corpus_cases_hold() {
    let root = workspace_root();
    let bin = binary();
    assert!(bin.exists(), "cairn binary not built at {}", bin.display());

    // The index belongs to the codebase it describes, not to cairn's own tree — the same
    // place `cairn` itself would look for it when run from inside that repository. Where
    // there is no such checkout, the fixture corpus stands in with its own case file: the
    // real cases assert facts about a real codebase and cannot be made to hold anywhere
    // else, but "no corpus" must not silently mean "no correctness cases".
    // `eval/corpus/cases.yaml` is not in this repository: its cases quote names and counts
    // from a closed codebase. Whoever has that checkout drops the file in beside it; for
    // everyone else the path simply does not exist and the fixture cases run instead.
    let real = indexed_repo(&root).join(".cairn/index.sqlite");
    let real_cases = root.join("eval/corpus/cases.yaml");
    let (db, repo, cases_path) = if real.exists() && real_cases.exists() {
        // Where the indexed source tree is *from here*. The same cases run on a host and
        // inside the build container, which mounts the tree at /repo, so an absolute path
        // written into a case would pass in one and fail in the other for no real reason.
        let repo = std::env::var("CAIRN_TEST_REPO").unwrap_or_else(|_| {
            if Path::new("/repo").exists() {
                "/repo".to_string()
            } else {
                root.join("../repos/backend").to_string_lossy().to_string()
            }
        });
        (real, repo, real_cases)
    } else {
        let fixtures = root.join("crates/cairn-cli/tests/fixtures");
        match build_fixture_index(&root, &bin, "corpus") {
            Some(db) => (
                db,
                fixtures.join("corpus").to_string_lossy().to_string(),
                fixtures.join("cases.yaml"),
            ),
            None => {
                eprintln!(
                    "SKIP: no index at {} and no fixture SCIP to build one from",
                    real.display()
                );
                return;
            }
        }
    };

    let fixtures = make_fixtures(&db);

    let text = std::fs::read_to_string(&cases_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", cases_path.display()));
    let cases: Vec<Case> = serde_yaml::from_str(&text)
        .unwrap_or_else(|e| panic!("parsing {}: {e}", cases_path.display()));
    assert!(
        !cases.is_empty(),
        "no cases defined in {}",
        cases_path.display()
    );

    // Every case runs before anything is reported, so one failure does not hide the rest —
    // a suite that stops at the first problem tells you least when it matters most.
    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        let started = Instant::now();
        let mut cmd = Command::new(&bin);
        match case.db.as_deref() {
            // `none` passes no --db at all, which is how discovery and $CAIRN_DB get
            // exercised: with the flag present they can never be reached.
            Some("none") => {}
            None | Some("default") => {
                cmd.arg("--db").arg(&db);
            }
            Some(kind) => {
                cmd.arg("--db").arg(
                    fixtures
                        .get(kind)
                        .unwrap_or_else(|| panic!("unknown db fixture {kind:?}")),
                );
            }
        }
        for (k, v) in &case.env {
            cmd.env(k, v.replace("{db}", &db.to_string_lossy()));
        }
        if let Some(dir) = &case.cwd {
            cmd.current_dir(
                dir.replace("{root}", &root.to_string_lossy())
                    .replace("{repo}", &repo),
            );
        }
        let out = cmd
            .args(case.args.iter().map(|a| a.replace("{repo}", &repo)))
            .output()
            .unwrap_or_else(|e| panic!("running {:?}: {e}", case.args));
        let elapsed = started.elapsed();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let combined = format!("{stdout}{stderr}");

        let mut problems: Vec<String> = Vec::new();

        if let Some(want) = case.exit {
            let got = out.status.code().unwrap_or(-1);
            if got != want {
                problems.push(format!("exit {got}, wanted {want}"));
            }
        }
        for needle in &case.contains {
            if !combined.contains(needle.as_str()) {
                problems.push(format!("missing {needle:?}"));
            }
        }
        for needle in &case.not_contains {
            if combined.contains(needle.as_str()) {
                problems.push(format!("present but should not be: {needle:?}"));
            }
        }
        if let Some(limit) = case.max_seconds {
            if elapsed.as_secs() > limit {
                problems.push(format!(
                    "took {:.1}s, ceiling is {limit}s",
                    elapsed.as_secs_f64()
                ));
            }
        }

        if !problems.is_empty() {
            failures.push(format!(
                "\n  {}\n    cairn {}\n    {}\n    --- output ---\n{}",
                case.name,
                case.args.join(" "),
                problems.join("\n    "),
                combined
                    .lines()
                    .take(12)
                    .map(|l| format!("    | {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} corpus cases failed:{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
