//! Watching a real directory, with real writes.
//!
//! `is_ignored` and the path rules already had unit tests. What did not was the thing the
//! daemon is for: notice that a file on disk no longer matches what the index recorded, and
//! say so — without inventing changes for files nobody touched, and without deciding the
//! whole index is suspect because one file moved.
//!
//! The failure that matters here is the quiet one. A watcher that misses a change makes
//! every later answer confidently stale, and nothing in the output looks wrong; the
//! `stale:` line would say "none" while the file has been rewritten twice.

use cairn_daemon::watch::DirtyTracker;
use std::collections::HashMap;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("cairn-watch").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn digest(bytes: &[u8]) -> [u8; 16] {
    let full = blake3::hash(bytes);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}

#[test]
fn an_unchanged_tree_is_clean_and_a_rewritten_file_is_not() {
    let dir = scratch("modified");
    let file = dir.join("a.py");
    let original = b"def alpha():\n    pass\n";
    std::fs::write(&file, original).expect("writing");

    let mut indexed = HashMap::new();
    indexed.insert("a.py".to_string(), digest(original));

    let tracker = DirtyTracker::new(&dir, indexed.clone());
    tracker.initial_scan();
    assert!(
        tracker.snapshot().is_empty(),
        "a tree that matches the index must be reported clean, or every answer carries a \
         stale marker nobody can act on"
    );

    // Same length, different content: a watcher comparing sizes or timestamps alone would
    // miss this, and the miss is invisible.
    std::fs::write(&file, b"def alpha():\n    fail\n").expect("rewriting");
    let tracker = DirtyTracker::new(&dir, indexed);
    tracker.initial_scan();
    let dirty = tracker.snapshot();
    assert!(
        dirty.modified.iter().any(|p| p == "a.py"),
        "a rewritten file was not seen as modified: {dirty:?}"
    );
    assert!(dirty.affects("a.py"));
    assert!(!dirty.affects("something-else.py"));
}

#[test]
fn a_file_the_index_knows_and_disk_does_not_is_removed_not_modified() {
    // The two are different facts and lead to different answers: a removed file makes
    // every symbol in it gone, a modified one only makes them uncertain.
    let dir = scratch("removed");
    let mut indexed = HashMap::new();
    indexed.insert("vanished.py".to_string(), digest(b"whatever"));

    let tracker = DirtyTracker::new(&dir, indexed);
    tracker.initial_scan();
    let dirty = tracker.snapshot();
    assert!(
        dirty.removed.iter().any(|p| p == "vanished.py"),
        "a file present in the index and absent from disk should be removed: {dirty:?}"
    );
    assert!(
        dirty.modified.is_empty(),
        "and not also modified: {dirty:?}"
    );
}

#[test]
fn a_file_disk_knows_and_the_index_does_not_is_created() {
    let dir = scratch("created");
    std::fs::write(dir.join("fresh.py"), b"x = 1\n").expect("writing");

    let tracker = DirtyTracker::new(&dir, HashMap::new());
    tracker.initial_scan();
    let dirty = tracker.snapshot();
    assert!(
        dirty.created.iter().any(|p| p == "fresh.py"),
        "a file the index has never seen should be created: {dirty:?}"
    );
    // Created files deliberately do not mark earlier answers stale — no earlier answer
    // could have mentioned a file that did not exist.
    assert!(!dirty.affects("fresh.py"));
}

#[test]
fn the_scan_ignores_what_it_should_and_says_how_much_it_tracks() {
    let dir = scratch("ignored");
    std::fs::create_dir_all(dir.join("node_modules/pkg")).expect("dirs");
    std::fs::create_dir_all(dir.join(".git")).expect("dirs");
    std::fs::write(dir.join("node_modules/pkg/index.js"), b"//\n").expect("writing");
    std::fs::write(dir.join(".git/HEAD"), b"ref: x\n").expect("writing");
    std::fs::write(dir.join("real.py"), b"y = 2\n").expect("writing");

    let tracker = DirtyTracker::new(&dir, HashMap::new());
    tracker.initial_scan();
    let dirty = tracker.snapshot();
    assert!(
        dirty.created.iter().any(|p| p == "real.py"),
        "the one real file was missed: {dirty:?}"
    );
    for noise in ["node_modules/pkg/index.js", ".git/HEAD"] {
        assert!(
            !dirty.created.iter().any(|p| p == noise),
            "{noise} should never have been walked: {dirty:?}"
        );
    }
    // `tracked` counts what the *index* recorded, not what is on disk, and this test
    // starts from an empty index. Asserted so the meaning stays pinned: it is not a
    // count of files being watched.
    assert_eq!(tracker.tracked(), 0);
}
