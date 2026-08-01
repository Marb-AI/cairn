//! Building the same index twice must produce the same index.
//!
//! Two properties, and the second is the one that would hurt quietly.
//!
//! Counts drifting between identical builds would mean the derivation depends on iteration
//! order somewhere, and every measured number in `eval/RESULTS.md` would be built on sand.
//!
//! Handles are worse. They are the tool's contract with an agent: `cairn symbol` hands out
//! `[fba]`, the agent writes it down, and uses it in the next command — or an hour later,
//! after the code has changed and the index has been rebuilt. If a rebuild reshuffled them,
//! `[fba]` would silently name a different symbol and every answer after that would be
//! confidently about the wrong thing. Nothing in the output would look wrong.
//!
//! Runs against the smallest SCIP fixture available so the whole thing costs a second or
//! two; the property is about determinism, not scale.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn binary() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cairn")
}

fn run(bin: &Path, db: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("running cairn");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn rebuilding_produces_the_same_index_and_the_same_handles() {
    let root = workspace_root();
    let scip = root.join("spike/out/tg.scip");
    if !scip.exists() {
        eprintln!("SKIP: no SCIP fixture at {}", scip.display());
        return;
    }
    let bin = binary();
    assert!(bin.exists(), "cairn not built at {}", bin.display());

    let dir = std::env::temp_dir().join("cairn-determinism");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let db = dir.join("index.sqlite");

    let scip = scip.to_string_lossy().to_string();
    let first_build = run(&bin, &db, &["index", &scip]);
    let first_status = run(&bin, &db, &["status"]);
    let first_symbols = run(&bin, &db, &["symbol", "e", "--limit", "40"]);

    let second_build = run(&bin, &db, &["index", &scip]);
    let second_status = run(&bin, &db, &["status"]);
    let second_symbols = run(&bin, &db, &["symbol", "e", "--limit", "40"]);

    assert!(
        first_build.contains("symbols"),
        "the first build produced nothing usable:\n{first_build}"
    );

    // Timings differ between runs, so compare the lines that are supposed to be facts.
    let facts = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| {
                l.starts_with("files")
                    || l.starts_with("symbols")
                    || l.starts_with("occurrence")
                    || l.starts_with("generated")
                    || l.starts_with("services")
            })
            .map(|l| l.to_string())
            .collect()
    };
    assert_eq!(
        facts(&first_status),
        facts(&second_status),
        "an identical rebuild changed the index's own counts"
    );
    assert!(
        !facts(&first_status).is_empty(),
        "status reported no counts at all, so this asserted nothing"
    );

    // The handles, in the order the same query returns them.
    let handles = |s: &str| -> Vec<String> {
        s.lines()
            .filter_map(|l| l.trim().strip_prefix('['))
            .filter_map(|l| l.split(']').next())
            .map(|h| h.to_string())
            .collect()
    };
    let before = handles(&first_symbols);
    let after = handles(&second_symbols);
    assert!(!before.is_empty(), "no handles to compare");
    assert_eq!(
        before, after,
        "a rebuild reshuffled handles - an agent holding one from before would now be \
         asking about a different symbol, and nothing in the answer would look wrong"
    );
    assert!(second_build.contains("symbols"), "the second build failed");
}

#[test]
fn two_indexes_merge_into_one_store_rather_than_replacing_each_other() {
    // Checked because a plausible-looking probe suggested the opposite: reading the
    // per-file progress line instead of the store total made it look as though the second
    // SCIP file had been discarded. It had not. The property is worth an assertion so
    // nobody has to re-derive it from output that is easy to misread.
    let root = workspace_root();
    let (a, b) = (
        root.join("spike/out/tg.scip"),
        root.join("spike/out/t-go-target.scip"),
    );
    if !a.exists() || !b.exists() {
        eprintln!("SKIP: need two SCIP fixtures");
        return;
    }
    let bin = binary();
    let dir = std::env::temp_dir().join("cairn-merge");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let count = |db: &Path, args: &[&str]| -> i64 {
        let out = run(&bin, db, args);
        out.lines()
            .find(|l| l.starts_with("symbols"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no symbol count in:\n{out}"))
    };

    let (a, b) = (a.to_string_lossy().to_string(), b.to_string_lossy().to_string());
    let only_a = dir.join("a.sqlite");
    run(&bin, &only_a, &["index", &a]);
    let only_b = dir.join("b.sqlite");
    run(&bin, &only_b, &["index", &b]);
    let both = dir.join("both.sqlite");
    run(&bin, &both, &["index", &a, &b]);

    let (na, nb, nboth) = (
        count(&only_a, &["status"]),
        count(&only_b, &["status"]),
        count(&both, &["status"]),
    );
    assert!(na > 0 && nb > 0, "a fixture produced nothing: {na}, {nb}");
    assert!(
        nboth > na && nboth > nb,
        "indexing both files gave {nboth} symbols, no more than either alone ({na}, {nb}) \
         - the second index replaced the first instead of merging"
    );
    assert!(
        nboth <= na + nb,
        "indexing both gave {nboth}, more than the sum of {na} and {nb} - symbols present \
         in both files were counted twice"
    );
}
