//! Socket server.
//!
//! Plain blocking threads rather than an async runtime: the daemon holds one watcher
//! and answers one tiny request per CLI invocation. An executor would be more
//! machinery than the job has work.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crate::ipc::{UnixListener, UnixStream};
use crate::lsp::Pool;
use crate::watch::DirtyTracker;
use crate::{DaemonStatus, Request, Response};

pub struct Daemon {
    repo: PathBuf,
    socket: PathBuf,
    tracker: Arc<DirtyTracker>,
    started: Instant,
    /// Guarded rather than per-connection: language servers are expensive and
    /// stateful, so one pool is shared and requests to it are serialised.
    pool: Arc<std::sync::Mutex<Pool>>,
}

impl Daemon {
    /// `container` names the repository's running container and where the repository is
    /// mounted inside it, when the language servers are to be run there rather than on
    /// this machine.
    pub fn new(
        repo: &Path,
        socket: &Path,
        indexed: HashMap<String, [u8; 16]>,
        roots: &[(String, String)],
        container: Option<(&str, &str)>,
    ) -> Daemon {
        Daemon {
            repo: repo.to_path_buf(),
            socket: socket.to_path_buf(),
            tracker: {
                let t = DirtyTracker::new(repo, indexed);
                // The same roots the language-server pool gets: what the indexers were
                // pointed at is exactly what the `created` set may report on.
                t.set_roots(roots.iter().map(|(_, r)| r.clone()).collect());
                Arc::new(t)
            },
            started: Instant::now(),
            pool: Arc::new(std::sync::Mutex::new(Pool::new(repo, roots, container))),
        }
    }

    /// Where the index lives and how to re-read it, so a rebuild is noticed.
    ///
    /// Separate from `new` because the daemon must start and watch even when the store
    /// cannot be reopened later: a watcher that reports file changes is worth having on
    /// its own, and that is the same reasoning the container start-up already follows.
    pub fn watch_index(self, db: &Path, reload: crate::watch::ReloadIndexed) -> Daemon {
        self.tracker.watch_index(db, reload);
        self
    }

    /// Serve until shutdown. Blocks.
    pub fn run(self) -> Result<()> {
        // A socket left by a crashed daemon would make bind fail; taking it over is
        // safe here because the caller has already failed to connect to it.
        let _ = std::fs::remove_file(&self.socket);
        let listener = UnixListener::bind(&self.socket)
            .with_context(|| format!("binding {}", self.socket.display()))?;

        // The first scan is the expensive one (one hash per indexed file), so it runs
        // off the accept loop. Until it finishes the set reports `complete: false` and
        // clients say so rather than treating an empty set as "nothing changed".
        let scanner = Arc::clone(&self.tracker);
        std::thread::spawn(move || scanner.initial_scan());

        // Warm the servers off the accept loop. Measurement showed the first
        // cross-file query costs an order of magnitude more than the rest, so paying
        // that at start-up rather than on the first question is the whole point of
        // having a daemon (spike-0-results 4.2c).
        let pool = Arc::clone(&self.pool);
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut p = pool.lock().unwrap();
            p.warm();
            let langs = p.languages().join(", ");
            let failed = p.failures().len();
            eprintln!(
                "cairn daemon: language servers ready ({langs}) in {:.1}s, {failed} unavailable",
                t0.elapsed().as_secs_f64()
            );
        });

        let (stop_tx, stop_rx) = mpsc::channel();
        let watcher = Arc::clone(&self.tracker);
        let watch_handle = std::thread::spawn(move || {
            if let Err(e) = watcher.watch_forever(stop_rx) {
                eprintln!("cairn daemon: watcher stopped: {e:#}");
            }
        });

        eprintln!(
            "cairn daemon: watching {} on {}",
            self.repo.display(),
            self.socket.display()
        );

        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            match self.serve_one(stream) {
                Ok(true) => break, // shutdown requested
                Ok(false) => {}
                Err(e) => eprintln!("cairn daemon: {e:#}"),
            }
        }

        let _ = stop_tx.send(());
        let _ = std::fs::remove_file(&self.socket);
        self.pool.lock().unwrap().shutdown();
        let _ = watch_handle.join();
        Ok(())
    }

    /// Serve a connection until the client hangs up. Returns true when asked to shut
    /// down.
    ///
    /// One connection carries many requests: `cairn status` asks for both status and
    /// the dirty set, and closing after the first left it talking to a dead socket.
    fn serve_one(&self, stream: UnixStream) -> Result<bool> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(false); // client hung up
            }
            if line.trim().is_empty() {
                continue;
            }
            if self.respond(&mut writer, line.trim())? {
                return Ok(true);
            }
        }
    }

    /// Answer one request. Returns true when it was a shutdown.
    fn respond(&self, writer: &mut UnixStream, line: &str) -> Result<bool> {
        // A caller asking what has changed is the moment the answer has to account for a
        // rebuilt index. Nothing else can notice: `.cairn` is in the watcher's ignore
        // list, so no file-system event ever arrives for the index itself. One `stat`
        // when the index has not moved, which is the common case.
        self.tracker.refresh_if_reindexed();
        let (response, shutdown) = match serde_json::from_str::<Request>(line) {
            Ok(Request::Dirty) => (Response::Dirty(self.tracker.snapshot()), false),
            Ok(Request::Status) => (
                Response::Status(DaemonStatus {
                    repo: self.repo.to_string_lossy().to_string(),
                    watching: true,
                    files_tracked: self.tracker.tracked(),
                    generation: self.tracker.snapshot().generation,
                    uptime_secs: self.started.elapsed().as_secs(),
                    reindex_due: self.tracker.snapshot().reindex_due,
                }),
                false,
            ),
            Ok(Request::FileSymbols { path }) => {
                match self.pool.lock().unwrap().document_symbols(&path) {
                    Ok(symbols) => (Response::FileSymbols { symbols }, false),
                    Err(e) => (
                        Response::Error {
                            message: format!("{e:#}"),
                        },
                        false,
                    ),
                }
            }
            Ok(Request::Shutdown) => (Response::Ok, true),
            Err(e) => (
                Response::Error {
                    message: format!("bad request: {e}"),
                },
                false,
            ),
        };
        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;
        Ok(shutdown)
    }
}
