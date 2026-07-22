//! Blocking RPC server ([`serve`]): accept → one [`Request`] per line → handler `emit`s
//! [`Response`]s. Undecodable lines skip the handler; `on_bad_request` is the observability hook.

use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{io, thread};

use crate::protocol::{Request, Response};
use crate::transport::{self, Stream};

/// Per-conn timeouts (mirror `client::connect`); cover ~120s streaming dictate.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap concurrent conns — flood otherwise panics spawn and kills the accept loop.
const MAX_CONNECTIONS: usize = 64;

/// Cap one request line (no-newline trickle vs per-read timeout).
const MAX_LINE_LEN: usize = 1024 * 1024; // 1 MiB

const MAX_CMD_LEN: usize = 32;
const MAX_DETAIL_LEN: usize = 200;

/// Request → zero-or-more responses via `emit`. One conn per thread.
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

/// Bind + accept forever (stale-socket unlink on bind). Blocks — own thread.
///
/// `on_bad_request` required: hooks discard socket replies, so stale missing-`source`
/// would otherwise kill the voice loop silently. Bounded detail via `bad_request_detail`.
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
        let line = match line {
            RequestLine::Text(line) => line,
            RequestLine::Rejected(reason) => {
                on_bad_request(&format!("rejected request: {reason}"));
                write_line(
                    &mut writer,
                    &Response::error(format!("bad request: {reason}")),
                )?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                on_bad_request(&bad_request_detail(&line, &e));
                write_line(&mut writer, &Response::error(format!("bad request: {e}")))?;
                continue;
            }
        };
        let mut emit = |resp: &Response| {
            let _ = write_line(&mut writer, resp);
        };
        handler.handle(req, &mut emit);
    }
    Ok(())
}

/// Parse only `cmd` for partial / missing-`source` lines.
#[derive(Deserialize)]
struct CmdOnly {
    cmd: String,
}

/// Log-safe report: `rejected request (cmd=…): <serde error>` (sanitized cmd + controls).
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

enum RequestLine {
    Text(String),
    Rejected(String),
}

/// Bounded line read (see `MAX_LINE_LEN`). Rejected frames are drained through their
/// newline so the connection can process the next request without retaining payload bytes.
fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<Option<RequestLine>> {
    buf.clear();
    // +1 so a line of exactly MAX_LINE_LEN still finds its terminator within the Take cap.
    let cap = MAX_LINE_LEN as u64 + 1;
    let n = reader.take(cap).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') && n as u64 >= cap {
        discard_through_newline(reader)?;
        return Ok(Some(RequestLine::Rejected(format!(
            "request line exceeds {MAX_LINE_LEN}-byte limit"
        ))));
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(Some(match String::from_utf8(std::mem::take(buf)) {
        Ok(line) => RequestLine::Text(line),
        Err(_) => RequestLine::Rejected("request line is not valid UTF-8".into()),
    }))
}

fn discard_through_newline(reader: &mut impl BufRead) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(());
        }
    }
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

    fn assert_rejected_frame_recovers(frame: &[u8], expected_reason: &str) {
        let (mut client, server) = socket_pair();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = Recorder(Arc::clone(&seen));
        let (reported, on_bad) = reports();
        let server_thread = thread::spawn(move || handle_conn(server, &handler, &on_bad));

        client.write_all(frame).unwrap();
        client.write_all(b"{\"cmd\":\"ping\"}\n").unwrap();

        let mut reader = BufReader::new(client);
        let mut rejected = String::new();
        reader.read_line(&mut rejected).expect("rejection response");
        let response: Response = serde_json::from_str(rejected.trim()).unwrap();
        assert!(
            matches!(response, Response::Error { ref message } if message.contains(expected_reason)),
            "got: {response:?}"
        );

        let mut recovered = String::new();
        reader
            .read_line(&mut recovered)
            .expect("response after rejection");
        assert!(matches!(
            serde_json::from_str::<Response>(recovered.trim()).unwrap(),
            Response::Done
        ));
        drop(reader);
        server_thread.join().unwrap().expect("clean disconnect");

        assert_eq!(seen.lock().unwrap().len(), 1);
        let reports = reported.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains(expected_reason), "got: {}", reports[0]);
    }

    #[test]
    fn handle_conn_reports_invalid_utf8_and_recovers() {
        assert_rejected_frame_recovers(&[0xFF, 0xFE, b'\n'], "not valid UTF-8");
    }

    #[test]
    fn handle_conn_reports_an_oversized_line_and_recovers() {
        let mut frame = vec![b'a'; MAX_LINE_LEN + 1];
        frame.push(b'\n');
        assert_rejected_frame_recovers(&frame, "exceeds");
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
        let RequestLine::Text(line) = line else {
            panic!("a line at the limit must not be rejected");
        };
        assert_eq!(line.len(), MAX_LINE_LEN);
    }
}
