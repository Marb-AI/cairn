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
    let fixtures = root.join("crates/cairn-cli/tests/fixtures");
    let (go, py) = (
        fixtures.join("index/go.scip"),
        fixtures.join("index/py.scip"),
    );
    if !go.exists() || !py.exists() {
        return None;
    }

    let dir = std::env::temp_dir().join(format!("cairn-fixture-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;

    let db = dir.join("index.sqlite");
    let out = Command::new(bin)
        .arg("--db")
        .arg(&db)
        .arg("index")
        .arg(&go)
        .arg(&py)
        .arg("--repo")
        .arg(fixtures.join("corpus"))
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "indexing the fixture corpus failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(db)
}
