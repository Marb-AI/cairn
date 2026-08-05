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

/// How many symbols to sweep, rather than how far apart they sit.
///
/// This used to be a fixed stride of 997. That is fine on a checkout with a hundred
/// thousand symbols and useless on the fixture corpus, which has a few hundred and would
/// yield a sample of nothing — so the sweep would pass by never running. A count works on
/// both, and the stride is derived from it.
const SAMPLE: usize = 40;
const CEILING_SECS: u64 = 10;

/// How many symbols this run sweeps. 40 keeps CI honest without making it slow; a hunt
/// wants hundreds, and editing a constant to get them is how a wide run becomes something
/// nobody does. `CAIRN_SWEEP_SAMPLE=400 cargo test --test sweep` is the hunt.
fn sample_size() -> usize {
    std::env::var("CAIRN_SWEEP_SAMPLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(SAMPLE)
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

mod common;
use common::build_fixture_index;

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
    let bin = binary();
    assert!(bin.exists(), "cairn not built at {}", bin.display());

    // A real checkout when there is one: it is the corpus worth sweeping and it is what a
    // workstation has. Otherwise the fixture in this tree, which is what CI has. The sweep
    // must not skip there — a contract test that only runs on one machine is a contract
    // test that does not run.
    let real = indexed_repo(&root).join(".cairn/index.sqlite");
    let (db, corpus) = if real.exists() {
        (real, "the indexed repository")
    } else {
        match build_fixture_index(&root, &bin, "sweep") {
            Some(db) => (db, "the fixture corpus"),
            None => {
                eprintln!(
                    "SKIP: no index at {} and no fixture SCIP to build one from",
                    real.display()
                );
                return;
            }
        }
    };

    let conn = rusqlite::Connection::open(&db).expect("opening the index");
    let mut stmt = conn
        .prepare(
            "SELECT h.handle FROM handles h JOIN symbols s ON s.id = h.symbol_id
              ORDER BY h.symbol_id",
        )
        .expect("listing handles");
    let all: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("reading handles")
        .filter_map(|h| h.ok())
        .collect();
    drop(stmt);
    drop(conn);

    // Ordered by symbol id and stepped, so the sample is the same on every run against the
    // same index: a failure is reproducible and a fix can be verified. Forced odd because
    // symbols arrive grouped by file, and an even stride can land on the same position
    // within each of them for a whole run.
    let stride = ((all.len() / sample_size()).max(1) | 1).max(1);
    let handles: Vec<String> = all.iter().cloned().step_by(stride).collect();
    assert!(
        handles.len() >= 20,
        "sample too small to mean anything: {} handles out of {} symbols",
        handles.len(),
        all.len()
    );

    // Every read command that takes a handle, as (before the handle, after it). `affects`
    // and `runs` are the expensive ones and are included deliberately: they are where the
    // latency regressions happened.
    //
    // The pair replaces a flat list that always put the handle straight after the command
    // word. That shape could not express `for understand <h>`, so the assembled answers —
    // the ones the guide sends agents to first — were the only commands in the binary the
    // contract sweep never ran.
    let commands: &[(&[&str], &[&str])] = &[
        (&["refs"], &[]),
        (&["usage"], &[]),
        (&["expand"], &[]),
        (&["graph"], &["--aspect", "callers"]),
        (&["graph"], &["--aspect", "calls"]),
        (&["graph"], &["--aspect", "tests"]),
        (&["reaches"], &[]),
        (&["reaches"], &["--outgoing"]),
        (&["runs"], &[]),
        (&["affects"], &[]),
        (&["weaklinks"], &[]),
        (&["links"], &[]),
        (&["for", "change"], &[]),
        (&["for", "understand"], &[]),
    ];

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for handle in &handles {
        for (head, tail) in commands {
            let started = Instant::now();
            let out = Command::new(&bin)
                .arg("--db")
                .arg(&db)
                .args(*head)
                .arg(handle)
                .args(*tail)
                .output()
                .unwrap_or_else(|e| panic!("running {} {handle}: {e}", head.join(" ")));
            let elapsed = started.elapsed();
            checked += 1;

            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let label = format!("{} {handle} {}", head.join(" "), tail.join(" "));
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
                // Every phrasing the binary uses to say it left something out. This list
                // drifting behind the output is not hypothetical: `symbol` announced
                // "--limit reached, there may be more" next to `suppressed: none` for
                // months, and the check written for exactly that class could not see it
                // because it only knew three other spellings.
                let cut = stdout.contains("beyond --limit")
                    || stdout.contains("beyond the")
                    || stdout.contains("more references")
                    || stdout.contains("--limit reached")
                    || stdout.contains("there may be more");
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
    eprintln!(
        "swept {checked} command runs across {} of {} symbols in {corpus}",
        handles.len(),
        all.len()
    );
}
