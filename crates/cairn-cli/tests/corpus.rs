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
        let out = Command::new(&bin)
            .arg("--db")
            .arg(&db)
            .args(&case.args)
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
