//! The sync WebSocket transport under the codex app-server subscriber — `tungstenite`'s
//! blocking client over any `Read + Write` stream: the unix control socket
//! (`$CODEX_HOME/app-server-control/app-server-control.sock`, WebSocket frames over UDS
//! behind a standard HTTP Upgrade) on unix, or a `TcpStream` for the `ws://` config
//! override. No tokio (the workspace's no-tokio stance); the read loop observes shutdown /
//! config flags via a socket **read timeout** — a timed-out read returns `Ok(None)` and the
//! caller does its housekeeping tick.

use std::io::{Read, Write};
use std::time::Duration;

use tungstenite::{Message, WebSocket};

use super::proto;

/// How long one blocking read waits before handing control back to the supervisor's
/// housekeeping tick (coalesce flushes, registry nudges, shutdown checks).
pub(crate) const READ_TICK: Duration = Duration::from_millis(150);

/// A stream we can arm with a read timeout — the one capability the read loop needs
/// beyond `Read + Write`. Implemented for the two production streams; the tests' fake
/// server uses the same unix impl.
pub(crate) trait ReadTimeout {
    fn set_read_timeout_opt(&self, d: Option<Duration>) -> std::io::Result<()>;
}

#[cfg(unix)]
impl ReadTimeout for std::os::unix::net::UnixStream {
    fn set_read_timeout_opt(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(d)
    }
}

impl ReadTimeout for std::net::TcpStream {
    fn set_read_timeout_opt(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(d)
    }
}

/// A connected, handshaken JSON-RPC-over-WebSocket client. Single-threaded by design:
/// the supervisor sends requests and drains incoming frames from one loop, correlating
/// responses by id (no blocking round-trips once attached).
pub(crate) struct WsClient<S: Read + Write> {
    ws: WebSocket<S>,
    next_id: i64,
}

impl<S: Read + Write + ReadTimeout> WsClient<S> {
    /// HTTP-Upgrade handshake over an already-connected stream, then arm the read timeout
    /// that paces the supervisor's loop. The URI's host part is nominal for the UDS case
    /// (the socket IS the address); tungstenite only needs a well-formed request.
    pub(crate) fn handshake(stream: S) -> Result<Self, String> {
        // Handshake blocking (no timeout yet) — it is a couple of round-trips.
        let mut attempt = tungstenite::client("ws://localhost/", stream);
        let ws = loop {
            match attempt {
                Ok((ws, _resp)) => break ws,
                Err(tungstenite::HandshakeError::Interrupted(mid)) => attempt = mid.handshake(),
                Err(tungstenite::HandshakeError::Failure(e)) => {
                    return Err(format!("websocket handshake: {e}"));
                }
            }
        };
        ws.get_ref()
            .set_read_timeout_opt(Some(READ_TICK))
            .map_err(|e| format!("set read timeout: {e}"))?;
        Ok(WsClient { ws, next_id: 0 })
    }
}

impl<S: Read + Write> WsClient<S> {
    /// Send one raw JSON-RPC text frame.
    pub(crate) fn send(&mut self, text: String) -> Result<(), String> {
        self.ws
            .send(Message::text(text))
            .map_err(|e| format!("websocket send: {e}"))
    }

    /// Allocate the next request id (per-connection monotone).
    pub(crate) fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// Read one text frame. `Ok(Some(text))` = a frame arrived; `Ok(None)` = the read
    /// timed out (housekeeping tick) or a non-text frame was absorbed (ping/pong/binary);
    /// `Err` = the connection is gone (the supervisor reconnects with backoff).
    pub(crate) fn read_text(&mut self) -> Result<Option<String>, String> {
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(t.as_str().to_string())),
            // Ping/pong are handled inside tungstenite; binary frames aren't ours.
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_)) => {
                Ok(None)
            }
            Ok(Message::Close(_)) => Err("websocket closed by peer".to_string()),
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(None) // the read-timeout tick — not an error
            }
            Err(e) => Err(format!("websocket read: {e}")),
        }
    }

    /// The `initialize` → response → `initialized` opening dance, bounded by `budget`.
    /// Notifications arriving mid-handshake (none are expected pre-subscribe) are dropped.
    pub(crate) fn initialize(&mut self, budget: Duration) -> Result<(), String> {
        let id = self.next_id();
        self.send(proto::initialize_request(id))?;
        let deadline = std::time::Instant::now() + budget;
        loop {
            if std::time::Instant::now() > deadline {
                return Err("initialize: no response within budget".to_string());
            }
            match self.read_text()? {
                None => continue,
                Some(text) => match proto::parse_incoming(&text) {
                    proto::Incoming::Response { id: rid, result } if rid == id => {
                        if result.is_none() {
                            return Err("initialize: server returned an error".to_string());
                        }
                        self.send(proto::initialized_notification())?;
                        return Ok(());
                    }
                    _ => continue,
                },
            }
        }
    }
}
