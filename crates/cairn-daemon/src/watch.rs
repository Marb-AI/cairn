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
    // Package-manager stores, which are dependencies rather than code and are large
    // enough to dominate a search: a pnpm store inside one repository held 73,731 files
    // against 1,059 of source, and `for find` reported "from 73435 searched" — a number
    // that says the answer was thorough when almost all of it was other people's code.
    ".pnpm-store",
    ".yarn",
    ".expo",
    ".turbo",
    "coverage",
];

/// Events arrive in bursts - one editor save can produce several, and a git operation
/// produces thousands. Coalescing them keeps rehashing proportional to real changes.
const DEBOUNCE: Duration = Duration::from_millis(120);

pub struct DirtyTracker {
    inner: Arc<Mutex<State>>,
    /// How to re-run a connector. Outside `State` so a refresh can run without holding
    /// the lock every query needs.
    refresh: Arc<Mutex<Option<RefreshConnector>>>,
    /// Connectors with a refresh in flight, so a burst of saves does not start five runs
    /// of the same connector.
    refreshing: Arc<Mutex<std::collections::BTreeSet<String>>>,
}

/// Re-reads path -> content hash from the store. Supplied by the caller because the
/// daemon deliberately does not depend on `cairn-store`: the watcher is a file-system
/// concern and pulling a database driver into it to answer one question would invert the
/// layering for the sake of one call.
pub type ReloadIndexed = Box<dyn Fn() -> Option<HashMap<String, [u8; 16]>> + Send>;

/// One connector the watcher knows about: its name, and whether a path is one of its
/// inputs.
///
/// A predicate rather than the globs, for the same reason `ReloadIndexed` is a closure:
/// the glob matcher lives beside the manifest parser in `cairn-store`, and the daemon has
/// no business depending on that to answer one question. The caller compiles the
/// manifest's `inputs` into this.
pub struct ConnectorWatch {
    pub name: String,
    pub matches: Box<dyn Fn(&str) -> bool + Send + Sync>,
}

/// Re-run one connector and apply its facts, returning what to say if it failed.
///
/// Supplied by the caller for the same layering reason. The daemon decides *when* and
/// *which*; running one means opening the store, which is not the watcher's business.
pub type RefreshConnector = Box<dyn Fn(&str) -> Result<()> + Send + Sync>;

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
    /// Extensions the index actually holds documents for, taken from the index itself —
    /// repository-wide and per project root. See `Coverage`.
    indexable: Coverage,
    /// Connectors whose inputs this watcher can recognise.
    connectors: Vec<ConnectorWatch>,
    /// Connectors whose inputs have changed since their facts were built, and which are
    /// therefore being refreshed or waiting to be. Reported on every answer: a query must
    /// never wait for one (D7), so it answers from the previous facts and says whose they
    /// are.
    stale_connectors: std::collections::BTreeSet<String>,
}

impl DirtyTracker {
    /// `indexed` is the state to compare against: path -> content hash from the store.
    pub fn new(repo: &Path, indexed: HashMap<String, [u8; 16]>) -> DirtyTracker {
        // Absolute, because every path this tracker sees afterwards is absolute. `notify`
        // reports the path it watched, so a tracker built from `--repo .` compared
        // `/home/…/apps/a.ts` against `.`, `strip_prefix` failed, and **every event was
        // dropped**. The daemon ran, the socket answered, `status` said `stale: none`, and
        // the tree had changed underneath it. Nothing in the output could tell that state
        // from a clean one, which makes it the worst kind: a confident negative from a
        // component that never looked.
        let repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        let indexable = coverage_of(&indexed, &[]);
        DirtyTracker {
            inner: Arc::new(Mutex::new(State {
                repo,
                indexable,
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
                connectors: Vec::new(),
                stale_connectors: Default::default(),
            })),
            refresh: Arc::new(Mutex::new(None)),
            refreshing: Arc::new(Mutex::new(Default::default())),
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
        let mut st = self.inner.lock().unwrap();
        // The per-root sets are derived from the roots, so they are rebuilt here rather
        // than only in `new` — which runs before the caller has said what the roots are.
        st.indexable = coverage_of(&st.indexed, &roots);
        st.roots = roots;
    }

    /// The connectors behind this index, and how to re-run one.
    ///
    /// Without this, a change to a `.proto` file left a connector's facts describing the
    /// previous version of it while every answer reported the tree as clean - the
    /// connector's inputs are not code, so nothing in the dirty set was ever about them.
    pub fn set_connectors(&self, connectors: Vec<ConnectorWatch>, refresh: RefreshConnector) {
        self.inner.lock().unwrap().connectors = connectors;
        *self.refresh.lock().unwrap() = Some(refresh);
    }

    /// Which connectors claim this path as an input.
    fn connectors_for(&self, rel: &str) -> Vec<String> {
        let st = self.inner.lock().unwrap();
        st.connectors
            .iter()
            .filter(|c| (c.matches)(rel))
            .map(|c| c.name.clone())
            .collect()
    }

    /// Mark the connectors a changed path belongs to, and start refreshing them.
    ///
    /// Only the ones that match: a `.proto` change is nothing to a connector that reads
    /// compose files, and re-running every connector on every save would make the daemon
    /// the most expensive thing in the repository.
    fn connectors_changed(self: &Arc<Self>, rel: &str) {
        let names = self.connectors_for(rel);
        if names.is_empty() {
            return;
        }
        {
            let mut st = self.inner.lock().unwrap();
            for name in &names {
                st.stale_connectors.insert(name.clone());
            }
            // The generation moves because what an answer would say has changed, even
            // though no file in the dirty set did.
            st.dirty.generation += 1;
        }
        for name in names {
            // One run per connector at a time. A burst of saves - a codegen pass, a
            // branch switch - would otherwise start a run per event, and they would
            // fight over the same rows.
            {
                let mut in_flight = self.refreshing.lock().unwrap();
                if !in_flight.insert(name.clone()) {
                    continue;
                }
            }
            let this = Arc::clone(self);
            // A thread, because a query must never wait for a connector (D7). While this
            // runs, `stale_connectors` is what every answer reports.
            std::thread::spawn(move || {
                let outcome = {
                    let refresh = this.refresh.lock().unwrap();
                    match refresh.as_ref() {
                        Some(f) => f(&name),
                        None => Ok(()),
                    }
                };
                match outcome {
                    Ok(()) => {
                        let mut st = this.inner.lock().unwrap();
                        st.stale_connectors.remove(&name);
                        st.dirty.generation += 1;
                        eprintln!("cairn daemon: connector {name} refreshed");
                    }
                    // Left stale on purpose: the facts in the index are the previous
                    // run's, and every answer has to go on saying so until a run works.
                    Err(e) => eprintln!("cairn daemon: connector {name} refresh failed: {e:#}"),
                }
                this.refreshing.lock().unwrap().remove(&name);
            });
        }
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
            // The set follows the index: a reindex that adds a language must not leave
            // the watcher judging new files by the languages of the previous build.
            st.indexable = coverage_of(&fresh, &st.roots);
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
        st.dirty.stale_connectors = st.stale_connectors.iter().cloned().collect();
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

    #[cfg(test)]
    fn repo_root(&self) -> PathBuf {
        self.inner.lock().unwrap().repo.clone()
    }

    /// Full comparison of the working tree against the index. Run once at start so the
    /// set is correct before any event arrives, and marked `complete` when done.
    pub fn initial_scan(&self) {
        let (repo, indexed, roots, indexable) = {
            let st = self.inner.lock().unwrap();
            (
                st.repo.clone(),
                st.indexed.clone(),
                st.roots.clone(),
                st.indexable.clone(),
            )
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
        walk_new(&repo, &repo, &indexed, &roots, &indexable, &mut created);

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
            (None, Some(_)) if covered(rel, &st.roots, &st.indexable) => {
                st.dirty.created.push(rel.to_string())
            }
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
    pub fn watch_forever(self: &Arc<Self>, stop: mpsc::Receiver<()>) -> Result<()> {
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
                    // A connector's inputs are usually not code, so nothing above would
                    // have noticed them: `.proto` is not in the index's extensions and
                    // `recheck` correctly says nothing about it. This is the only place
                    // that learns a declared fact may be out of date.
                    self.connectors_changed(&rel);
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
    indexable: &Coverage,
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
            walk_new(root, &path, indexed, roots, indexable, out);
        } else if covered(&rel, roots, indexable) && !indexed.contains_key(&rel) {
            out.push(rel);
        }
    }
}

/// Extensions the index actually holds documents for, read off the index.
///
/// This was a list with a comment above it saying **update this when a language is
/// added**. TypeScript was added and it was not updated, so on a repository whose index
/// is 25 053 TypeScript symbols a brand-new `.ts` module was reported as `0 created` —
/// the watcher going quiet about a whole new module, which is precisely what the comment
/// predicted. A note that has to be obeyed is not a mechanism. The index knows which
/// kinds of file it holds, and asking it cannot fall behind the thing it describes.
fn extensions_of(indexed: &HashMap<String, [u8; 16]>) -> std::collections::HashSet<String> {
    indexed
        .keys()
        .filter_map(|p| Path::new(p).extension().and_then(|e| e.to_str()))
        .map(str::to_string)
        .collect()
}

/// Which extensions the index holds, repository-wide and **per project root**.
///
/// The repository-wide set alone was measured wrong on a 4,786-file monorepo: three `.js`
/// and three `.json` documents somewhere in it made those two extensions "indexable"
/// everywhere, so every `package.json`, `tsconfig.json` and `jest.config.js` inside any
/// indexed project was reported as created on a tree nobody had touched — 90 files, on a
/// line that rides on every answer. Per root the same tree is quiet: `frontend/apps/auth`
/// holds `.ts` and `.tsx` and nothing else, so its `package.json` is not a file the index
/// could have held.
///
/// Both are kept because both are needed: the per-root sets for the judgement, the
/// repository-wide one for a file in a root the index holds nothing for.
#[derive(Debug, Clone, Default)]
struct Coverage {
    global: std::collections::HashSet<String>,
    by_root: std::collections::BTreeMap<String, std::collections::HashSet<String>>,
}

fn coverage_of(indexed: &HashMap<String, [u8; 16]>, roots: &[String]) -> Coverage {
    let mut by_root: std::collections::BTreeMap<String, std::collections::HashSet<String>> = roots
        .iter()
        .map(|r| (r.clone(), std::collections::HashSet::new()))
        .collect();
    for path in indexed.keys() {
        let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
            continue;
        };
        for (root, exts) in by_root.iter_mut() {
            if path == root || path.starts_with(&format!("{root}/")) {
                exts.insert(ext.to_string());
            }
        }
    }
    Coverage {
        global: extensions_of(indexed),
        by_root,
    }
}

/// What to fall back on when the index holds no files at all.
///
/// Only reachable with an empty index, where every question about staleness is moot. It
/// exists so that case degrades to the old behaviour rather than to silence.
const FALLBACK_EXTENSIONS: &[&str] = &["py", "pyi", "go", "ts", "tsx", "js", "jsx"];

/// Could the index have held this file at all?
///
/// Measured: on a clean tree the daemon reported `590 created` and advised a reindex,
/// because everything that is not Python or Go - documentation, compose files, protos,
/// SQL - is absent from the index by design and the walk read that absence as "new". A
/// staleness signal that is loud on a tree nobody has touched is one an agent learns to
/// ignore, which is worse than not having it: the one real edit that follows is buried in
/// it. `cairn verify --repo .` was exact throughout, which is what made the daemon's
/// number visibly wrong rather than merely unexplained.
pub fn is_indexable(rel: &str, indexable: &std::collections::HashSet<String>) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            if indexable.is_empty() {
                FALLBACK_EXTENSIONS.contains(&e)
            } else {
                indexable.contains(e)
            }
        })
}

/// Could the index have held this path at all: right extension, and inside a directory
/// an indexer was actually pointed at.
///
/// The extension test alone was not enough. `tools/eval/run_eval.py` is Python, and no
/// SCIP run has ever looked at it, so calling it "created" is the same false alarm as
/// calling a markdown file created — just harder to spot, because the extension is right.
/// With no roots recorded the extension test stands alone, so an older index degrades to
/// the previous behaviour rather than to silence.
fn covered(rel: &str, roots: &[String], indexable: &Coverage) -> bool {
    if roots.is_empty() {
        return is_indexable(rel, &indexable.global);
    }
    let containing: Vec<&String> = roots
        .iter()
        .filter(|r| rel == r.as_str() || rel.starts_with(&format!("{r}/")))
        .collect();
    if containing.is_empty() {
        return false;
    }
    // The NEAREST containing root decides, walking outward only when a root holds
    // nothing. Roots nest - `w17/services/x` is a Go module and `w17/services/x/spa` is a
    // TypeScript project inside it - and the union of the two was measured wrong: the Go
    // root holds a `schema/admin.json`, which made `spa/package.json` and
    // `spa/tsconfig.json` news in all three such services. The inner project's own
    // languages are the ones that answer for a file inside it.
    let mut containing = containing;
    containing.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for r in containing {
        match indexable.by_root.get(r.as_str()) {
            // A root the index holds nothing for says nothing about this file - a project
            // whose indexer failed outright is the ordinary way that happens - so keep
            // walking outward rather than reading its silence as a no.
            Some(exts) if !exts.is_empty() => return is_indexable(rel, exts),
            _ => continue,
        }
    }
    // Inside a root, but no root that contains it holds anything: fall back to the
    // repository-wide answer rather than going silent about new code there.
    is_indexable(rel, &indexable.global)
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
        assert!(is_ignored(".pnpm-store/v3/files/00/abcd"));
        assert!(is_ignored("apps/web/.expo/types/router.d.ts"));
        assert!(is_ignored(".cairn/index.sqlite"));
        assert!(!is_ignored("srcpy/domains/orders/x.py"));
        // A path merely containing the word is not a match.
        assert!(!is_ignored("srcpy/targeting/x.py"));
    }

    #[test]
    fn the_idle_window_outlasts_thinking_and_not_a_test_run() {
        // The numbers are the whole design, so they are asserted rather than left to a
        // reading of the constants. 135 daemons were alive at once on one workstation,
        // 63 of them started by `cargo test` runs against temp fixtures: the window has
        // to be short enough that those are gone before anyone notices, and long enough
        // that a person who steps away does not pay a respawn on every command.
        assert!(
            crate::server::IDLE_TIMEOUT >= Duration::from_secs(10 * 60),
            "too short: a pause for thought would cost a respawn"
        );
        assert!(
            crate::server::IDLE_TIMEOUT <= Duration::from_secs(60 * 60),
            "too long: a day's test runs would still pile up"
        );
        assert!(
            crate::server::IDLE_POLL < crate::server::IDLE_TIMEOUT,
            "a poll slower than the window would never fire"
        );
    }

    /// An `indexed` map from paths alone: the hashes are never compared in these tests.
    fn indexed(paths: &[&str]) -> HashMap<String, [u8; 16]> {
        paths.iter().map(|p| (p.to_string(), [0u8; 16])).collect()
    }

    #[test]
    fn a_python_file_no_indexer_was_pointed_at_is_not_news() {
        // Measured on the target repository: 17 `.py` and `.go` files under `tools/` and
        // `infra/` were reported as `created` for ever, because the extension was right
        // and nothing checked whether an indexer had ever been aimed there. `verify
        // --repo` called the same tree clean. A staleness number nobody can act on is the
        // one people learn to skip past.
        let roots = vec!["srcpy".to_string(), "srcgo".to_string()];
        let cov = coverage_of(
            &indexed(&["srcpy/domains/orders/repo.py", "srcgo/cmd/server/main.go"]),
            &roots,
        );
        assert!(covered("srcpy/domains/orders/x.py", &roots, &cov));
        assert!(covered("srcgo/cmd/main.go", &roots, &cov));
        assert!(!covered("tools/eval/run_eval.py", &roots, &cov));
        assert!(!covered("infra/sentinel/main.go", &roots, &cov));
        // A directory whose name merely starts the same way is not inside it.
        assert!(!covered("srcpython/x.py", &roots, &cov));
        // Still not indexable whatever the roots say.
        assert!(!covered("srcpy/README.md", &roots, &cov));
        // No roots recorded: the extension test stands alone, as it did before.
        assert!(covered("tools/eval/run_eval.py", &[], &cov));
    }

    #[test]
    fn an_extension_one_project_holds_is_not_news_in_a_project_that_holds_none() {
        // Measured on a 4,786-file monorepo: three `.js` and three `.json` documents
        // somewhere in it made both extensions indexable *everywhere*, so every
        // `package.json`, `tsconfig.json` and `jest.config.js` inside any indexed project
        // came back as created on a tree nobody had touched - 90 files, on the line that
        // rides on every answer. Per root the same tree is quiet.
        let roots = vec!["apps/auth".to_string(), "tooling".to_string()];
        let cov = coverage_of(
            &indexed(&[
                "apps/auth/src/login.ts",
                "apps/auth/src/view.tsx",
                // The one project that does hold a `.json` and a `.js`.
                "tooling/config.json",
                "tooling/build.js",
            ]),
            &roots,
        );
        // Still reported where that extension is genuinely part of the project.
        assert!(covered("tooling/other.json", &roots, &cov));
        assert!(covered("tooling/next.js", &roots, &cov));
        // And not reported where it is not.
        assert!(!covered("apps/auth/package.json", &roots, &cov));
        assert!(!covered("apps/auth/jest.config.js", &roots, &cov));
        // A new module in the language that project IS indexed in is still news - the
        // whole point of the `created` set, and the thing a blunter filter would lose.
        assert!(covered("apps/auth/src/brand/new.ts", &roots, &cov));
    }

    #[test]
    fn a_nested_project_is_judged_by_its_own_languages_not_its_parents() {
        // Measured on the same monorepo, and the reason this is nearest-root rather than
        // a union over every containing root: `w17/services/x` is a Go module holding a
        // `schema/admin.json`, and `w17/services/x/spa` is a TypeScript project inside it.
        // Pooling the two made `spa/package.json` and `spa/tsconfig.json` news in all
        // three such services - six files, on a tree nobody had touched.
        let roots = vec!["svc/admin".to_string(), "svc/admin/spa".to_string()];
        let cov = coverage_of(
            &indexed(&[
                "svc/admin/main.go",
                "svc/admin/schema/admin.json",
                "svc/admin/spa/src/main.tsx",
            ]),
            &roots,
        );
        // The parent's `.json` does not reach into the SPA.
        assert!(!covered("svc/admin/spa/package.json", &roots, &cov));
        assert!(!covered("svc/admin/spa/tsconfig.json", &roots, &cov));
        // The SPA's own language still is news, and so is the parent's.
        assert!(covered("svc/admin/spa/src/view.tsx", &roots, &cov));
        assert!(covered("svc/admin/cmd/serve.go", &roots, &cov));
        assert!(covered("svc/admin/schema/other.json", &roots, &cov));
    }

    #[test]
    fn a_root_the_index_holds_nothing_for_falls_back_rather_than_going_silent() {
        // A project whose indexer failed outright holds no documents, so its per-root set
        // is empty and says nothing about any file. Silence there would hide new code in
        // exactly the project already known to be incomplete.
        let roots = vec!["apps/ok".to_string(), "apps/failed".to_string()];
        let cov = coverage_of(&indexed(&["apps/ok/src/a.ts"]), &roots);
        assert!(covered("apps/failed/src/b.ts", &roots, &cov));
    }

    fn exts(list: &[&str]) -> std::collections::HashSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_new_file_in_a_language_the_index_holds_is_news_whatever_that_language_is() {
        // The defect: the list was `py`, `pyi`, `go`, written when those were the only
        // languages, with a comment saying to update it when one was added. TypeScript
        // was added and it was not, so a brand-new `.ts` module on a repository indexed
        // for TypeScript was reported as `0 created` - the watcher silent about exactly
        // the thing it exists to report. Reading the set off the index cannot fall behind
        // the index.
        let indexed = HashMap::from([
            ("apps/web/hooks/useThing.ts".to_string(), [0u8; 16]),
            ("apps/web/app/page.tsx".to_string(), [0u8; 16]),
        ]);
        let exts = coverage_of(&indexed, &[]);
        assert!(covered("apps/web/hooks/useOther.ts", &[], &exts));
        assert!(covered("apps/web/app/other.tsx", &[], &exts));
        // A language this index does not hold is still not news about this index.
        assert!(!covered("tools/gen.py", &[], &exts));
        assert!(!covered("apps/web/README.md", &[], &exts));

        // And the same index rebuilt for Python answers the other way round.
        let repython = HashMap::from([("srcpy/a.py".to_string(), [0u8; 16])]);
        let exts = coverage_of(&repython, &[]);
        assert!(covered("srcpy/b.py", &[], &exts));
        assert!(!covered("apps/web/hooks/useOther.ts", &[], &exts));
    }

    #[test]
    fn an_empty_index_falls_back_rather_than_going_silent() {
        // Nothing indexed means every extension question is moot, but answering "no" to
        // all of them would be the silent failure again, one layer down.
        let exts = Coverage::default();
        assert!(covered("srcpy/a.py", &[], &exts));
        assert!(covered("apps/web/a.tsx", &[], &exts));
        assert!(!covered("README.md", &[], &exts));
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
    fn a_relative_repo_root_is_made_absolute_before_anything_is_compared_against_it() {
        // `notify` reports the path it was told to watch, which is absolute. Started as
        // `cairn daemon --repo .` the tracker held ".", every `strip_prefix` failed, and
        // every event was dropped in silence: the daemon ran, answered on its socket, and
        // reported `stale: none` while the tree changed underneath it. Measured on the
        // frontend repository - a modified indexed file and two new ones, all invisible,
        // and the same test with an absolute path reported them within two seconds.
        assert!(
            relativise(Path::new("."), Path::new("/repo/apps/a.ts")).is_none(),
            "this is the comparison that was silently failing"
        );
        let t = DirtyTracker::new(Path::new("."), HashMap::new());
        assert!(
            t.repo_root().is_absolute(),
            "a tracker must not keep a root that can never match an event"
        );
    }

    #[test]
    fn a_file_no_indexer_reads_is_not_a_new_file() {
        // The daemon reported 590 created on a tree nobody had touched: docs, compose
        // files and protos are not in the index because nothing indexes them, and the
        // walk read that as new work. The signal has to be quiet when the tree is clean,
        // or the one edit that matters arrives inside a crowd.
        let e = exts(&["py", "pyi", "go"]);
        assert!(is_indexable("srcpy/domains/orders/repo.py", &e));
        assert!(is_indexable("srcgo/cmd/server/main.go", &e));
        assert!(is_indexable("srcpy/schema/x.pyi", &e));
        assert!(!is_indexable("docs/architecture.md", &e));
        assert!(!is_indexable("compose.yaml", &e));
        assert!(!is_indexable("proto/api/service.proto", &e));
        assert!(!is_indexable("tools/sql/01_ranked.sql", &e));
        assert!(!is_indexable("Makefile", &e));
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
