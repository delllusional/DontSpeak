//! Blocking RPC server over [`crate::transport`]. Engine calls [`serve`] on a dedicated
//! thread: accept, one [`Request`] per line, handler `emit`s [`Response`] lines (streaming).
//!
//! Undecodable lines never reach the handler. `on_bad_request` makes rejections observable
//! (engine logs WARN) without a logger dep in this crate.

use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{io, thread};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Per-conn timeouts (mirrors `client::connect`). Covers ~120s streaming dictate; kills
/// partial-line / wedged clients that would otherwise park a thread forever.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap concurrent connections/threads. Without it, a flood can panic `thread::spawn` past
/// the OS limit and kill the never-restarted accept loop (or abort under `panic = "abort"`).
const MAX_CONNECTIONS: usize = 64;

/// Cap one request line so a no-newline trickle can't grow memory unboundedly (defeats
/// per-read timeout). Legitimate payloads are KB-scale.
const MAX_LINE_LEN: usize = 1024 * 1024; // 1 MiB

/// Cap on `cmd` echoed into bad-request reports (hostile clients can put megabytes there).
const MAX_CMD_LEN: usize = 32;

/// Cap on the whole bad-request report. serde_json may quote field values (user/agent prose).
const MAX_DETAIL_LEN: usize = 200;

/// Parsed request → zero-or-more responses via `emit`. Thread-safe (one conn per thread).
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

/// Bind, accept forever, dispatch lines to `handler`. Stale-socket unlink on bind.
/// Blocks; run on its own thread.
///
/// `on_bad_request(detail)` runs on the conn thread for every undecodable line (bounded
/// report via `bad_request_detail`). Socket still gets `bad request: …`, but hooks discard
/// replies — without this, a stale CLI missing required `source` silently kills the voice
/// loop. Required (not optional no-op) so callers must decide how rejections are observed.
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
                        let _ = e; // hangup mid-write is normal
                    }
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

fn handle_conn<H: Handler, B: Fn(&str) + ?Sized>(
    stream: Stream,
    handler: &H,
    on_bad_request: &B,
) -> io::Result<()> {
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
                // Hooks discard the socket reply; callback is the only observability path.
                on_bad_request(&bad_request_detail(&line, &e));
                write_line(&mut writer, &Response::error(format!("bad request: {e}")))?;
                continue;
            }
        };
        let mut emit = |resp: &Response| {
            let _ = write_line(&mut writer, resp); // client may have vanished
        };
        handler.handle(req, &mut emit);
    }
    Ok(())
}

/// Parse only `cmd` so a partially-valid / missing-`source` line still names the command.
#[derive(Deserialize)]
struct CmdOnly {
    cmd: String,
}

/// Log-safe rejection report: `rejected request (cmd=…): <serde error>`. Never the raw
/// line. `cmd` sanitized to `[A-Za-z0-9_]` and [`MAX_CMD_LEN`]; finished string strips
/// controls (no forged log lines) and caps at [`MAX_DETAIL_LEN`].
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

/// Bounded line read (see `MAX_LINE_LEN`). Semantics like `BufRead::lines()`; over-limit
/// or invalid UTF-8 → `InvalidData`.
fn read_line_bounded<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    // +1 so a line of exactly MAX_LINE_LEN still finds its terminator within the Take cap.
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

    /// Throwaway socket pair for driving `handle_conn` without a full accept loop.
    fn socket_pair() -> (Stream, Stream) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = transport::bind(&path).expect("bind test socket");
        let accept_thread = thread::spawn(move || listener.accept().expect("accept").0);
        let client = transport::connect(&path).expect("connect test socket");
        let server = accept_thread.join().expect("join accept thread");
        (client, server)
    }

    struct Recorder(Arc<Mutex<Vec<Request>>>);
    impl Handler for Recorder {
        fn handle(&self, req: Request, emit: &mut dyn FnMut(&Response)) {
            self.0.lock().unwrap().push(req);
            emit(&Response::Done);
        }
    }

    /// Test double for `on_bad_request` (never the real activity log).
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

    /// Malformed line → error response; connection stays open for the next line.
    #[test]
    fn handle_conn_recovers_after_a_malformed_json_line() {
        let (mut client, server) = socket_pair();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Recorder(Arc::clone(&seen));
        let (_reports, on_bad) = reports();

        let server_thread = thread::spawn(move || handle_conn(server, &handler, &on_bad));

        client.write_all(b"this is not json\n").unwrap();
        client.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();

        // Single client handle so drop ⇒ true EOF on the server.
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

    /// `handle_conn` surfaces invalid UTF-8 as `InvalidData` (`serve` discards it).
    #[test]
    fn handle_conn_errors_on_invalid_utf8_instead_of_silently_ignoring_it() {
        let (mut client, server) = socket_pair();
        struct NoOp;
        impl Handler for NoOp {
            fn handle(&self, _req: Request, _emit: &mut dyn FnMut(&Response)) {}
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

    /// Over-limit line must error (not grow the buffer unboundedly).
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

    /// Stale CLI missing `source`: callback must fire and name cmd + missing field.
    #[test]
    fn handle_conn_reports_a_rejected_line_through_on_bad_request() {
        let (mut client, server) = socket_pair();
        let (seen, on_bad) = reports();
        struct NoOp;
        impl Handler for NoOp {
            fn handle(&self, _req: Request, _emit: &mut dyn FnMut(&Response)) {
                panic!("a line that fails to decode must never reach the handler");
            }
        }

        let server_thread = thread::spawn(move || handle_conn(server, &NoOp, &on_bad));
        client
            .write_all(br#"{"cmd":"greet_session","session":"sess-1"}"#)
            .unwrap();
        client.write_all(b"\n").unwrap();

        // Drain reply before hangup (Windows: unread close ⇒ reset, not clean EOF).
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

    /// Report names `cmd` but never payload prose (activity-log safety).
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

    /// Non-JSON → `cmd=?`; hostile/long/control-laden cmd must not forge log lines.
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
