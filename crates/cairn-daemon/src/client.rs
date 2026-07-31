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

/// Connect timeout. Generous enough for a loaded machine, short enough that a stale
/// socket never noticeably delays a query.
const TIMEOUT: Duration = Duration::from_millis(250);

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
        let _ = stream.set_read_timeout(Some(TIMEOUT));
        let _ = stream.set_write_timeout(Some(TIMEOUT));
        let reader = BufReader::new(stream.try_clone().ok()?);
        Some(Client { stream, reader })
    }

    fn call(&mut self, req: Request) -> Result<Response> {
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
