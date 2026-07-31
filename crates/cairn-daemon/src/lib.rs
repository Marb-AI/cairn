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
    /// Stop the daemon.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Dirty(DirtySet),
    Status(DaemonStatus),
    Ok,
    /// The daemon understood the request and cannot serve it. Distinct from a
    /// transport failure, which the client reports as "no daemon".
    Error { message: String },
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
        self.modified.iter().any(|p| p == path)
            || self.removed.iter().any(|p| p == path)
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

/// Socket path for a workspace.
///
/// Derived from the index location rather than the repo, so two checkouts sharing a
/// repo path but not an index do not collide, and so the socket sits beside the state
/// it belongs to.
pub fn socket_path(index_path: &Path) -> PathBuf {
    index_path.with_file_name(
        index_path
            .file_stem()
            .map(|s| format!("{}.sock", s.to_string_lossy()))
            .unwrap_or_else(|| "cairn.sock".to_string()),
    )
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
