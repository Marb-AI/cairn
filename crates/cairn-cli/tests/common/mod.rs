//! Shared by the integration tests that need a corpus to run against.
//!
//! In a subdirectory so cargo treats it as a module rather than a test binary of its own.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the fixture index from the SCIP files committed beside it.
///
/// The index itself is not committed — only its inputs. A stored SQLite file would have to
/// be rebuilt on every schema change and would rot silently between them, whereas
/// rebuilding here also exercises the ingest path the caller is about to query.
///
/// `label` names the calling test binary. Cargo runs them concurrently, so two callers
/// sharing one database would race each other rebuilding it.
///
/// Returns `None` when the SCIP files are absent, which is the caller's cue to skip.
pub fn build_fixture_index(root: &Path, bin: &Path, label: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("cairn-fixture-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    build_fixture_index_in(&dir, root, bin)
}

/// The same index, in a directory the caller chose and prepared.
///
/// Separate because a connector's manifest lives *beside* the index, so a test about
/// connectors has to write one before the index is built — and it must not write it into
/// the fixture corpus, where it would change what every other test sees.
pub fn build_fixture_index_in(dir: &Path, root: &Path, bin: &Path) -> Option<PathBuf> {
    let fixtures = root.join("crates/cairn-cli/tests/fixtures");
    let (go, py) = (
        fixtures.join("index/go.scip"),
        fixtures.join("index/py.scip"),
    );
    if !go.exists() || !py.exists() {
        return None;
    }

    std::fs::create_dir_all(dir).ok()?;
    let db = dir.join("index.sqlite");
    let out = Command::new(bin)
        .arg("--db")
        .arg(&db)
        .arg("index")
        .arg(&go)
        .arg(&py)
        .arg("--repo")
        .arg(fixtures.join("corpus"))
        // Without this every test run writes the agent guide into the fixture, which is a
        // source directory. It did, and the file was committed before anyone noticed.
        .arg("--without-skill")
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "indexing the fixture corpus failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(db)
}
