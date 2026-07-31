//! Socket server.
//!
//! Plain blocking threads rather than an async runtime: the daemon holds one watcher
//! and answers one tiny request per CLI invocation. An executor would be more
//! machinery than the job has work.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use crate::watch::DirtyTracker;
use crate::{DaemonStatus, Request, Response};

pub struct Daemon {
    repo: PathBuf,
    socket: PathBuf,
    tracker: Arc<DirtyTracker>,
    started: Instant,
}

impl Daemon {
    pub fn new(repo: &Path, socket: &Path, indexed: HashMap<String, [u8; 16]>) -> Daemon {
        Daemon {
            repo: repo.to_path_buf(),
            socket: socket.to_path_buf(),
            tracker: Arc::new(DirtyTracker::new(repo, indexed)),
            started: Instant::now(),
        }
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
        let (response, shutdown) = match serde_json::from_str::<Request>(line) {
            Ok(Request::Dirty) => (Response::Dirty(self.tracker.snapshot()), false),
            Ok(Request::Status) => (
                Response::Status(DaemonStatus {
                    repo: self.repo.to_string_lossy().to_string(),
                    watching: true,
                    files_tracked: self.tracker.tracked(),
                    generation: self.tracker.snapshot().generation,
                    uptime_secs: self.started.elapsed().as_secs(),
                }),
                false,
            ),
            Ok(Request::Shutdown) => (Response::Ok, true),
            Err(e) => (
                Response::Error { message: format!("bad request: {e}") },
                false,
            ),
        };
        writeln!(writer, "{}", serde_json::to_string(&response)?)?;
        writer.flush()?;
        Ok(shutdown)
    }
}
