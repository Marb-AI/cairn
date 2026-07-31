//! Thin client.
//!
//! Every call is best-effort: a missing daemon is a normal state, not an error. The
//! CLI must work without one — it simply cannot report live staleness, and says so
//! rather than implying the index is current.

use crate::{DirtySet, Request, Response};
use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Timeout for the cheap requests. Deliberately tight: `dirty` is asked on *every*
/// CLI invocation, so a wedged daemon must never noticeably delay an ordinary query.
const FAST_TIMEOUT: Duration = Duration::from_millis(250);

/// Timeout for requests that make the daemon do real work. A language server query
/// after an edit costs tens of milliseconds warm, but pyright's first cross-file query
/// measured 1.35 s even after warm-up (spike-0-results 4.2c), so the bound has to leave
/// room for the cold case rather than turning it into a spurious failure.
const WORK_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Client {
    stream: UnixStream,
    /// Held across calls: a fresh BufReader per request would discard bytes it had
    /// already buffered from the socket.
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connect, or return None when no daemon is listening.
    pub fn connect(socket: &Path) -> Option<Client> {
        let stream = UnixStream::connect(socket).ok()?;
        let _ = stream.set_read_timeout(Some(FAST_TIMEOUT));
        let _ = stream.set_write_timeout(Some(FAST_TIMEOUT));
        let reader = BufReader::new(stream.try_clone().ok()?);
        Some(Client { stream, reader })
    }

    fn call(&mut self, req: Request) -> Result<Response> {
        self.call_with(req, FAST_TIMEOUT)
    }

    fn call_with(&mut self, req: Request, timeout: Duration) -> Result<Response> {
        let _ = self.stream.set_read_timeout(Some(timeout));
        writeln!(self.stream, "{}", serde_json::to_string(&req)?)?;
        self.stream.flush()?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(serde_json::from_str(line.trim())?)
    }

    pub fn dirty(&mut self) -> Result<DirtySet> {
        match self.call(Request::Dirty)? {
            Response::Dirty(d) => Ok(d),
            Response::Error { message } => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected response: {other:?}")),
        }
    }

    pub fn status(&mut self) -> Result<crate::DaemonStatus> {
        match self.call(Request::Status)? {
            Response::Status(s) => Ok(s),
            Response::Error { message } => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected response: {other:?}")),
        }
    }

    /// Ask the language server what is in a file right now.
    pub fn file_symbols(&mut self, path: &str) -> Result<Vec<crate::lsp::LiveSymbol>> {
        match self.call_with(Request::FileSymbols { path: path.to_string() }, WORK_TIMEOUT)? {
            Response::FileSymbols { symbols } => Ok(symbols),
            Response::Error { message } => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected response: {other:?}")),
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.call(Request::Shutdown)?;
        Ok(())
    }
}

/// Ask the daemon what has changed, if there is one.
///
/// Returns None when no daemon is running, which the caller must surface: an empty
/// dirty set and an unknown dirty set look identical in an answer, and conflating them
/// is exactly the silent staleness the design forbids.
pub fn dirty_if_running(socket: &Path) -> Option<DirtySet> {
    Client::connect(socket)?.dirty().ok()
}
