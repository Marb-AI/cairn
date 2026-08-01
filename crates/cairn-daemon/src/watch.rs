//! Filesystem watching and the dirty set.
//!
//! The dirty set is defined against the *index*, not against the last event: a file is
//! dirty when its current content differs from what was indexed, so editing a file and
//! undoing the edit leaves nothing dirty. That matters because the alternative - "any
//! file that received an event" - would mark half the tree dirty after a git checkout
//! that changed nothing of substance.

use anyhow::Result;
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::schedule::{Decision, Scheduler};
use crate::DirtySet;

/// Directories never worth watching. Watching them is not merely wasted work: a build
/// or a virtualenv generates thousands of events and would drown real edits.
const IGNORED_DIRS: &[&str] = &[
    ".git", "node_modules", "__pycache__", ".venv", "venv", "target", "dist", ".next",
    ".mypy_cache", ".pytest_cache", ".ruff_cache", "vendor", ".cairn",
];

/// Events arrive in bursts - one editor save can produce several, and a git operation
/// produces thousands. Coalescing them keeps rehashing proportional to real changes.
const DEBOUNCE: Duration = Duration::from_millis(120);

pub struct DirtyTracker {
    inner: Arc<Mutex<State>>,
}

struct State {
    repo: PathBuf,
    /// Path -> content hash recorded at index time.
    indexed: HashMap<String, [u8; 16]>,
    dirty: DirtySet,
    scheduler: Scheduler,
}

impl DirtyTracker {
    /// `indexed` is the state to compare against: path -> content hash from the store.
    pub fn new(repo: &Path, indexed: HashMap<String, [u8; 16]>) -> DirtyTracker {
        DirtyTracker {
            inner: Arc::new(Mutex::new(State {
                repo: repo.to_path_buf(),
                indexed,
                dirty: DirtySet { complete: false, ..Default::default() },
                scheduler: Scheduler::new(),
            })),
        }
    }

    pub fn snapshot(&self) -> DirtySet {
        let mut st = self.inner.lock().unwrap();
        let now = Instant::now();
        // The decision is recomputed on read rather than on a timer: it is pure and
        // cheap, and this way an idle daemon does not need a tick just to notice that
        // time has passed.
        let due = match st.scheduler.decide(now) {
            Decision::Due(t) => Some(t.reason().to_string()),
            Decision::WaitingForQuiet(t) => {
                Some(format!("{} - waiting for the tree to settle", t.reason()))
            }
            Decision::Cooling(t) => Some(format!("{} - rate limited", t.reason())),
            Decision::Idle => None,
        };
        st.dirty.reindex_due = due;
        st.dirty.clone()
    }

    /// `.git/HEAD` moving means a commit, checkout, merge or rebase: the cleanest
    /// moment to reindex, because the tree is in a state someone chose.
    fn note_head_moved(&self) {
        self.inner
            .lock()
            .unwrap()
            .scheduler
            .on_head_moved(Instant::now());
    }

    pub fn tracked(&self) -> usize {
        self.inner.lock().unwrap().indexed.len()
    }

    /// Full comparison of the working tree against the index. Run once at start so the
    /// set is correct before any event arrives, and marked `complete` when done.
    pub fn initial_scan(&self) {
        let (repo, indexed) = {
            let st = self.inner.lock().unwrap();
            (st.repo.clone(), st.indexed.clone())
        };
        let mut modified = Vec::new();
        let mut removed = Vec::new();
        for (rel, then) in &indexed {
            match std::fs::read(repo.join(rel)) {
                Ok(bytes) => {
                    if hash16(&bytes) != *then {
                        modified.push(rel.clone());
                    }
                }
                Err(_) => removed.push(rel.clone()),
            }
        }
        // And the other direction: files on disk the index has never seen. Without this
        // the scan only ever compared the index against itself, so anything added since
        // the last build stayed invisible until something happened to touch it again —
        // `stale:` would say nothing while a whole new module sat there unindexed.
        let mut created = Vec::new();
        walk_new(&repo, &repo, &indexed, &mut created);

        modified.sort();
        removed.sort();
        created.sort();
        let mut st = self.inner.lock().unwrap();
        st.dirty.modified = modified;
        st.dirty.removed = removed;
        st.dirty.created = created;
        st.dirty.generation += 1;
        st.dirty.complete = true;
    }

    /// Re-check one path and update the set. Returns true when the set changed.
    fn recheck(&self, rel: &str) -> bool {
        let mut st = self.inner.lock().unwrap();
        let full = st.repo.join(rel);
        let known = st.indexed.get(rel).copied();
        let now = std::fs::read(&full).ok().map(|b| hash16(&b));

        let before = (
            st.dirty.modified.len(),
            st.dirty.created.len(),
            st.dirty.removed.len(),
        );
        st.dirty.modified.retain(|p| p != rel);
        st.dirty.created.retain(|p| p != rel);
        st.dirty.removed.retain(|p| p != rel);

        match (known, now) {
            // Known file, unchanged content: clean. This is the case that makes an
            // edit-then-undo disappear from the set instead of lingering.
            (Some(then), Some(now)) if then == now => {}
            (Some(_), Some(_)) => st.dirty.modified.push(rel.to_string()),
            (Some(_), None) => st.dirty.removed.push(rel.to_string()),
            (None, Some(_)) => st.dirty.created.push(rel.to_string()),
            (None, None) => {}
        }
        let after = (
            st.dirty.modified.len(),
            st.dirty.created.len(),
            st.dirty.removed.len(),
        );
        let debt = st.dirty.len();
        st.scheduler.on_change(Instant::now(), debt);
        if before != after {
            st.dirty.generation += 1;
            true
        } else {
            false
        }
    }

    /// Watch until the returned handle is dropped. Blocks, so callers give it a thread.
    pub fn watch_forever(&self, stop: mpsc::Receiver<()>) -> Result<()> {
        let repo = self.inner.lock().unwrap().repo.clone();
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(tx)?;
        watcher.watch(&repo, RecursiveMode::Recursive)?;
        // Watched explicitly: the recursive watch above skips nothing, but `.git`
        // events are dropped by the ignore filter, and HEAD is the one we want.
        let _ = watcher.watch(&repo.join(".git"), RecursiveMode::Recursive);

        let mut pending: Vec<PathBuf> = Vec::new();
        let mut last_event = Instant::now();
        loop {
            if stop.try_recv().is_ok() {
                return Ok(());
            }
            match rx.recv_timeout(DEBOUNCE) {
                Ok(Ok(ev)) => {
                    if !matches!(
                        ev.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                    ) {
                        continue;
                    }
                    pending.extend(ev.paths);
                    last_event = Instant::now();
                }
                Ok(Err(_)) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }

            if !pending.is_empty() && last_event.elapsed() >= DEBOUNCE {
                let batch = std::mem::take(&mut pending);
                let mut seen: Vec<String> = Vec::new();
                for p in batch {
                    let Some(rel) = relativise(&repo, &p) else { continue };
                    // `.git` is ignored for indexing, but HEAD moving is the single
                    // most useful reindex signal there is, so it is read first.
                    if rel == ".git/HEAD" || rel.starts_with(".git/refs/heads/") {
                        self.note_head_moved();
                        continue;
                    }
                    if is_ignored(&rel) || seen.contains(&rel) {
                        continue;
                    }
                    seen.push(rel.clone());
                    self.recheck(&rel);
                }
            }
        }
    }
}

fn hash16(bytes: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&blake3::hash(bytes).as_bytes()[..16]);
    out
}

fn relativise(root: &Path, p: &Path) -> Option<String> {
    p.strip_prefix(root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty())
}

/// Files present on disk that the index does not know about.
///
/// Bounded by the same ignore rules the watcher uses, so a `node_modules` does not turn a
/// startup scan into a minute of walking.
fn walk_new(
    root: &Path,
    dir: &Path,
    indexed: &HashMap<String, [u8; 16]>,
    out: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().to_string();
        if is_ignored(&rel) {
            continue;
        }
        if path.is_dir() {
            walk_new(root, &path, indexed, out);
        } else if !indexed.contains_key(&rel) {
            out.push(rel);
        }
    }
}

pub fn is_ignored(rel: &str) -> bool {
    rel.split('/').any(|seg| IGNORED_DIRS.contains(&seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_build_and_vcs_directories() {
        assert!(is_ignored(".git/objects/ab"));
        assert!(is_ignored("srcpy/__pycache__/x.pyc"));
        assert!(is_ignored("web/node_modules/pkg/i.js"));
        assert!(is_ignored(".cairn/index.sqlite"));
        assert!(!is_ignored("srcpy/domains/orders/x.py"));
        // A path merely containing the word is not a match.
        assert!(!is_ignored("srcpy/targeting/x.py"));
    }

    #[test]
    fn edit_then_undo_leaves_nothing_dirty() {
        let dir = std::env::temp_dir().join(format!("cairn-watch-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("a.py");
        std::fs::write(&file, "original\n").unwrap();

        let indexed = HashMap::from([("a.py".to_string(), hash16(b"original\n"))]);
        let t = DirtyTracker::new(&dir, indexed);
        t.initial_scan();
        assert!(t.snapshot().is_empty(), "matching content is not dirty");

        std::fs::write(&file, "changed\n").unwrap();
        t.recheck("a.py");
        assert_eq!(t.snapshot().modified, vec!["a.py".to_string()]);

        std::fs::write(&file, "original\n").unwrap();
        t.recheck("a.py");
        assert!(
            t.snapshot().is_empty(),
            "reverting the content must clear the file, not leave it flagged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracks_creation_and_removal_separately() {
        let dir = std::env::temp_dir().join(format!("cairn-watch2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("known.py"), "x\n").unwrap();
        let indexed = HashMap::from([("known.py".to_string(), hash16(b"x\n"))]);
        let t = DirtyTracker::new(&dir, indexed);
        t.initial_scan();

        std::fs::write(dir.join("new.py"), "y\n").unwrap();
        t.recheck("new.py");
        assert_eq!(t.snapshot().created, vec!["new.py".to_string()]);

        std::fs::remove_file(dir.join("known.py")).unwrap();
        t.recheck("known.py");
        assert_eq!(t.snapshot().removed, vec!["known.py".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generation_only_moves_on_real_change() {
        let dir = std::env::temp_dir().join(format!("cairn-watch3-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.py"), "x\n").unwrap();
        let t = DirtyTracker::new(&dir, HashMap::from([("a.py".into(), hash16(b"x\n"))]));
        t.initial_scan();
        let g = t.snapshot().generation;
        t.recheck("a.py");
        assert_eq!(t.snapshot().generation, g, "no content change, no generation bump");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
