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
        let mut s = serde_json::to_string(req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
        serde_json::from_str(line.trim()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// A real connected (client, server) stream pair over a throwaway
    /// temp-dir socket, so tests can script a fake engine peer without
    /// running the actual daemon.
    fn socket_pair() -> (Stream, Stream) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = transport::bind(&path).expect("bind test socket");
        let accept_thread = thread::spawn(move || listener.accept().expect("accept").0);
        let client = transport::connect(&path).expect("connect test socket");
        let server = accept_thread.join().expect("join accept thread");
        (client, server)
    }

    /// Build a `Client` directly from a raw stream with a caller-chosen read
    /// timeout, bypassing `connect()`'s hardcoded 120s. That real timeout is
    /// otherwise untestable in a reasonable amount of time — this makes the
    /// timeout path exercisable with a short one instead.
    fn client_with_timeout(stream: Stream, read_timeout: Duration) -> Client {
        stream.set_read_timeout(Some(read_timeout)).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Client {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    /// Finding: "zero tests for EOF-on-recv". The engine closing the
    /// connection mid-wait must come back as a clear error, not a hang or a
    /// panic.
    #[test]
    fn recv_errs_on_eof() {
        let (client_stream, server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_secs(5));
        drop(server_stream); // "the engine closed the connection"

        let err = client.recv().expect_err("EOF must be an error, not Ok");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(err.to_string().contains("engine closed the connection"));
    }

    /// Finding: "zero tests for ... garbled response lines".
    #[test]
    fn recv_errs_on_a_garbled_non_json_response_line() {
        let (client_stream, mut server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_secs(5));

        server_stream.write_all(b"not json at all\n").unwrap();

        let err = client
            .recv()
            .expect_err("a garbled response line must be an error, not Ok");
        // Wrapped via `io::Error::other` around the serde_json parse failure,
        // not an I/O-level failure.
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Finding: "recv_terminal's unbounded loop over non-terminal responses".
    /// Several non-terminal lines (`Listening`, `Partial`) must be silently
    /// skipped, and the loop must stop at (and return) the first terminal
    /// line (`Transcript`).
    #[test]
    fn recv_terminal_skips_non_terminal_lines_then_returns_the_terminal_one() {
        let (client_stream, mut server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_secs(5));

        for resp in [
            Response::Listening,
            Response::Partial { text: "he".into() },
            Response::Partial {
                text: "hello".into(),
            },
        ] {
            let mut line = serde_json::to_string(&resp).unwrap();
            line.push('\n');
            server_stream.write_all(line.as_bytes()).unwrap();
        }
        let mut terminal_line = serde_json::to_string(&Response::Transcript {
            text: "hello".into(),
        })
        .unwrap();
        terminal_line.push('\n');
        server_stream.write_all(terminal_line.as_bytes()).unwrap();

        match client
            .recv_terminal()
            .expect("a terminal response is expected")
        {
            Response::Transcript { text } => assert_eq!(text, "hello"),
            other => panic!("expected Transcript, got {other:?}"),
        }
    }

    /// Finding: recv_terminal is "bounded only by an untested 120s timeout".
    /// Makes the timeout path reachable in a fast test by injecting a short
    /// read timeout instead of the real 120s one; a silent peer must
    /// eventually error rather than hang.
    #[test]
    fn recv_times_out_when_the_peer_goes_silent() {
        let (client_stream, server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_millis(200));

        let started = std::time::Instant::now();
        let err = client
            .recv()
            .expect_err("a silent peer must eventually error, not hang forever");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the injected short timeout should fire quickly, took {:?}",
            started.elapsed()
        );
        // A `set_read_timeout` expiry is platform-dependent between these two
        // kinds (e.g. WouldBlock/EAGAIN on some platforms, TimedOut on others).
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "expected a timeout-flavored error, got: {err:?}"
        );
        drop(server_stream); // keep the peer alive until here so this is a real timeout, not EOF
    }
}
