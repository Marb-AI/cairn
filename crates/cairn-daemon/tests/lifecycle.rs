//! The daemon's edges: where it is, what happens when it is not there, and what it says
//! is stale.
//!
//! The scheduling and watching logic already had tests. The socket — how it is found,
//! what a client does when nothing is listening, what a crashed daemon leaves behind —
//! had none, and it is the only part of cairn with a long-lived process and shared state.
//! It also now meets the rebuild lock and the atomic index swap, so the failure modes here
//! are the ones that would look like the tool being flaky rather than broken.

use cairn_daemon::{clear_stale_socket, socket_path, Client, DirtySet};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("cairn-daemon-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn the_socket_sits_beside_the_index_it_belongs_to() {
    // Derived from the index, not the repository: two checkouts can share a source path
    // and not an index, and a socket in the wrong place connects a client to someone
    // else's daemon.
    assert_eq!(
        socket_path(Path::new("/w/.cairn/index.sqlite")),
        PathBuf::from("/w/.cairn/index.sock")
    );
    assert_eq!(
        socket_path(Path::new("/a/b/other.sqlite")),
        PathBuf::from("/a/b/other.sock")
    );
    // Two indexes never share a socket, which is the property that actually matters.
    assert_ne!(
        socket_path(Path::new("/one/.cairn/index.sqlite")),
        socket_path(Path::new("/two/.cairn/index.sqlite"))
    );
}

#[test]
fn connecting_to_a_daemon_that_is_not_there_is_none_not_an_error() {
    // The common case by far: no daemon runs, and every read path calls this. It has to
    // be a quiet `None` — a panic or an `Err` here would make the whole tool look broken
    // whenever nobody had started a daemon.
    let dir = scratch("absent");
    assert!(Client::connect(&dir.join("index.sock")).is_none());
    assert!(cairn_daemon::client::dirty_if_running(&dir.join("index.sock")).is_none());
}

#[test]
fn a_socket_file_with_nothing_listening_is_also_none() {
    // What a crashed daemon leaves: the file exists, so an existence check would say the
    // daemon is up, and connecting fails. Distinguishing the two is the whole point.
    let dir = scratch("orphan");
    let sock = dir.join("index.sock");
    std::fs::write(&sock, b"").expect("orphan socket");
    assert!(sock.exists());
    assert!(
        Client::connect(&sock).is_none(),
        "a file that is not a live socket must not read as a running daemon"
    );
}

#[test]
fn a_stale_socket_is_removed_and_a_missing_one_is_not_an_error() {
    let dir = scratch("clear");
    let sock = dir.join("index.sock");
    std::fs::write(&sock, b"").expect("stale socket");
    clear_stale_socket(&sock).expect("clearing a stale socket");
    assert!(!sock.exists());
    // Idempotent: the caller has just failed to connect and should not have to check.
    clear_stale_socket(&sock).expect("clearing nothing is fine");
}

#[test]
fn staleness_covers_changed_and_deleted_files_but_not_new_ones() {
    // A created file cannot invalidate an earlier answer, because no earlier answer could
    // have mentioned it. Pinned so the omission stays a decision rather than a bug someone
    // "fixes" into noisy staleness on every new file.
    let dirty = DirtySet {
        modified: vec!["a.py".to_string()],
        created: vec!["new.py".to_string()],
        removed: vec!["gone.py".to_string()],
        ..DirtySet::default()
    };
    // `complete` is false by default, which is the honest starting state: the initial
    // scan has not finished and the set must not be presented as authoritative.
    assert!(
        !dirty.complete,
        "a hand-built set should not claim to be complete"
    );
    assert!(dirty.affects("a.py"));
    assert!(dirty.affects("gone.py"));
    assert!(!dirty.affects("new.py"));
    assert!(!dirty.affects("untouched.py"));
    assert_eq!(dirty.len(), 3);
    assert!(!dirty.is_empty());
    assert!(DirtySet::default().is_empty());
}
