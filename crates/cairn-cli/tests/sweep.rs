//! Every command against a mechanically chosen sample of the index.
//!
//! The hand-written corpus cases assert what specific answers should be. They are only as
//! good as my guesses about where to look, and every area probed by hand today produced a
//! defect — which says more about the guessing than about the code.
//!
//! So this stops choosing. It walks the index at a fixed stride, runs every read command
//! against each symbol it lands on, and asserts the things that must hold for *any* symbol
//! rather than the right answer for a particular one:
//!
//!   * it never panics, and the exit code is one the contract defines;
//!   * the envelope is always present, because a missing `unknown:` reads as "this is
//!     everything" and that is the silent error the whole design exists to prevent;
//!   * it never says `suppressed: none` while also reporting that it cut something —
//!     three separate truncation bugs found today all had exactly this shape;
//!   * nothing takes longer than a ceiling, because the worst regression of the day was
//!     purely latency and no correctness assertion would have seen it.
//!
//! A stride rather than a random sample: the same symbols every run, so a failure is
//! reproducible and a fix can be verified.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const STRIDE: usize = 997; // prime, so the sample is not aligned with any file boundary
const CEILING_SECS: u64 = 10;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn binary() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cairn")
}

#[test]
fn every_command_holds_its_contract_across_the_index() {
    let root = workspace_root();
    let db = root.join(".cairn/index.sqlite");
    if !db.exists() {
        eprintln!("SKIP: no index at {}", db.display());
        return;
    }
    let bin = binary();
    assert!(bin.exists(), "cairn not built at {}", bin.display());

    let conn = rusqlite::Connection::open(&db).expect("opening the index");
    let mut stmt = conn
        .prepare(
            "SELECT h.handle FROM handles h JOIN symbols s ON s.id = h.symbol_id
              ORDER BY h.symbol_id",
        )
        .expect("listing handles");
    let handles: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("reading handles")
        .filter_map(|h| h.ok())
        .step_by(STRIDE)
        .collect();
    drop(stmt);
    drop(conn);
    assert!(
        handles.len() > 20,
        "sample too small to mean anything: {}",
        handles.len()
    );

    // Every read command that takes a handle. `affects` and `runs` are the expensive ones
    // and are included deliberately: they are where the latency regressions happened.
    let commands: &[&[&str]] = &[
        &["refs"],
        &["usage"],
        &["expand"],
        &["graph", "--aspect", "callers"],
        &["graph", "--aspect", "calls"],
        &["graph", "--aspect", "tests"],
        &["reaches"],
        &["reaches", "--outgoing"],
        &["runs"],
        &["affects"],
        &["weaklinks"],
        &["links"],
    ];

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for handle in &handles {
        for cmd in commands {
            let started = Instant::now();
            let out = Command::new(&bin)
                .arg("--db")
                .arg(&db)
                .arg(cmd[0])
                .arg(handle)
                .args(&cmd[1..])
                .output()
                .unwrap_or_else(|e| panic!("running {} {handle}: {e}", cmd[0]));
            let elapsed = started.elapsed();
            checked += 1;

            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let label = format!("{} {handle} {}", cmd[0], cmd[1..].join(" "));
            let mut say = |problem: String| {
                failures.push(format!("  {label}: {problem}"));
            };

            match out.status.code() {
                Some(0..=3) => {}
                other => say(format!("exit {other:?}, which is not in the contract")),
            }
            if stderr.contains("panicked") {
                say(format!("panicked: {}", stderr.lines().next().unwrap_or("")));
            }
            if elapsed.as_secs() > CEILING_SECS {
                say(format!("took {:.1}s", elapsed.as_secs_f64()));
            }

            // A successful answer must carry the envelope; an error need not.
            if out.status.code() == Some(0) {
                // Two forms: `label: one thing` and `label (N):` for several. Checking
                // only the first reported fifty false violations, which is the sweep
                // testing my reading of the format rather than the format.
                for section in ["suppressed", "unknown", "stale"] {
                    let present = stdout.contains(&format!("{section}:"))
                        || stdout.contains(&format!("{section} ("));
                    if !present {
                        say(format!("no {section} section - the answer looks complete"));
                    }
                }
                // The shape three separate bugs took today: cutting the list and
                // simultaneously claiming nothing was cut.
                let cut = stdout.contains("beyond --limit")
                    || stdout.contains("beyond the")
                    || stdout.contains("more references");
                if cut && stdout.contains("suppressed: none") {
                    say("reports a cut and `suppressed: none` at once".to_string());
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {checked} command runs across {} symbols broke the contract:\n{}",
        failures.len(),
        handles.len(),
        failures.join("\n")
    );
    eprintln!("swept {checked} command runs across {} symbols", handles.len());
}
