//! The live-state daemon.
//!
//! Deliberately **not** a query proxy. SQLite in WAL mode serves concurrent readers
//! fine, and CLI startup is already ~1 ms, so routing queries through a socket would
//! add latency and buy nothing. What cannot live in a one-shot process is *live* state:
//! a file watcher that has been running since before the query was asked, and later the
//! warm language servers that make the dirty path fast (architecture 2, 4.2).
//!
//! So the daemon owns the watcher and answers exactly one question — what has changed
//! since the index was built — and the CLI folds that into every answer's `stale:`
//! section. When the LSP pool arrives it joins for the same reason, and the protocol
//! grows one more request rather than changing shape.
//!
//! Protocol: newline-delimited JSON over a unix socket. Chosen over a compact binary
//! encoding because the traffic is one small request per CLI invocation, and being able
//! to `socat` the socket while debugging is worth more than the bytes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod client;
pub mod ipc;
pub mod lsp;
pub mod schedule;
pub mod server;
pub mod watch;

pub use client::Client;
pub use schedule::{Decision, Scheduler, Trigger};
pub use server::Daemon;

/// One request. An enum rather than a free-form command so that an old client talking
/// to a new daemon fails loudly instead of being misunderstood.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Files that differ from what was indexed.
    Dirty,
    /// Liveness and what is being watched.
    Status,
    /// Current symbols in a file, straight from the language server. This is the
    /// dirty overlay: the index cannot answer about a file that changed, a warm
    /// server can.
    FileSymbols { path: String },
    /// Stop the daemon.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Dirty(DirtySet),
    Status(DaemonStatus),
    FileSymbols {
        symbols: Vec<lsp::LiveSymbol>,
    },
    Ok,
    /// The daemon understood the request and cannot serve it. Distinct from a
    /// transport failure, which the client reports as "no daemon".
    Error {
        message: String,
    },
}

/// What the watcher has seen since the index was built.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirtySet {
    pub modified: Vec<String>,
    pub created: Vec<String>,
    pub removed: Vec<String>,
    /// Bumped on every change, so a caller can tell "nothing changed" from
    /// "I asked again and got the same answer".
    pub generation: u64,
    /// False while the initial scan is still running: the set is incomplete and must
    /// not be presented as authoritative.
    pub complete: bool,
    /// Set when the scheduler has decided a reindex is warranted. Reported rather than
    /// acted on: deciding is free, running spawns heavy external indexers.
    pub reindex_due: Option<String>,
}

impl DirtySet {
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.created.is_empty() && self.removed.is_empty()
    }

    pub fn len(&self) -> usize {
        self.modified.len() + self.created.len() + self.removed.len()
    }

    /// Is this specific file affected? Used to mark individual answers stale rather
    /// than declaring the whole index suspect.
    pub fn affects(&self, path: &str) -> bool {
        self.modified.iter().any(|p| p == path) || self.removed.iter().any(|p| p == path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub repo: String,
    pub watching: bool,
    pub files_tracked: usize,
    pub generation: u64,
    pub uptime_secs: u64,
    pub reindex_due: Option<String>,
}

/// How long a unix socket path may be.
///
/// `sun_path` is 108 bytes on Linux and 104 on macOS, including the terminator, and bind
/// fails outright above it. 100 leaves room on both without needing to know which one this
/// is. Not a style limit — a checkout nested deep enough silently had no daemon at all,
/// because binding failed before anything else could report it.
const MAX_SOCKET_PATH: usize = 100;

/// Socket path for a workspace.
///
/// Derived from the index location rather than the repo, so two checkouts sharing a repo
/// path but not an index do not collide, and so the socket sits beside the state it
/// belongs to — which is where you would look for it, and where a stale one is obvious.
///
/// Unless it does not fit. A repository under a long enough path produces a socket path
/// the kernel will not accept, and there is nowhere to put it near the index that would
/// help. So it moves to the runtime directory under a name derived from the index path:
/// still one socket per index, still collision-free, just somewhere short.
pub fn socket_path(index_path: &Path) -> PathBuf {
    let beside = index_path.with_file_name(
        index_path
            .file_stem()
            .map(|s| format!("{}.sock", s.to_string_lossy()))
            .unwrap_or_else(|| "cairn.sock".to_string()),
    );
    if beside.as_os_str().len() <= MAX_SOCKET_PATH {
        return beside;
    }

    // Absolute first: two different relative paths can name one index, and they must not
    // end up with two sockets for it.
    let key = std::path::absolute(index_path).unwrap_or_else(|_| index_path.to_path_buf());
    let digest = blake3::hash(key.to_string_lossy().as_bytes());
    runtime_dir().join(format!("cairn-{}.sock", &digest.to_hex()[..16]))
}

/// Somewhere short-lived, writable, and short enough to hold a socket.
fn runtime_dir() -> PathBuf {
    // XDG_RUNTIME_DIR is the right home for a socket and is already per-user, but it is
    // not everywhere; the temp directory is.
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// Remove a socket file left behind by a daemon that did not exit cleanly.
///
/// Only safe once a connection attempt has already failed: a live daemon owns its
/// socket, and unlinking it underneath would strand every future client.
pub fn clear_stale_socket(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("removing stale socket {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_sits_beside_the_index_when_it_fits() {
        let s = socket_path(Path::new("/w/repo/.cairn/index.sqlite"));
        assert_eq!(s, Path::new("/w/repo/.cairn/index.sock"));
    }

    #[test]
    fn a_deep_checkout_gets_a_short_socket_instead_of_none() {
        // The failure this exists to prevent: bind refuses anything over ~108 bytes, so a
        // repository nested this deep had no daemon and no explanation.
        let deep = format!("/{}/repo/.cairn/index.sqlite", "nested".repeat(30));
        let s = socket_path(Path::new(&deep));
        assert!(
            s.as_os_str().len() <= MAX_SOCKET_PATH,
            "still too long: {} bytes",
            s.as_os_str().len()
        );
        assert!(s.to_string_lossy().ends_with(".sock"));
    }

    #[test]
    fn two_deep_checkouts_do_not_share_a_socket() {
        let a = format!("/{}/one/.cairn/index.sqlite", "nested".repeat(30));
        let b = format!("/{}/two/.cairn/index.sqlite", "nested".repeat(30));
        assert_ne!(socket_path(Path::new(&a)), socket_path(Path::new(&b)));
    }

    #[test]
    fn the_same_index_always_gets_the_same_socket() {
        let deep = format!("/{}/repo/.cairn/index.sqlite", "nested".repeat(30));
        assert_eq!(socket_path(Path::new(&deep)), socket_path(Path::new(&deep)));
    }
}
