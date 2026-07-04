//! Blocking RPC server over the [`crate::transport`] byte stream. The engine
//! calls [`serve`] on a dedicated thread; it accepts connections, reads one
//! [`Request`] per line, and invokes the handler with an `emit` callback the
//! handler uses to write one-or-more [`Response`] lines back (supporting
//! streaming).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{io, thread};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Per-connection socket timeouts so a stuck client can't park a server thread
/// forever (one-thread-per-conn ⇒ otherwise a slow thread leak). Generous enough
/// not to abort the legitimate one-shot flow (a client drains a possibly ~120s
/// streaming `dictate` response, then closes); fires only on a client that sends a
/// partial line and never closes, or wedges mid-stream. Mirrors the client's own
/// timeouts (see `client::connect`).
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard cap on concurrent connections (⇒ threads, one-per-conn). Real usage is
/// a handful of callers (the app + a couple of hook processes), so this is
/// generous headroom, not a throttle. Without a cap, a local connection flood
/// could drive `thread::spawn` past the OS's thread-count limit; `spawn`
/// PANICS (there's no `Err` to handle) on refusal, which kills this detached,
/// never-restarted accept-loop thread outright (or aborts the whole process
/// under `panic = "abort"`) — permanently taking down the daemon's IPC. Capping
/// the accept loop keeps this daemon's own thread usage bounded regardless of
/// how many clients pile on.
const MAX_CONNECTIONS: usize = 64;

/// Max size (bytes) of a single request line the server will buffer before
/// erroring out and closing the connection, so a client that streams bytes
/// with no newline (defeating `READ_TIMEOUT`'s per-read granularity by
/// trickling data) can't force unbounded memory growth in this single-process
/// daemon. Generous versus any legitimate request: the largest realistic
/// payloads (`Speak`/`SpeakNarration` text, `ModelStatus`'s JSON `Value`) are
/// still comfortably KB-scale.
const MAX_LINE_LEN: usize = 1024 * 1024; // 1 MiB

/// Handler signature: given a parsed request, emit zero-or-more responses via the
/// callback. Must be thread-safe — one connection per thread runs it concurrently.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request, emit: &mut dyn FnMut(&Response));
}

impl<F> Handler for F
where
    F: Fn(Request, &mut dyn FnMut(&Response)) + Send + Sync + 'static,
{
    fn handle(&self, req: Request, emit: &mut dyn FnMut(&Response)) {
        self(req, emit)
    }
}

/// Bind `sock_path` and accept forever, dispatching each line to `handler`.
/// Removes a stale socket file first (a previous run that didn't clean up), so a
/// restart never fails with `EADDRINUSE`. Blocks; run on its own thread.
pub fn serve<H: Handler>(sock_path: &Path, handler: H) -> io::Result<()> {
    let listener = transport::bind(sock_path)?;
    let handler = Arc::new(handler);
    let active_conns = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if active_conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    // Already at the cap: refuse this connection instead of
                    // risking thread::spawn past the OS thread limit (see
                    // MAX_CONNECTIONS docs). Dropping `stream` just closes the
                    // socket; the client observes a clean EOF/reset rather
                    // than a hang.
                    active_conns.fetch_sub(1, Ordering::SeqCst);
                    drop(stream);
                    continue;
                }
                let h = Arc::clone(&handler);
                let active_conns = Arc::clone(&active_conns);
                // One thread per connection; cheap (clients are the app + hooks),
                // bounded by MAX_CONNECTIONS above.
                thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, h.as_ref()) {
                        // A client hanging up mid-write is normal; don't spam.
                        let _ = e;
                    }
                    active_conns.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_conn<H: Handler>(stream: Stream, handler: &H) -> io::Result<()> {
    // Bound a stuck/partial-line client so its thread can't leak (see the const
    // docs). Best-effort: a platform that rejects the option still serves.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line_buf = Vec::new();
    while let Some(line) = read_line_bounded(&mut reader, &mut line_buf)? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_line(&mut writer, &Response::error(format!("bad request: {e}")))?;
                continue;
            }
        };
        let mut emit = |resp: &Response| {
            // Best-effort: if the client vanished, stop emitting for this request.
            let _ = write_line(&mut writer, resp);
        };
        handler.handle(req, &mut emit);
    }
    Ok(())
}

/// Read one newline-terminated line from `reader`, bounded to `MAX_LINE_LEN`
/// bytes so a client trickling data with no newline can't grow `buf` without
/// limit (see `MAX_LINE_LEN` docs). Mirrors `BufRead::lines()` semantics: no
/// trailing `\n`/`\r\n` in the returned string, a final unterminated line at
/// EOF is still returned, and clean EOF with zero bytes read yields `Ok(None)`.
/// Additionally errors with `ErrorKind::InvalidData` if a line's length would
/// exceed the bound before a newline is found (caller should then close the
/// connection), or if the line's bytes aren't valid UTF-8 (matching
/// `BufRead::lines()`'s own behavior for invalid UTF-8).
fn read_line_bounded<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    // Cap what a single `read_until` call can pull in at MAX_LINE_LEN content
    // bytes plus one for the delimiter itself, so a legitimate line of
    // exactly MAX_LINE_LEN bytes still finds its terminator within the cap,
    // while any longer line hits the cap before one is found. `Take` stops
    // handing out bytes once the limit is hit, so this can't allocate past
    // the bound no matter how the client paces its writes.
    let cap = MAX_LINE_LEN as u64 + 1;
    let n = reader.take(cap).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') && n as u64 >= cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("request line exceeds {MAX_LINE_LEN}-byte limit"),
        ));
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    let s = String::from_utf8(std::mem::take(buf)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })?;
    Ok(Some(s))
}

fn write_line(w: &mut impl Write, resp: &Response) -> io::Result<()> {
    let mut s = serde_json::to_string(resp)
        .unwrap_or_else(|_| serde_json::to_string(&Response::error("serialize failed")).unwrap());
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A real connected (client, server) stream pair over a throwaway
    /// temp-dir socket, so tests can drive `handle_conn` (which takes the
    /// concrete `transport::Stream`, not a generic `Read + Write`) with a
    /// hand-scripted peer instead of a full `serve()` accept loop.
    fn socket_pair() -> (Stream, Stream) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = transport::bind(&path).expect("bind test socket");
        let accept_thread = thread::spawn(move || listener.accept().expect("accept").0);
        let client = transport::connect(&path).expect("connect test socket");
        let server = accept_thread.join().expect("join accept thread");
        (client, server)
    }

    /// Records every `Request` it sees and replies `Done` to each.
    struct Recorder(Arc<Mutex<Vec<Request>>>);
    impl Handler for Recorder {
        fn handle(&self, req: Request, emit: &mut dyn FnMut(&Response)) {
            self.0.lock().unwrap().push(req);
            emit(&Response::Done);
        }
    }

    /// Finding: "no test proves malformed-JSON-then-resume behavior". A
    /// malformed line must produce an error response but NOT end the
    /// connection — the next, well-formed line must still be parsed and
    /// dispatched to the handler.
    #[test]
    fn handle_conn_recovers_after_a_malformed_json_line() {
        let (mut client, server) = socket_pair();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Recorder(Arc::clone(&seen));

        let server_thread = thread::spawn(move || handle_conn(server, &handler));

        client.write_all(b"this is not json\n").unwrap();
        client.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();

        // Consume `client` into the reader (rather than reading from a
        // clone) so there is only ever one client-side handle to the socket:
        // dropping this reader below is then sufficient to produce a true
        // EOF on the server side.
        let mut reader = BufReader::new(client);
        let mut line1 = String::new();
        reader
            .read_line(&mut line1)
            .expect("error response for bad line");
        assert!(
            line1.contains("bad request"),
            "malformed line should get an error response, got: {line1}"
        );

        let mut line2 = String::new();
        reader
            .read_line(&mut line2)
            .expect("response for the valid line that follows");
        let resp: Response = serde_json::from_str(line2.trim()).unwrap();
        assert!(
            matches!(resp, Response::Done),
            "the valid line after a malformed one must still be processed, got: {resp:?}"
        );

        drop(reader); // closes the last client-side handle ⇒ EOF, handle_conn's loop ends
        server_thread
            .join()
            .unwrap()
            .expect("handle_conn should return Ok on a clean client disconnect");
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "exactly the one valid request should have reached the handler"
        );
    }

    /// Finding: "invalid UTF-8 on the wire is silently swallowed (`let _ =
    /// e;`) rather than intentional". This pins down what actually happens
    /// today: `handle_conn` itself surfaces an `InvalidData` error (it does
    /// NOT silently ignore it) — it's `serve()`'s `let _ = e;` around the
    /// `handle_conn` call that discards it without telling the client or
    /// logging. If this test starts failing, that's a deliberate behavior
    /// change to invalid-UTF-8 handling, not an accidental regression.
    #[test]
    fn handle_conn_errors_on_invalid_utf8_instead_of_silently_ignoring_it() {
        let (mut client, server) = socket_pair();
        struct NoOp;
        impl Handler for NoOp {
            fn handle(&self, _req: Request, _emit: &mut dyn FnMut(&Response)) {}
        }

        let server_thread = thread::spawn(move || handle_conn(server, &NoOp));

        client.write_all(&[0xFF, 0xFE, b'\n']).unwrap();
        drop(client); // EOF after the bad bytes so handle_conn doesn't block forever

        let result = server_thread.join().unwrap();
        let err =
            result.expect_err("invalid UTF-8 on the wire must surface as an Err from handle_conn");
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "expected the InvalidData kind that BufRead::lines()/read_line_bounded use for bad UTF-8"
        );
    }

    /// Finding: "no max line length" ⇒ unbounded memory growth from a client
    /// that streams bytes with no newline. A line over `MAX_LINE_LEN` must be
    /// rejected (and the connection closed) instead of the buffer growing
    /// without bound.
    #[test]
    fn read_line_bounded_rejects_a_line_over_the_max_length() {
        let mut buf = Vec::new();
        let mut data = vec![b'a'; MAX_LINE_LEN + 1];
        data.push(b'\n');
        let mut reader = BufReader::new(&data[..]);
        let err = read_line_bounded(&mut reader, &mut buf)
            .expect_err("an over-limit line must error, not buffer without bound");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// A line at or under the limit is unaffected by the bound.
    #[test]
    fn read_line_bounded_accepts_a_line_at_the_max_length() {
        let mut buf = Vec::new();
        let mut data = vec![b'a'; MAX_LINE_LEN];
        data.push(b'\n');
        let mut reader = BufReader::new(&data[..]);
        let line = read_line_bounded(&mut reader, &mut buf)
            .expect("a line exactly at the limit must still be accepted")
            .expect("Some(line), not EOF");
        assert_eq!(line.len(), MAX_LINE_LEN);
    }
}
