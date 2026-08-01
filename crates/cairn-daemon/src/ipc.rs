//! The local socket, per platform.
//!
//! Unix domain sockets rather than named pipes on Windows, which needs saying because
//! named pipes are the more obvious choice there. The protocol leans on read and write
//! timeouts — `FAST_TIMEOUT` in the client exists so a wedged daemon cannot add latency to
//! an ordinary query — and a socket honours `SO_RCVTIMEO` where a pipe handle does not.
//! Windows has carried `AF_UNIX` since Windows 10 build 17063, so the same socket file
//! beside the index works on every platform we ship and the two implementations stay one
//! code path rather than two.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};

#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
