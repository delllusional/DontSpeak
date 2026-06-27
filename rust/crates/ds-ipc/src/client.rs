//! Blocking RPC client over the [`crate::transport`] byte stream, used by
//! `ds-core` (for the SwiftUI app), the `dontspeak` MCP server, and the Claude
//! Code hooks. Every call is fallible by design: a missing socket means "engine
//! down", and callers fall back to their legacy path.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::{io, time::Duration};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Connect to the engine socket at `sock_path`. Err ⇒ engine not running.
pub fn connect(sock_path: &Path) -> io::Result<Client> {
    let stream = transport::connect(sock_path)?;
    // Don't let a wedged engine hang a client forever, but stay generous for
    // STREAMING reads: a `dictate`/test-recognition session can listen up to ~60s
    // (possibly silent, so no partials arrive) before its final transcript, which a
    // shorter timeout would falsely abort. 120s covers the longest dictate + final pass.
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(Client {
        writer: stream.try_clone()?,
        reader: BufReader::new(stream),
    })
}

/// Fire-and-forget convenience: connect, send one request, read until the
/// terminal response, return it. Err if the engine is down or the link breaks.
pub fn request(sock_path: &Path, req: &Request) -> io::Result<Response> {
    let mut c = connect(sock_path)?;
    c.send(req)?;
    c.recv_terminal()
}

/// A connected client. Streaming responses are drained line by line via
/// [`Client::recv`].
pub struct Client {
    writer: Stream,
    reader: BufReader<Stream>,
}

impl Client {
    /// Write one request line.
    pub fn send(&mut self, req: &Request) -> io::Result<()> {
        let mut s = serde_json::to_string(req).map_err(io::Error::other)?;
        s.push('\n');
        self.writer.write_all(s.as_bytes())?;
        self.writer.flush()
    }

    /// Read one response line. Err on EOF (engine closed) or a parse failure.
    pub fn recv(&mut self) -> io::Result<Response> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "engine closed the connection",
            ));
        }
        serde_json::from_str(line.trim()).map_err(io::Error::other)
    }

    /// Read lines until a terminal response, returning that terminal line. For
    /// streaming requests, intermediate non-terminal lines are dropped — use
    /// [`Client::recv`] in a loop if you need them.
    pub fn recv_terminal(&mut self) -> io::Result<Response> {
        loop {
            let resp = self.recv()?;
            if resp.is_terminal() {
                return Ok(resp);
            }
        }
    }
}
