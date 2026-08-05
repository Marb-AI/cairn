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
    ".git",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "target",
    "dist",
    ".next",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "vendor",
    ".cairn",
];

/// Events arrive in bursts - one editor save can produce several, and a git operation
/// produces thousands. Coalescing them keeps rehashing proportional to real changes.
const DEBOUNCE: Duration = Duration::from_millis(120);

pub struct DirtyTracker {
    inner: Arc<Mutex<State>>,
}

/// Re-reads path -> content hash from the store. Supplied by the caller because the
/// daemon deliberately does not depend on `cairn-store`: the watcher is a file-system
/// concern and pulling a database driver into it to answer one question would invert the
/// layering for the sake of one call.
pub type ReloadIndexed = Box<dyn Fn() -> Option<HashMap<String, [u8; 16]>> + Send>;

struct State {
    repo: PathBuf,
    /// Path -> content hash recorded at index time.
    indexed: HashMap<String, [u8; 16]>,
    dirty: DirtySet,
    scheduler: Scheduler,
    /// The index file and when it was last seen, so a rebuild underneath the daemon can
    /// be noticed. `.cairn` is in `IGNORED_DIRS`, so no file-system event ever arrives
    /// for it — this is the only signal there is.
    index_db: Option<PathBuf>,
    index_stamp: Option<std::time::SystemTime>,
    reload: Option<ReloadIndexed>,
    /// Directories the indexers were actually pointed at, e.g. `srcpy`, `srcgo`. Empty
    /// when the index recorded none, in which case the extension filter stands alone.
    roots: Vec<String>,
}

impl DirtyTracker {
    /// `indexed` is the state to compare against: path -> content hash from the store.
    pub fn new(repo: &Path, indexed: HashMap<String, [u8; 16]>) -> DirtyTracker {
        DirtyTracker {
            inner: Arc::new(Mutex::new(State {
                repo: repo.to_path_buf(),
                indexed,
                dirty: DirtySet {
                    complete: false,
                    ..Default::default()
                },
                scheduler: Scheduler::new(),
                index_db: None,
                index_stamp: None,
                reload: None,
                roots: Vec::new(),
            })),
        }
    }

    /// The directories the indexers were pointed at.
    ///
    /// Without them the `created` set reports every `.py` and `.go` outside those
    /// directories as new — 17 of them on the target repository, under `tools/` and
    /// `infra/`, none of which any indexer was ever going to read. That is the same
    /// false-loudness the extension filter was added for, one level in: `verify --repo`
    /// called the tree clean while `status` advertised seventeen files of debt.
    pub fn set_roots(&self, roots: Vec<String>) {
        self.inner.lock().unwrap().roots = roots;
    }

    /// Tell the tracker where the index lives and how to re-read it.
    ///
    /// Without this the snapshot taken at start-up is compared against forever. Measured:
    /// two files were edited, `cairn index` was run, and `status` went on reporting them
    /// as modified while `verify --repo` — which reads the store directly — reported a
    /// clean tree. The stress harness caught the disagreement; the daemon simply had no
    /// way to learn that the thing it compares against had been replaced.
    pub fn watch_index(&self, db: &Path, reload: ReloadIndexed) {
        let mut st = self.inner.lock().unwrap();
        st.index_stamp = stamp_of(db);
        st.index_db = Some(db.to_path_buf());
        st.reload = Some(reload);
    }

    /// Re-read the index snapshot if the index has been rebuilt, and drop from the dirty
    /// set anything the rebuild made current. Returns true when something changed.
    ///
    /// Only the files already believed dirty are re-checked, not the whole tree: a
    /// rebuild can only ever *clean* a file, since it records what is on disk. That keeps
    /// this cheap enough to run on a status request, which is where it has to run — a
    /// caller asking what is stale is exactly the moment the answer must account for a
    /// reindex.
    pub fn refresh_if_reindexed(&self) -> bool {
        let (db, seen) = {
            let st = self.inner.lock().unwrap();
            match &st.index_db {
                Some(db) => (db.clone(), st.index_stamp),
                None => return false,
            }
        };
        let now = stamp_of(&db);
        if now.is_none() || now == seen {
            return false;
        }
        let fresh = {
            let st = self.inner.lock().unwrap();
            match &st.reload {
                Some(f) => f(),
                None => None,
            }
        };
        let Some(fresh) = fresh else {
            // Record the stamp anyway: a store that cannot be read now will not read any
            // better on the next status request, and retrying it on every one turns a
            // broken index into a busy loop.
            self.inner.lock().unwrap().index_stamp = now;
            return false;
        };
        let stale: Vec<String> = {
            let mut st = self.inner.lock().unwrap();
            st.indexed = fresh;
            st.index_stamp = now;
            let d = &st.dirty;
            d.modified
                .iter()
                .chain(&d.created)
                .chain(&d.removed)
                .cloned()
                .collect()
        };
        let mut changed = false;
        for rel in stale {
            changed |= self.recheck(&rel);
        }
        changed
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
        let (repo, indexed, roots) = {
            let st = self.inner.lock().unwrap();
            (st.repo.clone(), st.indexed.clone(), st.roots.clone())
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
        walk_new(&repo, &repo, &indexed, &roots, &mut created);

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
            // Same filter as the initial scan, for the same reason: a live event for a
            // file the index could never have held is not news about the index.
            (None, Some(_)) if covered(rel, &st.roots) => st.dirty.created.push(rel.to_string()),
            (None, Some(_)) => {}
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
                    let Some(rel) = relativise(&repo, &p) else {
                        continue;
                    };
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
    roots: &[String],
    out: &mut Vec<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().to_string();
        if is_ignored(&rel) {
            continue;
        }
        if path.is_dir() {
            walk_new(root, &path, indexed, roots, out);
        } else if covered(&rel, roots) && !indexed.contains_key(&rel) {
            out.push(rel);
        }
    }
}

/// Extensions the indexers actually produce documents for.
///
/// **Update this when a language is added**, or the watcher will go quiet about whole new
/// modules in it.
const INDEXABLE_EXTENSIONS: &[&str] = &["py", "pyi", "go"];

/// Could the index have held this file at all?
///
/// Measured: on a clean tree the daemon reported `590 created` and advised a reindex,
/// because everything that is not Python or Go - documentation, compose files, protos,
/// SQL - is absent from the index by design and the walk read that absence as "new". A
/// staleness signal that is loud on a tree nobody has touched is one an agent learns to
/// ignore, which is worse than not having it: the one real edit that follows is buried in
/// it. `cairn verify --repo .` was exact throughout, which is what made the daemon's
/// number visibly wrong rather than merely unexplained.
pub fn is_indexable(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| INDEXABLE_EXTENSIONS.contains(&e))
}

/// Could the index have held this path at all: right extension, and inside a directory
/// an indexer was actually pointed at.
///
/// The extension test alone was not enough. `tools/eval/run_eval.py` is Python, and no
/// SCIP run has ever looked at it, so calling it "created" is the same false alarm as
/// calling a markdown file created — just harder to spot, because the extension is right.
/// With no roots recorded the extension test stands alone, so an older index degrades to
/// the previous behaviour rather than to silence.
fn covered(rel: &str, roots: &[String]) -> bool {
    if !is_indexable(rel) {
        return false;
    }
    if roots.is_empty() {
        return true;
    }
    roots
        .iter()
        .any(|r| rel == r.as_str() || rel.starts_with(&format!("{r}/")))
}

/// Last-modified time of the index, or `None` if it cannot be read.
fn stamp_of(db: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(db).ok()?.modified().ok()
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
    fn a_python_file_no_indexer_was_pointed_at_is_not_news() {
        // Measured on the target repository: 17 `.py` and `.go` files under `tools/` and
        // `infra/` were reported as `created` for ever, because the extension was right
        // and nothing checked whether an indexer had ever been aimed there. `verify
        // --repo` called the same tree clean. A staleness number nobody can act on is the
        // one people learn to skip past.
        let roots = vec!["srcpy".to_string(), "srcgo".to_string()];
        assert!(covered("srcpy/domains/orders/x.py", &roots));
        assert!(covered("srcgo/cmd/main.go", &roots));
        assert!(!covered("tools/eval/run_eval.py", &roots));
        assert!(!covered("infra/sentinel/main.go", &roots));
        // A directory whose name merely starts the same way is not inside it.
        assert!(!covered("srcpython/x.py", &roots));
        // Still not indexable whatever the roots say.
        assert!(!covered("srcpy/README.md", &roots));
        // No roots recorded: the extension test stands alone, as it did before.
        assert!(covered("tools/eval/run_eval.py", &[]));
    }

    #[test]
    fn reindexing_under_the_daemon_clears_what_it_made_current() {
        // The defect this pins: `indexed` was read once at start-up and compared against
        // forever, so after an edit *and a reindex* `status` still reported the file as
        // modified while `verify --repo` — which reads the store — reported a clean tree.
        // Two commands, one fact, disagreeing; the stress harness caught it in the wild.
        let dir = std::env::temp_dir().join(format!("cairn-reindex-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("a.py");
        let db = dir.join("index.sqlite");
        std::fs::write(&file, "original\n").unwrap();
        std::fs::write(&db, b"v1").unwrap();

        let t = DirtyTracker::new(
            &dir,
            HashMap::from([("a.py".to_string(), hash16(b"original\n"))]),
        );
        t.initial_scan();

        // Someone edits the file: correctly dirty.
        std::fs::write(&file, "changed\n").unwrap();
        t.recheck("a.py");
        assert_eq!(t.snapshot().modified, vec!["a.py".to_string()]);

        // Then reindexes. The store now records the new content; the index file's
        // timestamp is the only signal that reaches the daemon, because `.cairn` is
        // ignored by the watcher.
        t.watch_index(
            &db,
            Box::new(|| Some(HashMap::from([("a.py".to_string(), hash16(b"changed\n"))]))),
        );
        std::fs::write(&db, b"v2").unwrap();
        filetime_bump(&db);

        assert!(t.refresh_if_reindexed(), "a rebuilt index is news");
        assert!(
            t.snapshot().modified.is_empty(),
            "the file matches the index that was just built, so nothing is stale: {:?}",
            t.snapshot().modified
        );
        // And it does not re-fire on every request once it has caught up.
        assert!(!t.refresh_if_reindexed(), "an unchanged index is not news");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Push a file's mtime forward so the change is visible whatever the clock's
    /// resolution. Two writes inside one filesystem tick are indistinguishable otherwise,
    /// and that is a flaky test rather than a real one.
    fn filetime_bump(p: &Path) {
        let later = std::time::SystemTime::now() + Duration::from_secs(2);
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(later))
            .unwrap();
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
    fn a_file_no_indexer_reads_is_not_a_new_file() {
        // The daemon reported 590 created on a tree nobody had touched: docs, compose
        // files and protos are not in the index because nothing indexes them, and the
        // walk read that as new work. The signal has to be quiet when the tree is clean,
        // or the one edit that matters arrives inside a crowd.
        assert!(is_indexable("srcpy/domains/orders/repo.py"));
        assert!(is_indexable("srcgo/cmd/server/main.go"));
        assert!(is_indexable("srcpy/schema/x.pyi"));
        assert!(!is_indexable("docs/architecture.md"));
        assert!(!is_indexable("compose.yaml"));
        assert!(!is_indexable("proto/api/service.proto"));
        assert!(!is_indexable("tools/sql/01_ranked.sql"));
        assert!(!is_indexable("Makefile"));
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
        assert_eq!(
            t.snapshot().generation,
            g,
            "no content change, no generation bump"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
