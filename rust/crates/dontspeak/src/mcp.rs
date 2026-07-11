//! The stdio JSON-RPC 2.0 MCP server core: the request/response envelope helpers,
//! the [`dispatch`] router, the `initialize`/`tools`/`tools_call` handlers, and the
//! stderr logger. stdio is the only transport.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::engine_launch::ensure_engine;
use crate::tools;

/// MCP protocol revision we implement (date-based). We echo the client's version
/// when it matches; otherwise we answer with this one and let the client decide.
pub(crate) const PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const SERVER_NAME: &str = "DontSpeak";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the stdio MCP server loop. A real MCP client (Claude Code, the WinUI/GTK/macOS
/// host, …) always sends at least one JSON-RPC message before EOF. If stdin hits EOF
/// having handled zero, this wasn't a real client — most likely `dontspeak`/
/// `dontspeak.exe` invoked directly by a human (e.g. double-clicked) rather than
/// spawned by an MCP client — so fall back to launching the resident host app, same as
/// a real `tools/call` would (see [`ensure_engine`]). Without this, a stray direct
/// launch silently exits 0 with no host app started and no log line — GH issue #20.
pub(crate) fn serve() {
    let sock = ds_config::Paths::resolve().map(|p| p.engine_sock);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_on(stdin.lock(), stdout.lock(), sock.as_ref(), || {
        match sock.as_ref() {
            Some(sock) => {
                log(
                    "no MCP client sent a request before EOF; launching the resident host \
                     app as a standalone-run fallback",
                );
                ensure_engine(sock);
            }
            None => log("cannot resolve engine socket path; skipping standalone-run fallback"),
        }
    });
}

/// The stdio loop's actual logic, generic over reader/writer so it's unit-testable
/// without real stdio. Calls `on_no_requests` once, after EOF, if the reader never
/// produced a single parseable JSON-RPC message (blank lines and unparseable lines
/// don't count; a bare no-id notification DOES count — it's still real client traffic).
fn serve_on<R: BufRead, W: Write>(
    reader: R,
    mut out: W,
    sock: Option<&PathBuf>,
    on_no_requests: impl FnOnce(),
) {
    let mut handled_any = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            log("ignoring non-JSON line");
            continue;
        };
        handled_any = true;
        if let Some(resp) = dispatch(&msg, sock) {
            let mut s = resp.to_string();
            s.push('\n');
            if out.write_all(s.as_bytes()).is_err() || out.flush().is_err() {
                break; // client went away
            }
        }
    }
    if !handled_any {
        on_no_requests();
    }
}

/// Route one JSON-RPC message to its handler, returning the response envelope (or
/// `None` for a notification, which gets no reply). The stdio loop calls this with
/// the `sock` to the engine.
pub(crate) fn dispatch(msg: &Value, sock: Option<&PathBuf>) -> Option<Value> {
    // A message with no "id" is a notification — never respond.
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "initialize" => Some(ok(id, initialize(msg))),
        "notifications/initialized" => None, // notification: no reply
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, json!({ "tools": tools() }))),
        "tools/call" => Some(tools::tools_call(id, msg, sock)),
        // Unknown method: respond with an error only if it had an id.
        _ => id
            .as_ref()
            .map(|_| err(id.clone(), -32601, &format!("method not found: {method}"))),
    }
}

// ── JSON-RPC envelope helpers ────────────────────────────────────────────────

pub(crate) fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tools/call SUCCESS result with a single text content block. `is_error=true`
/// surfaces a tool-level failure the model can see/retry (distinct from a
/// protocol error).
pub(crate) fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

// ── MCP methods ──────────────────────────────────────────────────────────────

fn initialize(msg: &Value) -> Value {
    // Echo the client's protocolVersion if we support it; else advertise ours.
    let client_ver = msg
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str());
    let version = match client_ver {
        Some(v) if v == PROTOCOL_VERSION => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
    })
}

/// The static tool catalog (JSON Schema 2020-12 input schemas). Lives in the
/// shared `ds-tools` crate so the app's FFI (`ds_tools_json`) exposes the
/// EXACT same list to the Tools window — the catalog can never drift from what
/// Claude sees here.
fn tools() -> Value {
    ds_tools::catalog()
}

/// Log to STDERR (stdout is reserved for JSON-RPC messages) AND persist to the unified
/// activity log (source `mcp`) via the `log` facade.
pub(crate) fn log(msg: &str) {
    eprintln!("{msg}");
    log::info!(target: "mcp", "{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── serve_on ─────────────────────────────────────────────────────────────
    // Pure `io::Cursor` fixtures only — no real stdio, socket, or process spawn, so
    // these never touch the real engine socket or launch a real host app (GH #20's
    // fallback logic, in isolation from `ensure_engine`'s own real-world side effects).

    #[test]
    fn serve_on_falls_back_when_no_request_ever_arrives() {
        let reader = std::io::Cursor::new(&b""[..]); // immediate EOF — no console/pipe attached
        let mut out = Vec::new();
        let mut fell_back = false;
        serve_on(reader, &mut out, None, || fell_back = true);
        assert!(fell_back, "empty stdin must trigger the fallback");
    }

    #[test]
    fn serve_on_falls_back_when_only_blank_or_unparseable_lines_arrive() {
        let reader = std::io::Cursor::new(&b"\n   \nnot json\n"[..]);
        let mut out = Vec::new();
        let mut fell_back = false;
        serve_on(reader, &mut out, None, || fell_back = true);
        assert!(
            fell_back,
            "blank/unparseable-only input is not real client traffic"
        );
    }

    #[test]
    fn serve_on_skips_fallback_once_a_real_request_arrives() {
        let reader = std::io::Cursor::new(&br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#[..]);
        let mut out = Vec::new();
        let mut fell_back = false;
        serve_on(reader, &mut out, None, || fell_back = true);
        assert!(!fell_back, "a real request must suppress the fallback");
        assert!(!out.is_empty(), "the ping response was actually written");
    }

    #[test]
    fn serve_on_skips_fallback_for_a_bare_notification() {
        // notifications/initialized has no id and gets no reply, but it IS real
        // traffic from a real client — must not trigger the standalone fallback.
        let reader =
            std::io::Cursor::new(&br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#[..]);
        let mut out = Vec::new();
        let mut fell_back = false;
        serve_on(reader, &mut out, None, || fell_back = true);
        assert!(!fell_back, "a notification is still real client traffic");
    }

    // ── dispatch ─────────────────────────────────────────────────────────────

    #[test]
    fn dispatch_initialize_routes_to_initialize() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": PROTOCOL_VERSION },
        });
        let resp = dispatch(&msg, None).expect("initialize gets a reply");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn dispatch_notifications_initialized_returns_none_even_with_id() {
        // Per spec this is a notification and gets no reply — even if the (malformed)
        // request happens to carry an "id".
        let msg = json!({ "jsonrpc": "2.0", "id": 7, "method": "notifications/initialized" });
        assert!(dispatch(&msg, None).is_none());
    }

    #[test]
    fn dispatch_ping_replies_with_empty_result() {
        let msg = json!({ "jsonrpc": "2.0", "id": "abc", "method": "ping" });
        let resp = dispatch(&msg, None).expect("ping gets a reply");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], "abc");
        assert_eq!(resp["result"], json!({}));
    }

    #[test]
    fn dispatch_tools_list_matches_catalog() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let resp = dispatch(&msg, None).expect("tools/list gets a reply");
        let tools = resp["result"]["tools"]
            .as_array()
            .expect("result.tools is an array");
        let catalog = ds_tools::catalog();
        let catalog = catalog.as_array().expect("catalog is an array");
        assert_eq!(tools.len(), catalog.len());
        assert_eq!(tools, catalog);
    }

    #[test]
    fn dispatch_tools_call_delegates_to_tools_call() {
        // Smoke test only: confirm dispatch wires "tools/call" through to
        // `tools::tools_call` — its own logic is covered in tools.rs's tests. Use an
        // unknown tool name so this stays a pure dispatch check with no engine socket,
        // filesystem, or process spawning involved.
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "definitely_not_a_real_tool", "arguments": {} },
        });
        let resp = dispatch(&msg, None).expect("tools/call gets a reply");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(text.contains("definitely_not_a_real_tool"));
    }

    #[test]
    fn dispatch_unknown_method_with_id_is_method_not_found_error() {
        let msg = json!({ "jsonrpc": "2.0", "id": 42, "method": "bogus/method" });
        let resp = dispatch(&msg, None).expect("unknown method with an id gets a reply");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["error"]["code"], -32601);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("bogus/method")
        );
    }

    #[test]
    fn dispatch_unknown_method_without_id_returns_none() {
        // Mirrors a notification: no id means no reply, even for an unrecognized method.
        let msg = json!({ "jsonrpc": "2.0", "method": "bogus/method" });
        assert!(dispatch(&msg, None).is_none());
    }

    // ── initialize ───────────────────────────────────────────────────────────

    #[test]
    fn initialize_echoes_matching_protocol_version() {
        let msg = json!({ "params": { "protocolVersion": PROTOCOL_VERSION } });
        let result = initialize(&msg);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn initialize_falls_back_on_mismatched_protocol_version() {
        let msg = json!({ "params": { "protocolVersion": "1999-01-01" } });
        let result = initialize(&msg);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn initialize_falls_back_when_protocol_version_missing() {
        let msg = json!({ "params": {} });
        let result = initialize(&msg);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);

        let msg_no_params = json!({});
        let result = initialize(&msg_no_params);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    }

    // ── envelope helpers ─────────────────────────────────────────────────────

    #[test]
    fn ok_builds_a_result_envelope_with_id_passthrough() {
        let resp = ok(Some(json!(5)), json!({ "x": 1 }));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 5);
        assert_eq!(resp["result"], json!({ "x": 1 }));
    }

    #[test]
    fn ok_passes_through_a_null_id() {
        let resp = ok(Some(Value::Null), json!({}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp["id"].is_null());
    }

    #[test]
    fn err_builds_an_error_envelope_with_id_passthrough() {
        let resp = err(Some(json!("req-1")), -32601, "method not found: foo");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], "req-1");
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "method not found: foo");
    }

    #[test]
    fn err_passes_through_a_null_id() {
        let resp = err(Some(Value::Null), -32601, "method not found: foo");
        assert!(resp["id"].is_null());
    }

    #[test]
    fn tool_result_marks_success_not_an_error() {
        let r = tool_result("all good".into(), false);
        assert_eq!(r["isError"], false);
        assert_eq!(r["content"][0]["type"], "text");
        assert_eq!(r["content"][0]["text"], "all good");
    }

    #[test]
    fn tool_result_marks_failure_as_an_error() {
        let r = tool_result("boom".into(), true);
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "boom");
    }
}
