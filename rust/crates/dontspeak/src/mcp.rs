//! The stdio JSON-RPC 2.0 MCP server core: the request/response envelope helpers,
//! the [`dispatch`] router, the `initialize`/`tools`/`tools_call` handlers, and the
//! stderr logger. stdio is the only transport.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::tools;

/// MCP protocol revision we implement (date-based). We echo the client's version
/// when it matches; otherwise we answer with this one and let the client decide.
pub(crate) const PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const SERVER_NAME: &str = "DontSpeak";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the stdio MCP server loop: read newline-delimited JSON-RPC from stdin, route
/// each line through [`dispatch`], and write each response (one per line) to stdout.
/// Per the spec, stdout carries ONLY JSON-RPC messages; logging goes to stderr.
pub(crate) fn serve() {
    let sock = ds_config::Paths::resolve().map(|p| p.engine_sock);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            log("ignoring non-JSON line");
            continue;
        };
        if let Some(resp) = dispatch(&msg, sock.as_ref()) {
            let mut s = resp.to_string();
            s.push('\n');
            if out.write_all(s.as_bytes()).is_err() || out.flush().is_err() {
                break; // client went away
            }
        }
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

/// Log to STDERR only — stdout is reserved for JSON-RPC messages.
pub(crate) fn log(msg: &str) {
    eprintln!("dontspeak: {msg}");
}
