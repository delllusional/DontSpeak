//! Blocking RPC client over [`crate::transport`]. Used by `ds-core`, the MCP server, and hooks.
//! Fallible by design: missing socket ⇒ engine down; callers fall back.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::{io, time::Duration};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Connect to the engine socket. Err ⇒ engine not running.
pub fn connect(sock_path: &Path) -> io::Result<Client> {
    connect_with_read_timeout(sock_path, Duration::from_secs(120))
}

fn connect_with_read_timeout(sock_path: &Path, read_timeout: Duration) -> io::Result<Client> {
    let stream = transport::connect(sock_path)?;
    // 120s covers longest STREAMING dictate (~60s silence possible) + final pass; shorter
    // would abort a quiet session. Bounded status waits override it explicitly.
    stream.set_read_timeout(Some(read_timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(Client {
        writer: stream.try_clone()?,
        reader: BufReader::new(stream),
    })
}

/// Connect, one request, drain to terminal response. Err if engine down or link breaks.
pub fn request(sock_path: &Path, req: &Request) -> io::Result<Response> {
    let mut c = connect(sock_path)?;
    c.send(req)?;
    c.recv_terminal()
}

/// One request with a caller-owned read bound. Use for operations whose protocol timeout
/// is shorter than the default streaming-dictation allowance.
pub fn request_with_read_timeout(
    sock_path: &Path,
    req: &Request,
    read_timeout: Duration,
) -> io::Result<Response> {
    let mut c = connect_with_read_timeout(sock_path, read_timeout)?;
    c.send(req)?;
    c.recv_terminal()
}

/// Connected client. Stream intermediates via [`Client::recv`].
pub struct Client {
    writer: Stream,
    reader: BufReader<Stream>,
}

impl Client {
    pub fn send(&mut self, req: &Request) -> io::Result<()> {
        let mut s = serde_json::to_string(req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        s.push('\n');
        self.writer.write_all(s.as_bytes())?;
        self.writer.flush()
    }

    /// One response line. Err on EOF (engine closed) or parse failure.
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

    /// Drain until a terminal response (drops intermediates). Use [`Self::recv`] in a loop for partials.
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

    /// Throwaway socket pair so tests can script a fake engine without the daemon.
    fn socket_pair() -> (Stream, Stream) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = transport::bind(&path).expect("bind test socket");
        let accept_thread = thread::spawn(move || listener.accept().expect("accept").0);
        let client = transport::connect(&path).expect("connect test socket");
        let server = accept_thread.join().expect("join accept thread");
        (client, server)
    }

    /// Bypass `connect()`'s 120s read timeout so the timeout path is testable quickly.
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

    /// Engine close mid-wait must surface as UnexpectedEof, not hang/panic.
    #[test]
    fn recv_errs_on_eof() {
        let (client_stream, server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_secs(5));
        drop(server_stream); // "the engine closed the connection"

        let err = client.recv().expect_err("EOF must be an error, not Ok");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(err.to_string().contains("engine closed the connection"));
    }

    #[test]
    fn recv_errs_on_a_garbled_non_json_response_line() {
        let (client_stream, mut server_stream) = socket_pair();
        let mut client = client_with_timeout(client_stream, Duration::from_secs(5));

        server_stream.write_all(b"not json at all\n").unwrap();

        let err = client
            .recv()
            .expect_err("a garbled response line must be an error, not Ok");
        // serde parse failure → InvalidData, not an I/O-level failure.
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Non-terminal lines (`Listening`/`Partial`) skipped; first terminal (`Transcript`) returned.
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

    /// Short injected timeout: silent peer must error (not hang on real 120s).
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
        // Platform-dependent: WouldBlock/EAGAIN vs TimedOut.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "expected a timeout-flavored error, got: {err:?}"
        );
        drop(server_stream); // keep peer alive until here so this is timeout, not EOF
    }

    #[test]
    fn bounded_request_times_out_when_the_peer_accepts_but_never_replies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = transport::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            thread::sleep(Duration::from_millis(300));
            drop(stream);
        });

        let started = std::time::Instant::now();
        let err =
            request_with_read_timeout(&path, &Request::ModelStatus, Duration::from_millis(100))
                .expect_err("a bounded request must not inherit the 120s streaming timeout");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the caller-owned read timeout must bound the request: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "expected a timeout-flavored error, got: {err:?}"
        );
        server.join().expect("join fake server");
    }
}
