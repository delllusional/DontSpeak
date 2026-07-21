//! Blocking RPC server over [`crate::transport`]. Engine calls [`serve`] on a
//! dedicated thread: accept, one [`Request`] per line, owned [`Conn`] for fallible
//! writes / [`Conn::recv_deadline`] acks / [`HandleOutcome::TookOver`] subscriptions.
//!
//! Undecodable lines never reach the handler. `on_bad_request` makes rejections
//! observable (engine logs WARN) without a logger dep in this crate.

use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::{io, thread};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Per-conn timeouts (mirrors `client::connect`). Covers ~120s streaming dictate;
/// kills partial-line / wedged clients that would otherwise park a thread forever.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap concurrent connections/threads. Without it, a flood can panic `thread::spawn`
/// past the OS limit and kill the never-restarted accept loop (or abort under
/// `panic = "abort"`). Taken-over conns ([`HandleOutcome::TookOver`]) release their
/// slot immediately; owners bound their own population (≤1 frontend per app tag).
const MAX_CONNECTIONS: usize = 64;

/// Cap one request line so a no-newline trickle can't grow memory unboundedly
/// (defeats per-read timeout). Legitimate payloads are KB-scale.
const MAX_LINE_LEN: usize = 1024 * 1024; // 1 MiB

/// Cap on `cmd` echoed into bad-request reports (hostile clients can put megabytes there).
const MAX_CMD_LEN: usize = 32;

/// Cap on the whole bad-request report. serde_json may quote field values (user/agent prose).
const MAX_DETAIL_LEN: usize = 200;

/// Owned connection per request (replaces fallible-blind `emit` closure). Fallible
/// [`Self::send`], bounded [`Self::recv_deadline`] for deliver→ack, [`HandleOutcome::TookOver`]
/// for long-lived subscriptions without leaking accept-loop slots.
pub struct Conn {
    reader: BufReader<Stream>,
    writer: Stream,
    write_timeout: Duration,
    line_buf: Vec<u8>,
}

impl Conn {
    /// Apply server timeouts (best-effort) and split read/write. Public for tests/registry.
    pub fn new(stream: Stream) -> io::Result<Self> {
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let writer = stream.try_clone()?;
        Ok(Conn {
            reader: BufReader::new(stream),
            writer,
            write_timeout: WRITE_TIMEOUT,
            line_buf: Vec::new(),
        })
    }

    /// Write one response line. Fallible so streamers notice client hang-up.
    pub fn send(&mut self, resp: &Response) -> io::Result<()> {
        write_line(&mut self.writer, resp, self.write_timeout)
    }

    /// Shrink write timeout (subscriptions call at takeover). One overall deadline
    /// across partial writes — not a fresh timeout per OS write.
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
        let _ = self.writer.set_write_timeout(Some(timeout));
    }

    /// Force-close this socket from another thread without taking `Conn`'s external lock
    /// (so eviction can interrupt a blocked send/recv rather than queue behind it).
    pub fn shutdown_handle(&self) -> io::Result<ShutdownHandle> {
        Ok(ShutdownHandle(self.writer.try_clone()?))
    }

    /// Wait up to `deadline` for the next request line (deliver→ack). `Ok(Some)` /
    /// `Ok(None)` EOF / `Err` timeout|malformed|transport. Restores `READ_TIMEOUT`.
    /// After timeout, treat socket as suspect (partial line / Win indeterminate).
    pub fn recv_deadline(&mut self, deadline: Duration) -> io::Result<Option<Request>> {
        // A zero timeout means "infinite" to setsockopt — clamp so an
        // already-elapsed deadline stays a bounded wait.
        let d = deadline.max(Duration::from_millis(1));
        let _ = self.reader.get_ref().set_read_timeout(Some(d));
        let result = match read_line_bounded(&mut self.reader, &mut self.line_buf) {
            Ok(None) => Ok(None),
            Ok(Some(line)) => serde_json::from_str::<Request>(&line)
                .map(Some)
                .map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad request: {e}"))
                }),
            Err(e) => Err(e),
        };
        let _ = self.reader.get_ref().set_read_timeout(Some(READ_TIMEOUT));
        result
    }
}

/// Lock-free force-close for a `Conn` from another thread (see [`Conn::shutdown_handle`]).
pub struct ShutdownHandle(Stream);

impl ShutdownHandle {
    /// Best-effort bidirectional close (idempotent).
    pub fn shutdown(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// What the [`Handler`] did with the connection.
pub enum HandleOutcome {
    /// Done; return `Conn` to the read loop (one-shot / finite stream).
    Done(Conn),
    /// Handler owns `Conn` (e.g. frontend subscription). Read loop ends; accept slot released.
    TookOver,
}

/// Parsed request + owned conn → replies via [`Conn::send`]. `Err` closes the conn.
/// Thread-safe (one conn per thread).
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request, conn: Conn) -> io::Result<HandleOutcome>;
}

impl<F> Handler for F
where
    F: Fn(Request, Conn) -> io::Result<HandleOutcome> + Send + Sync + 'static,
{
    fn handle(&self, req: Request, conn: Conn) -> io::Result<HandleOutcome> {
        self(req, conn)
    }
}

/// Bind, accept forever, dispatch lines to `handler`. Stale-socket unlink on bind.
/// Blocks; run on its own thread.
///
/// `on_bad_request(detail)` runs on the conn thread for every undecodable line
/// (bounded report via `bad_request_detail`). Socket still gets `bad request: …`,
/// but hooks discard replies — without this, a stale CLI missing required `source`
/// silently kills the voice loop. Required (not optional no-op) so callers must
/// decide how rejections are observed.
pub fn serve<H: Handler, B>(sock_path: &Path, handler: H, on_bad_request: B) -> io::Result<()>
where
    B: Fn(&str) + Send + Sync + 'static,
{
    let listener = transport::bind(sock_path)?;
    let handler = Arc::new(handler);
    let on_bad_request = Arc::new(on_bad_request);
    let active_conns = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if active_conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    // Cap: refuse rather than panic spawn past OS thread limit.
                    active_conns.fetch_sub(1, Ordering::SeqCst);
                    drop(stream);
                    continue;
                }
                let h = Arc::clone(&handler);
                let bad = Arc::clone(&on_bad_request);
                let active_conns = Arc::clone(&active_conns);
                thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, h.as_ref(), bad.as_ref()) {
                        let _ = e; // hang-up mid-write is normal
                    }
                    // TookOver returns promptly so taken-over conns free their slot.
                    active_conns.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) => {
                if e.kind() != std::io::ErrorKind::Interrupted {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                continue;
            }
        }
    }
    Ok(())
}

/// Per-conn read loop until EOF, fatal error, or [`HandleOutcome::TookOver`].
fn handle_conn<H: Handler, B: Fn(&str) + ?Sized>(
    stream: Stream,
    handler: &H,
    on_bad_request: &B,
) -> io::Result<()> {
    let mut conn = Conn::new(stream)?;
    loop {
        let Some(line) = read_line_bounded(&mut conn.reader, &mut conn.line_buf)? else {
            return Ok(()); // clean EOF
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                // Hooks discard the socket reply; callback is the only visibility path.
                on_bad_request(&bad_request_detail(&line, &e));
                conn.send(&Response::error(format!("bad request: {e}")))?;
                continue;
            }
        };
        match handler.handle(req, conn)? {
            HandleOutcome::Done(c) => conn = c,
            HandleOutcome::TookOver => return Ok(()),
        }
    }
}

/// Just the `cmd` tag, for naming a request that failed to decode as a full [`Request`].
/// Every other field is ignored, so this still parses a line whose payload is wrong,
/// incomplete, or (the case that matters) missing its `source`.
#[derive(Deserialize)]
struct CmdOnly {
    cmd: String,
}

/// A log-safe description of a line that failed to decode: the serde error, prefixed with
/// WHICH command was rejected when the line is at least well-formed JSON with a `cmd`
/// (`?` when it isn't).
///
/// Never the raw line, and never unbounded. A `Speak`/`SpeakNarration` payload is user or
/// agent prose, and the whole point of this report is that it lands in the activity log —
/// so the only thing taken from the line itself is its `cmd` tag, sanitized to the
/// `[A-Za-z0-9_]` a real tag is made of and capped at [`MAX_CMD_LEN`]. serde_json's own
/// message can quote a field value too (`unknown variant \`…\``, `invalid type: string
/// "…"`), so the FINISHED string is also stripped of control characters (a newline here
/// would forge a second activity-log line) and capped at [`MAX_DETAIL_LEN`].
/// Costs one extra parse, on the error path only.
fn bad_request_detail(line: &str, err: &serde_json::Error) -> String {
    let cmd = serde_json::from_str::<CmdOnly>(line)
        .map(|c| {
            sanitize(&c.cmd, MAX_CMD_LEN, |ch| {
                ch.is_ascii_alphanumeric() || ch == '_'
            })
        })
        .unwrap_or_else(|_| "?".to_string());
    sanitize(
        &format!("rejected request (cmd={cmd}): {err}"),
        MAX_DETAIL_LEN,
        |ch| !ch.is_control(),
    )
}

/// `s` capped at `max` chars, with every char `keep` rejects replaced by `?`, and an
/// ellipsis appended if anything was cut.
fn sanitize(s: &str, max: usize, keep: impl Fn(char) -> bool) -> String {
    let mut out: String = s
        .chars()
        .take(max)
        .map(|ch| if keep(ch) { ch } else { '?' })
        .collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
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

fn write_line(w: &mut Stream, resp: &Response, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    let mut s = serde_json::to_string(resp)
        .unwrap_or_else(|_| serde_json::to_string(&Response::error("serialize failed")).unwrap());
    s.push('\n');
    write_bytes_deadline(w, s.as_bytes(), started, timeout)
}

fn write_bytes_deadline(
    w: &mut Stream,
    mut remaining: &[u8],
    started: Instant,
    timeout: Duration,
) -> io::Result<()> {
    while !remaining.is_empty() {
        let left = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "response write deadline elapsed")
        })?;
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response write deadline elapsed",
            ));
        }
        // Socket write timeouts are per syscall. Replacing the timeout with the
        // shrinking remainder makes a sequence of partial writes share one bound.
        let _ = w.set_write_timeout(Some(left));
        match w.write(remaining) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write response line",
                ));
            }
            Ok(written) => remaining = &remaining[written..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let left = match timeout.checked_sub(started.elapsed()) {
        Some(left) if !left.is_zero() => left,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response write deadline elapsed",
            ));
        }
    };
    let _ = w.set_write_timeout(Some(left));
    let result = w.flush();
    // Keep the configured bound for the next response; the last partial write
    // may have left the socket with only a tiny remainder.
    let _ = w.set_write_timeout(Some(timeout));
    result
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
        fn handle(&self, req: Request, mut conn: Conn) -> io::Result<HandleOutcome> {
            self.0.lock().unwrap().push(req);
            conn.send(&Response::Done)?;
            Ok(HandleOutcome::Done(conn))
        }
    }

    /// The `on_bad_request` sink a test hands to `handle_conn`: records each report so a
    /// test can assert BOTH that the rejection was announced and what it did (not) carry.
    /// Stands in for the engine's real sink, which is a `ds_log::log_from` WARN — no test
    /// here (or anywhere) may reach a logger that resolves the developer's real log file.
    fn reports() -> (
        Arc<Mutex<Vec<String>>>,
        impl Fn(&str) + Send + Sync + 'static,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        (seen, move |detail: &str| {
            sink.lock().unwrap().push(detail.to_string())
        })
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
        let (_reports, on_bad) = reports();

        let server_thread = thread::spawn(move || handle_conn(server, &handler, &on_bad));

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
            fn handle(&self, _req: Request, conn: Conn) -> io::Result<HandleOutcome> {
                Ok(HandleOutcome::Done(conn))
            }
        }

        let (_reports, on_bad) = reports();
        let server_thread = thread::spawn(move || handle_conn(server, &NoOp, &on_bad));

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

    /// Contract test (frontend subscriptions): a handler that returns
    /// `TookOver` must end the per-connection read loop IMMEDIATELY — that is
    /// what releases the accept-loop slot (`serve` decrements `active_conns`
    /// right after `handle_conn` returns, on the same thread) — while the
    /// moved-out `Conn` stays fully usable for pushing further lines to the
    /// still-connected client. Without this, every frontend (re)subscribe
    /// would leak a blocked thread + connection slot until `MAX_CONNECTIONS`
    /// bricks the daemon's IPC.
    #[test]
    fn takeover_ends_the_read_loop_immediately_and_the_conn_stays_usable() {
        let (mut client, server) = socket_pair();
        struct TakeOver(Mutex<Option<std::sync::mpsc::Sender<Conn>>>);
        impl Handler for TakeOver {
            fn handle(&self, _req: Request, conn: Conn) -> io::Result<HandleOutcome> {
                self.0.lock().unwrap().take().unwrap().send(conn).unwrap();
                Ok(HandleOutcome::TookOver)
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let handler = TakeOver(Mutex::new(Some(tx)));
        let (_reports, on_bad) = reports();

        client.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();
        let server_thread = thread::spawn(move || handle_conn(server, &handler, &on_bad));

        // The loop must end WITHOUT the client hanging up (the client stays
        // connected throughout this test) — a `Done`-style loop would sit in
        // its blocking read here instead of returning.
        server_thread
            .join()
            .unwrap()
            .expect("a takeover is a clean outcome, not an error");

        // The taken-over Conn keeps working: its owner can still push lines.
        let mut conn = rx.recv().expect("handler forwarded the Conn");
        conn.send(&Response::Pong)
            .expect("taken-over conn still writable");
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).expect("pushed line arrives");
        let resp: Response = serde_json::from_str(line.trim()).unwrap();
        assert!(matches!(resp, Response::Pong), "got: {resp:?}");
    }

    /// The refactor's core promise: a write to a hung-up client SURFACES as an
    /// error (the old emit closure discarded it with `let _ =`), so a
    /// streaming handler / subscription owner can abort instead of writing
    /// into the void forever.
    #[test]
    fn send_surfaces_a_write_error_after_the_client_disconnects() {
        let (client, server) = socket_pair();
        let mut conn = Conn::new(server).expect("conn");
        drop(client);

        // The disconnect may take a write or two to surface (platform-
        // dependent buffering) — but it MUST surface, bounded.
        let mut observed = false;
        for _ in 0..100 {
            if conn.send(&Response::Pong).is_err() {
                observed = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            observed,
            "a write to a disconnected client must eventually return Err"
        );
    }

    /// `recv_deadline` happy path: a line already on the wire parses into its
    /// `Request` well within the deadline.
    #[test]
    fn recv_deadline_returns_a_pending_request() {
        let (mut client, server) = socket_pair();
        let mut conn = Conn::new(server).expect("conn");
        client.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();
        match conn.recv_deadline(Duration::from_secs(5)) {
            Ok(Some(Request::Ping)) => {}
            other => panic!("expected Ok(Some(Ping)), got {other:?}"),
        }
    }

    /// `recv_deadline` with a silent peer: a timeout-flavored error, promptly
    /// (the deadline, not the server's 120s `READ_TIMEOUT`).
    #[test]
    fn recv_deadline_times_out_on_a_silent_peer() {
        let (client, server) = socket_pair();
        let mut conn = Conn::new(server).expect("conn");
        let started = std::time::Instant::now();
        let err = conn
            .recv_deadline(Duration::from_millis(150))
            .expect_err("a silent peer must time out, not hang");
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "expected a timeout-flavored error, got: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the short deadline should fire quickly, took {:?}",
            started.elapsed()
        );
        drop(client); // keep the peer alive until here so this is a real timeout, not EOF
    }

    /// `recv_deadline` when the client hangs up: clean EOF is `Ok(None)`, not
    /// an error — the caller distinguishes "gone" from "slow".
    #[test]
    fn recv_deadline_reports_clean_eof_as_none() {
        let (client, server) = socket_pair();
        let mut conn = Conn::new(server).expect("conn");
        drop(client);
        match conn.recv_deadline(Duration::from_millis(500)) {
            Ok(None) => {}
            other => panic!("expected Ok(None) on EOF, got {other:?}"),
        }
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

    /// THE STALE-CLI SCENARIO, pinned. A `dontspeak` CLI built before `source` became
    /// required sends `{"cmd":"greet_session","session":"…"}`; the engine rejects it, and the
    /// hook — like every hook call site — throws the reply away and exits 0. If the rejection
    /// isn't announced through `on_bad_request`, the entire voice loop dies with zero
    /// diagnostics anywhere. So: a bad line MUST invoke the callback, and the report MUST name
    /// both the rejected `cmd` and the missing field.
    #[test]
    fn handle_conn_reports_a_rejected_line_through_on_bad_request() {
        let (mut client, server) = socket_pair();
        let (seen, on_bad) = reports();
        struct NoOp;
        impl Handler for NoOp {
            fn handle(&self, _req: Request, _conn: Conn) -> io::Result<HandleOutcome> {
                panic!("a line that fails to decode must never reach the handler");
            }
        }

        let server_thread = thread::spawn(move || handle_conn(server, &NoOp, &on_bad));
        client
            .write_all(br#"{"cmd":"greet_session","session":"sess-1"}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();

        // Drain the error reply BEFORE hanging up (as the real client does, even though it
        // then throws it away): closing on unread data resets the connection on Windows, and
        // the server would see that reset rather than the clean EOF this test is asserting.
        let mut reader = BufReader::new(client);
        let mut reply = String::new();
        reader.read_line(&mut reply).expect("error response");
        assert!(reply.contains("bad request"), "got: {reply}");

        drop(reader); // EOF ⇒ handle_conn's loop ends
        server_thread.join().unwrap().expect("clean disconnect");
        let reports = seen.lock().unwrap();
        assert_eq!(reports.len(), 1, "the one bad line must be reported once");
        let detail = &reports[0];
        assert!(
            detail.contains("cmd=greet_session"),
            "the report must name WHICH command was rejected, got: {detail}"
        );
        assert!(
            detail.contains("source"),
            "the report must name the missing field, got: {detail}"
        );
    }

    /// The report goes into the activity log, so it must never carry the request's payload —
    /// a `speak` line's `text` is user/agent prose. Only the `cmd` tag and the serde error
    /// leave this module, and the whole thing is length-capped.
    #[test]
    fn bad_request_detail_carries_the_cmd_but_never_the_payload() {
        let line = r#"{"cmd":"speak","text":"my private prompt text"}"#; // no `source` ⇒ rejected
        let err = serde_json::from_str::<Request>(line).expect_err("must not decode");
        let detail = bad_request_detail(line, &err);
        assert!(detail.contains("cmd=speak"), "got: {detail}");
        assert!(detail.contains("source"), "got: {detail}");
        assert!(
            !detail.contains("my private prompt text"),
            "the payload must never reach the log, got: {detail}"
        );
        assert!(detail.chars().count() <= MAX_DETAIL_LEN + 1); // +1 for the '…'
    }

    /// A line that isn't even JSON has no `cmd` to name, and a hostile one could put a
    /// megabyte of prose (or a newline-forged fake log line) where the tag belongs. Neither
    /// may escape into the report.
    #[test]
    fn bad_request_detail_bounds_and_sanitizes_a_hostile_cmd() {
        let err = serde_json::from_str::<Request>("this is not json").expect_err("must not decode");
        assert!(
            bad_request_detail("this is not json", &err).contains("cmd=?"),
            "a non-JSON line has no cmd to report"
        );

        let hostile = format!(r#"{{"cmd":"{}"}}"#, "a\nb".repeat(200));
        let err = serde_json::from_str::<Request>(&hostile).expect_err("must not decode");
        let detail = bad_request_detail(&hostile, &err);
        assert!(!detail.contains('\n'), "no newline forging, got: {detail}");
        assert!(detail.chars().count() <= MAX_DETAIL_LEN + 1);
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

    /// A `ShutdownHandle` force-closes the socket independently of whatever
    /// holds (or is blocked inside) the `Conn` itself — a registry uses this to
    /// invalidate a subscriber it's evicting without taking that subscriber's
    /// own lock (see `ds-ipc`'s crate-level users: `dontspeakd`'s
    /// `FrontendRegistry`). Both directions close: the peer reads EOF, and a
    /// further local write fails.
    #[test]
    fn shutdown_handle_closes_the_socket_from_outside_conns_own_lock() {
        let (client, server) = socket_pair();
        let mut conn = Conn::new(server).expect("wrap server stream");
        let handle = conn.shutdown_handle().expect("clone a shutdown handle");

        handle.shutdown();

        assert!(
            conn.send(&Response::Done).is_err(),
            "a write on the shut-down connection must fail, not silently succeed"
        );

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        assert_eq!(
            reader.read_line(&mut line).unwrap(),
            0,
            "the peer must see a clean EOF once shutdown runs"
        );
    }

    /// `set_write_timeout` is what lets a long-lived subscription shrink the
    /// constructor's generous 5s RPC bound down to something close to its own
    /// ack-wait deadline. Pin that it actually changes the OS-level timeout: a
    /// write that would otherwise block for the full default blocks for only
    /// about as long as the shortened bound.
    ///
    /// `#[cfg(unix)]`: this needs a send buffer small enough that an unread
    /// 32 MiB write actually blocks. Linux/macOS's default `AF_UNIX` buffer is
    /// comfortably under that; `uds_windows`' Windows backing socket does not
    /// reliably block at any practical test payload size, so there is nothing
    /// deterministic to assert there — CI (`ci.yml`) is Linux-only anyway.
    #[cfg(unix)]
    #[test]
    fn set_write_timeout_bounds_a_stalled_write() {
        let (client, mut server) = socket_pair();

        // Prebuild the bytes so this measures only socket progress. A per-syscall
        // timeout can take several multiples of the bound as partial writes land;
        // the shrinking deadline must cap the entire sequence.
        let bytes = vec![b'x'; 32 * 1024 * 1024];
        let started = std::time::Instant::now();
        let result = write_bytes_deadline(&mut server, &bytes, started, Duration::from_millis(200));
        assert!(result.is_err(), "a stalled write must time out, not hang");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the total write deadline must apply across partial writes, took {:?}",
            started.elapsed()
        );
        drop(client);
    }
}
