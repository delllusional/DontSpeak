//! Blocking RPC server over the [`crate::transport`] byte stream. The engine
//! calls [`serve`] on a dedicated thread; it accepts connections, reads one
//! [`Request`] per line, and invokes the handler with an `emit` callback the
//! handler uses to write one-or-more [`Response`] lines back (supporting
//! streaming).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Arc;
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

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let h = Arc::clone(&handler);
                // One thread per connection; cheap (clients are the app + hooks).
                thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, h.as_ref()) {
                        // A client hanging up mid-write is normal; don't spam.
                        let _ = e;
                    }
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
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
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

fn write_line(w: &mut impl Write, resp: &Response) -> io::Result<()> {
    let mut s = serde_json::to_string(resp)
        .unwrap_or_else(|_| serde_json::to_string(&Response::error("serialize failed")).unwrap());
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}
