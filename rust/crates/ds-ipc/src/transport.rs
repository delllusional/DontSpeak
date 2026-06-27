//! Transport seam for the RPC plane. Both ends (`server`, `client`) speak
//! newline-delimited JSON over a byte stream supporting `Read`, `Write`,
//! `try_clone()`, `set_read_timeout` and `set_write_timeout`. This module is the
//! ONLY place that names the concrete OS transport, so the framing/timeout logic
//! above it is platform-agnostic.
//!
//! - unix (macOS/Linux): a filesystem Unix-domain socket — the current, shipping
//!   transport, kept byte-identical (stale-socket unlink on bind, same accept
//!   loop, same timeouts set by the client).
//! - windows: a real AF_UNIX filesystem socket via the `uds_windows` crate
//!   (Win10 1803+ ships AF_UNIX; std just doesn't expose it). Its
//!   `UnixListener`/`UnixStream` mirror std's surface, so this arm is a
//!   near-verbatim copy of the unix one and nothing above this module changes.
//!   SECURITY: same filesystem-scoped `.sock` model as unix — the socket lives in
//!   the user-only `~/.claude`, so no loopback-TCP + auth-token handshake is
//!   needed (which is why the earlier TCP design was dropped).

use std::io;
use std::path::Path;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;

    /// The connected byte stream (one per client connection).
    pub type Stream = UnixStream;
    /// The accepting endpoint owned by the server.
    pub type Listener = UnixListener;

    /// Bind the server endpoint at `path`. Removes a stale socket file from a
    /// crashed run first (so a restart never fails with `EADDRINUSE`) and creates
    /// the parent dir.
    pub fn bind(path: &Path) -> io::Result<Listener> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        UnixListener::bind(path)
    }

    /// Connect a client stream to the server endpoint at `path`.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

#[cfg(windows)]
mod imp {
    // Real AF_UNIX filesystem socket via `uds_windows` — a near-verbatim mirror of
    // the unix arm (same stale-socket unlink on bind, same parent-dir creation).
    use std::io;
    use std::path::Path;
    use uds_windows::{UnixListener, UnixStream};

    /// The connected byte stream (one per client connection).
    pub type Stream = UnixStream;
    /// The accepting endpoint owned by the server.
    pub type Listener = UnixListener;

    /// Bind the server endpoint at `path`. Removes a stale socket file from a
    /// crashed run first (AF_UNIX bind fails if the path exists) and creates the
    /// parent dir.
    pub fn bind(path: &Path) -> io::Result<Listener> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        UnixListener::bind(path)
    }

    /// Connect a client stream to the server endpoint at `path`.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

pub use imp::{Listener, Stream};

/// Bind the server endpoint at `path` (see backend docs).
pub fn bind(path: &Path) -> io::Result<Listener> {
    imp::bind(path)
}

/// Connect a client stream to the server endpoint at `path`.
pub fn connect(path: &Path) -> io::Result<Stream> {
    imp::connect(path)
}
