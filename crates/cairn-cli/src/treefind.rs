//! Content search over the working tree.
//!
//! Deliberately over the tree and not over a stored copy of it. Storing the text beside
//! the index was the obvious design and it is the wrong one twice over: a full search of
//! this repository's 2273 files takes 20 ms, so there is nothing to buy on speed, and a
//! stored copy is a second thing to keep in sync — which is the exact defect the
//! measurement punished hardest (scenario 9, where the index is behind the tree and the
//! arm paid 3× for it). The tree is the truth. Reading it cannot be stale.
//!
//! Substring, not regex, and case-insensitive. That is what `literal` already offers and
//! what every measured question needed (`MCP_SERVER_PORT`, `X-Api-Key`, `quota_headroom`).
//! Regex belongs here when a scenario asks for it and not before.

use cairn_daemon::watch::is_ignored;
use std::path::Path;

/// One matching line, before anything is attributed to it.
pub struct Hit {
    pub path: String,
    /// One-based, as every other line number this tool prints.
    pub line: usize,
    pub text: String,
}

/// Files above this are almost certainly not what anyone is searching for by hand, and
/// one of them can carry more matches than the whole rest of the tree. Skipped with a
/// count, never silently.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct Found {
    pub hits: Vec<Hit>,
    /// Files skipped for size, so a miss can say whether it was really a miss.
    pub skipped_large: usize,
    /// True when `limit` cut the list.
    pub truncated: bool,
    pub files_read: usize,
}

/// Case-insensitive substring search over every file under `root` the walker will read.
pub fn search(root: &Path, needle: &str, limit: usize) -> Found {
    let needle = needle.to_lowercase();
    let mut out = Found {
        hits: Vec::new(),
        skipped_large: 0,
        truncated: false,
        files_read: 0,
    };
    walk(root, root, &needle, limit, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, needle: &str, limit: usize, out: &mut Found) {
    if out.truncated {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Sorted, so two runs over one tree return the same rows in the same order. An answer
    // that reorders itself between runs cannot be diffed, and every other command here is
    // deterministic.
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();

    for path in entries {
        if out.truncated {
            return;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().to_string();
        if is_ignored(&rel) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, needle, limit, out);
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > MAX_FILE_BYTES {
                out.skipped_large += 1;
                continue;
            }
        }
        // Not valid UTF-8 means a binary, and a binary has no lines worth printing.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.files_read += 1;
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(needle) {
                if out.hits.len() >= limit {
                    out.truncated = true;
                    return;
                }
                out.hits.push(Hit {
                    path: rel.clone(),
                    line: i + 1,
                    text: line.trim_end().to_string(),
                });
            }
        }
    }
}
