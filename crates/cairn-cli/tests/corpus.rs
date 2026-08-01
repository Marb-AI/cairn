//! Correctness cases run against a real indexed corpus.
//!
//! The unit tests in this workspace check pure functions — how a symbol name parses, what
//! a start command resolves to. They are worth having and they caught none of the eight
//! defects a day of measurement turned up, because every one of those lived in the
//! interaction between the index and an actual codebase: a dispatched method reported as
//! run by nothing, a handlers package reported as entirely dead, a truncated list that
//! looked complete, a correctness fix that cost two orders of magnitude of latency.
//!
//! The cases are data (`eval/corpus/cases.yaml`) so that adding one needs no Rust.
//!
//! Skipped, loudly, when there is no index: a fresh clone has none, and a test that fails
//! for want of a fixture teaches people to ignore it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

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
    let _ = std::fs::write(&garbage, b"this is not a database, it is a sentence.\n".repeat(64));
    out.insert("garbage".to_string(), garbage);

    // An index written by an older build: the binary must refuse it rather than read
    // whatever the old layout happens to put where it now expects something else.
    let old = dir.join("old-schema.sqlite");
    std::fs::copy(real, &old).expect("copying the index to make an old-schema fixture");
    let conn = rusqlite::Connection::open(&old).expect("opening the old-schema fixture");
    let changed = conn
        .execute("UPDATE meta SET value = '1' WHERE key = 'schema_version'", [])
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
    let db = root.join(".cairn/index.sqlite");
    if !db.exists() {
        eprintln!(
            "SKIP: no index at {}. Build one with `cairn index <file.scip> --repo <dir>` \
             to run the corpus cases.",
            db.display()
        );
        return;
    }
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

    let fixtures = make_fixtures(&db);
    let bin = binary();
    assert!(bin.exists(), "cairn binary not built at {}", bin.display());

    let text = std::fs::read_to_string(root.join("eval/corpus/cases.yaml"))
        .expect("reading eval/corpus/cases.yaml");
    let cases: Vec<Case> = serde_yaml::from_str(&text).expect("parsing cases.yaml");
    assert!(!cases.is_empty(), "no cases defined");

    // Every case runs before anything is reported, so one failure does not hide the rest —
    // a suite that stops at the first problem tells you least when it matters most.
    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        let started = Instant::now();
        let db_for_case = match case.db.as_deref() {
            None | Some("default") => db.clone(),
            Some(kind) => fixtures
                .get(kind)
                .unwrap_or_else(|| panic!("unknown db fixture {kind:?}"))
                .clone(),
        };
        let out = Command::new(&bin)
            .arg("--db")
            .arg(&db_for_case)
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
