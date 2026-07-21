//! Strict MCP 2025-11-25 / JSON-RPC 2.0 stdio boundary. Framing, envelope, and
//! lifecycle failures are protocol errors; only failures from a valid tool
//! invocation become `CallToolResult.isError`. Bounded worker pool so
//! cancellation stays observable while `listen` blocks on streaming IPC.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use ds_config::{ClientSource, client_from_mcp_name};
use serde_json::{Map, Value, json};

use crate::engine_launch::ensure_engine;
use crate::tools;

pub(crate) const PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const SERVER_NAME: &str = "DontSpeak";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_STDIN_FRAME_BYTES: usize = 1024 * 1024;
const MAX_IN_FLIGHT_TOOL_CALLS: usize = 8;

type Executor = dyn Fn(Option<Value>, &Value, Option<&PathBuf>, ClientSource, Arc<AtomicBool>) -> Value
    + Send
    + Sync;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RequestId {
    String(String),
    Integer(String),
}

impl RequestId {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::String(value) => Some(Self::String(value.clone())),
            Value::Number(value) if value.is_i64() || value.is_u64() => {
                Some(Self::Integer(value.to_string()))
            }
            _ => None,
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::String(value) => json!(value),
            Self::Integer(value) => serde_json::from_str(value).expect("stored JSON integer"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Lifecycle {
    #[default]
    Uninitialized,
    Initialized,
}

#[derive(Default)]
struct Session {
    lifecycle: Lifecycle,
    initialized_notification: bool,
    client: ClientSource,
}

struct Envelope<'a> {
    id: Option<RequestId>,
    method: &'a str,
    params: Option<&'a Map<String, Value>>,
}

enum Route {
    Reply(Value),
    Notification,
    ToolCall {
        id: RequestId,
        message: Value,
        client: ClientSource,
    },
    Cancel(RequestId),
}

enum Frame {
    Bytes(Vec<u8>),
    TooLarge,
}

pub(crate) fn serve() {
    let sock = ds_config::Paths::resolve().map(|paths| paths.engine_sock);
    let stdin = std::io::stdin();
    serve_on(
        stdin.lock(),
        std::io::stdout(),
        sock.as_ref(),
        || match sock.as_ref() {
            Some(sock) => {
                log(
                    "no MCP client sent traffic before EOF; launching the resident host app as a standalone-run fallback",
                );
                ensure_engine(sock);
            }
            None => log("cannot resolve engine socket path; skipping standalone-run fallback"),
        },
    );
}

fn serve_on<R, W>(reader: R, out: W, sock: Option<&PathBuf>, on_no_traffic: impl FnOnce())
where
    R: BufRead,
    W: Write + Send + 'static,
{
    let executor: Arc<Executor> = Arc::new(|id, message, sock, client, cancelled| {
        tools::tools_call_validated(id, message, sock, client, cancelled)
    });
    serve_on_with(reader, out, sock, on_no_traffic, executor);
}

fn serve_on_with<R, W>(
    mut reader: R,
    out: W,
    sock: Option<&PathBuf>,
    on_no_traffic: impl FnOnce(),
    executor: Arc<Executor>,
) where
    R: BufRead,
    W: Write + Send + 'static,
{
    let out = Arc::new(Mutex::new(out));
    let output_open = Arc::new(AtomicBool::new(true));
    let active = Arc::new(AtomicUsize::new(0));
    let (completed_tx, completed_rx) = mpsc::channel();
    let mut workers = HashMap::<RequestId, std::thread::JoinHandle<()>>::new();
    let mut in_flight = HashMap::<RequestId, Arc<AtomicBool>>::new();
    let mut session = Session::default();
    let sock = sock.cloned();
    let mut handled_any = false;
    let mut reached_eof = false;

    loop {
        while let Ok(id) = completed_rx.try_recv() {
            in_flight.remove(&id);
            if let Some(worker) = workers.remove(&id) {
                let _ = worker.join();
            }
        }
        let frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                reached_eof = true;
                break;
            }
            Err(error) => {
                log(&format!("MCP stdin read failed: {error}"));
                break;
            }
        };
        let message = match frame {
            Frame::TooLarge => {
                handled_any = true;
                if !write_response(
                    &out,
                    &err(None, -32700, "Parse error: input frame exceeds 1 MiB"),
                ) {
                    break;
                }
                continue;
            }
            Frame::Bytes(bytes) => {
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                handled_any = true;
                match serde_json::from_slice::<Value>(&bytes) {
                    Ok(message) => message,
                    Err(error) => {
                        log(&format!("MCP JSON parse error: {error}"));
                        if !write_response(&out, &err(None, -32700, "Parse error")) {
                            break;
                        }
                        continue;
                    }
                }
            }
        };

        match route(&message, &mut session) {
            Route::Reply(response) => {
                if !write_response(&out, &response) {
                    break;
                }
            }
            Route::Notification => {}
            Route::Cancel(id) => {
                if let Some(cancelled) = in_flight.get(&id) {
                    cancelled.store(true, Ordering::Release);
                }
            }
            Route::ToolCall {
                id,
                message,
                client,
            } => {
                if in_flight.contains_key(&id) {
                    if !write_response(
                        &out,
                        &err(
                            Some(id.value()),
                            -32600,
                            "Invalid Request: request id is already in flight",
                        ),
                    ) {
                        break;
                    }
                    continue;
                }
                if !reserve_slot(&active) {
                    if !write_response(
                        &out,
                        &err(Some(id.value()), -32000, "Too many in-flight tool calls"),
                    ) {
                        break;
                    }
                    continue;
                }
                let cancelled = Arc::new(AtomicBool::new(false));
                in_flight.insert(id.clone(), cancelled.clone());
                let worker_out = out.clone();
                let worker_output_open = output_open.clone();
                let worker_active = active.clone();
                let worker_sock = sock.clone();
                let worker_executor = executor.clone();
                let worker_completed = completed_tx.clone();
                let worker_id = id.clone();
                let worker = std::thread::spawn(move || {
                    let _slot = SlotGuard(worker_active);
                    let response = worker_executor(
                        Some(id.value()),
                        &message,
                        worker_sock.as_ref(),
                        client,
                        cancelled.clone(),
                    );
                    if !cancelled.load(Ordering::Acquire) && !write_response(&worker_out, &response)
                    {
                        worker_output_open.store(false, Ordering::Release);
                    }
                    let _ = worker_completed.send(id);
                });
                workers.insert(worker_id, worker);
            }
        }
        if !output_open.load(Ordering::Acquire) {
            break;
        }
    }

    for cancelled in in_flight.values() {
        cancelled.store(true, Ordering::Release);
    }
    for worker in workers.into_values() {
        let _ = worker.join();
    }
    if reached_eof && !handled_any {
        on_no_traffic();
    }
}

fn reserve_slot(active: &AtomicUsize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < MAX_IN_FLIGHT_TOOL_CALLS).then_some(count + 1)
        })
        .is_ok()
}

struct SlotGuard(Arc<AtomicUsize>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn read_frame(reader: &mut impl BufRead) -> std::io::Result<Option<Frame>> {
    let mut bytes = Vec::new();
    let mut too_large = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if bytes.is_empty() && !too_large {
                Ok(None)
            } else if too_large {
                Ok(Some(Frame::TooLarge))
            } else {
                Ok(Some(Frame::Bytes(bytes)))
            };
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if !too_large {
            if bytes.len() + end > MAX_STDIN_FRAME_BYTES {
                too_large = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&available[..end]);
            }
        }
        let ended = available[..end].last() == Some(&b'\n');
        reader.consume(end);
        if ended {
            return Ok(Some(if too_large {
                Frame::TooLarge
            } else {
                Frame::Bytes(bytes)
            }));
        }
    }
}

fn write_response<W: Write>(out: &Arc<Mutex<W>>, response: &Value) -> bool {
    let Ok(mut out) = out.lock() else {
        return false;
    };
    writeln!(out, "{response}").is_ok() && out.flush().is_ok()
}

fn route(message: &Value, session: &mut Session) -> Route {
    let envelope = match validate_envelope(message) {
        Ok(envelope) => envelope,
        Err(response) => return Route::Reply(response),
    };
    match envelope.method {
        "initialize" => route_initialize(message, envelope, session),
        "notifications/initialized" => {
            if let Some(id) = envelope.id {
                Route::Reply(err(
                    Some(id.value()),
                    -32600,
                    "Invalid Request: notifications/initialized must not carry an id",
                ))
            } else {
                if session.lifecycle == Lifecycle::Initialized {
                    session.initialized_notification = true;
                } else {
                    log("ignoring notifications/initialized before initialize");
                }
                Route::Notification
            }
        }
        "ping" => match envelope.id {
            Some(id) => Route::Reply(ok(Some(id.value()), json!({}))),
            None => Route::Notification,
        },
        "notifications/cancelled" => route_cancellation(envelope, session),
        _ if session.lifecycle == Lifecycle::Uninitialized => match envelope.id {
            Some(id) => Route::Reply(err(Some(id.value()), -32002, "Server not initialized")),
            None => Route::Notification,
        },
        "tools/list" => match envelope.id {
            Some(id) => {
                if let Some(cursor) = envelope.params.and_then(|params| params.get("cursor"))
                    && !cursor.is_string()
                {
                    Route::Reply(err(
                        Some(id.value()),
                        -32602,
                        "Invalid params: cursor must be a string",
                    ))
                } else {
                    Route::Reply(ok(Some(id.value()), json!({ "tools": tools() })))
                }
            }
            None => Route::Notification,
        },
        "tools/call" => match envelope.id {
            None => Route::Notification,
            Some(id) => match tools::validate_tools_call(message) {
                Ok(()) => Route::ToolCall {
                    id,
                    message: message.clone(),
                    client: session.client,
                },
                Err(reason) => Route::Reply(err(
                    Some(id.value()),
                    -32602,
                    &format!("Invalid params: {reason}"),
                )),
            },
        },
        method => match envelope.id {
            Some(id) => Route::Reply(err(
                Some(id.value()),
                -32601,
                &format!("Method not found: {method}"),
            )),
            None => Route::Notification,
        },
    }
}

fn route_initialize(message: &Value, envelope: Envelope<'_>, session: &mut Session) -> Route {
    let Some(id) = envelope.id else {
        log("ignoring initialize notification without an id");
        return Route::Notification;
    };
    if session.lifecycle != Lifecycle::Uninitialized {
        return Route::Reply(err(
            Some(id.value()),
            -32600,
            "Invalid Request: initialize may only be sent once",
        ));
    }
    if let Err(reason) = validate_initialize(envelope.params) {
        return Route::Reply(err(
            Some(id.value()),
            -32602,
            &format!("Invalid params: {reason}"),
        ));
    }

    let (client, raw) = client_from_initialize(message);
    session.client = client;
    session.lifecycle = Lifecycle::Initialized;
    log(&format!(
        "initialize clientInfo.name={raw:?} client={}",
        client.as_str()
    ));
    Route::Reply(ok(Some(id.value()), initialize(message)))
}

fn route_cancellation(envelope: Envelope<'_>, session: &Session) -> Route {
    if let Some(id) = envelope.id {
        return Route::Reply(err(
            Some(id.value()),
            -32600,
            "Invalid Request: notifications/cancelled must not carry an id",
        ));
    }
    if session.lifecycle == Lifecycle::Uninitialized {
        return Route::Notification;
    }
    let Some(params) = envelope.params else {
        log("ignoring malformed cancellation notification without params");
        return Route::Notification;
    };
    let Some(id) = params.get("requestId").and_then(RequestId::from_value) else {
        log("ignoring malformed cancellation notification without a valid requestId");
        return Route::Notification;
    };
    if params
        .get("reason")
        .is_some_and(|reason| !reason.is_string())
    {
        log("ignoring malformed cancellation notification with a non-string reason");
        return Route::Notification;
    }
    if let Some(reason) = params.get("reason").and_then(Value::as_str) {
        log(&format!(
            "cancelling MCP request id={:?} reason={reason:?}",
            id
        ));
    }
    Route::Cancel(id)
}

fn validate_envelope(message: &Value) -> Result<Envelope<'_>, Value> {
    let Some(object) = message.as_object() else {
        return Err(err(None, -32600, "Invalid Request"));
    };
    let safe_id = object.get("id").and_then(RequestId::from_value);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(err(
            safe_id.as_ref().map(RequestId::value),
            -32600,
            "Invalid Request: jsonrpc must be `2.0`",
        ));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Err(err(
            safe_id.as_ref().map(RequestId::value),
            -32600,
            "Invalid Request: method must be a string",
        ));
    };
    if object.contains_key("id") && safe_id.is_none() {
        return Err(err(
            None,
            -32600,
            "Invalid Request: id must be a non-null string or integer",
        ));
    }
    let params = match object.get("params") {
        Some(Value::Object(params)) => Some(params),
        Some(_) => {
            return Err(err(
                safe_id.as_ref().map(RequestId::value),
                -32600,
                "Invalid Request: params must be an object",
            ));
        }
        None => None,
    };
    Ok(Envelope {
        id: safe_id,
        method,
        params,
    })
}

fn validate_initialize(params: Option<&Map<String, Value>>) -> Result<(), String> {
    let params = params.ok_or_else(|| "params are required".to_string())?;
    if !params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .is_some_and(|version| !version.is_empty())
    {
        return Err("protocolVersion must be a non-empty string".into());
    }
    if !params.get("capabilities").is_some_and(Value::is_object) {
        return Err("capabilities must be an object".into());
    }
    let client = params
        .get("clientInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| "clientInfo must be an object".to_string())?;
    for field in ["name", "version"] {
        if !client
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            return Err(format!("clientInfo.{field} must be a non-empty string"));
        }
    }
    Ok(())
}

fn client_from_initialize(message: &Value) -> (ClientSource, String) {
    let raw = message["params"]["clientInfo"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (client_from_mcp_name(&raw), raw)
}

fn initialize(message: &Value) -> Value {
    let requested = message["params"]["protocolVersion"].as_str();
    let version = requested
        .filter(|version| *version == PROTOCOL_VERSION)
        .unwrap_or(PROTOCOL_VERSION);
    let mut result = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
    });
    // Grok ignores passive-hook additionalContext (#95); MCP initialize.instructions still
    // delivers the digest contract at connect when digests are on.
    if digests_narration_on() {
        result.as_object_mut().expect("object").insert(
            "instructions".into(),
            json!(ds_config::DEFAULT_NARRATION_SPEC.trim_end()),
        );
    }
    result
}

/// Digests on in live config. Missing paths/config → false (initialize must not need home).
fn digests_narration_on() -> bool {
    let Some(paths) = ds_config::Paths::resolve() else {
        return false;
    };
    ds_config::VoiceConfig::load(&paths).narrates(ds_config::NarrateKind::Digests)
}

fn tools() -> Value {
    ds_tools::catalog()
}

pub(crate) fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub(crate) fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

pub(crate) fn structured_tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).expect("JSON values always serialize");
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": value,
        "isError": false,
    })
}

pub(crate) fn log(message: &str) {
    eprintln!("{message}");
    log::info!(target: "mcp", "{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn structured_results_include_machine_and_text_content() {
        let value = json!({"engine": "off"});
        let result = structured_tool_result(value.clone());
        assert_eq!(result["structuredContent"], value);
        assert_eq!(result["isError"], false);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            value
        );
    }

    fn initialize_line(id: i64) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "codex-mcp-client", "version": "1.0" }
            }
        })
        .to_string()
    }

    fn run(input: impl Into<Vec<u8>>) -> (Vec<Value>, bool) {
        let writer = SharedWriter::default();
        let bytes = writer.0.clone();
        let fell_back = Arc::new(AtomicBool::new(false));
        let fallback = fell_back.clone();
        serve_on(io::Cursor::new(input.into()), writer, None, move || {
            fallback.store(true, Ordering::Release)
        });
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let messages = output
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdout contains only JSON-RPC"))
            .collect();
        (messages, fell_back.load(Ordering::Acquire))
    }

    #[test]
    fn empty_or_blank_stdin_falls_back() {
        assert!(run(Vec::new()).1);
        assert!(run(b"\n  \n".to_vec()).1);
    }

    #[test]
    fn malformed_traffic_returns_parse_error_and_suppresses_fallback() {
        let (messages, fell_back) = run(b"not json\n".to_vec());
        assert!(!fell_back);
        assert_eq!(messages[0]["error"]["code"], -32700);
        assert!(messages[0]["id"].is_null());
    }

    #[test]
    fn oversized_frame_is_bounded_discarded_and_does_not_poison_the_next_frame() {
        let mut input = vec![b'x'; MAX_STDIN_FRAME_BYTES + 1];
        input.push(b'\n');
        input.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let (messages, fell_back) = run(input);
        assert!(!fell_back);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["error"]["code"], -32700);
        assert_eq!(messages[1]["result"], json!({}));
    }

    #[test]
    fn invalid_request_objects_are_rejected_with_only_safe_ids() {
        let cases = [
            (json!([]), Value::Null),
            (json!({"id": 3, "method": "ping"}), json!(3)),
            (
                json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
                Value::Null,
            ),
            (
                json!({"jsonrpc": "2.0", "id": 1.5, "method": "ping"}),
                Value::Null,
            ),
            (json!({"jsonrpc": "2.0", "id": 4, "method": 9}), json!(4)),
            (
                json!({"jsonrpc": "2.0", "id": 5, "method": "ping", "params": []}),
                json!(5),
            ),
        ];
        for (message, expected_id) in cases {
            let (responses, _) = run(message.to_string());
            assert_eq!(responses[0]["error"]["code"], -32600, "{message}");
            assert_eq!(responses[0]["id"], expected_id, "{message}");
        }
    }

    #[test]
    fn initialize_requires_all_required_fields_and_controls_lifecycle() {
        let malformed = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "clientInfo": {"name": "x", "version": "1"}}
        });
        let (responses, _) = run(malformed.to_string());
        assert_eq!(responses[0]["error"]["code"], -32602);

        let before = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let (responses, _) = run(before.to_string());
        assert_eq!(responses[0]["error"]["code"], -32002);

        let input = format!(
            "{}\n{}\n",
            initialize_line(3),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"})
        );
        let (responses, _) = run(input);
        assert_eq!(responses[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(responses[1]["result"]["tools"].is_array());
    }

    #[test]
    fn duplicate_initialize_is_an_invalid_request() {
        let input = format!("{}\n{}\n", initialize_line(1), initialize_line(2));
        let (responses, _) = run(input);
        assert_eq!(responses[1]["error"]["code"], -32600);
    }

    #[test]
    fn notification_methods_with_ids_are_not_mistaken_for_notifications() {
        for method in ["notifications/initialized", "notifications/cancelled"] {
            let input = format!(
                "{}\n{}\n",
                initialize_line(1),
                json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": {"requestId": 1}})
            );
            let (responses, _) = run(input);
            assert_eq!(responses.len(), 2);
            assert_eq!(responses[1]["error"]["code"], -32600);
        }
    }

    #[test]
    fn valid_notifications_never_receive_responses() {
        let input = format!(
            "{}\n{}\n{}\n",
            initialize_line(1),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "unknown"}})
        );
        let (responses, _) = run(input);
        assert_eq!(responses.len(), 1);
    }

    #[test]
    fn malformed_tools_calls_are_protocol_errors() {
        let calls = [
            json!({"params": {"name": "unknown", "arguments": {}}}),
            json!({"params": {"arguments": {}}}),
            json!({"params": {"name": "status", "arguments": []}}),
        ];
        for (index, call) in calls.into_iter().enumerate() {
            let request = json!({
                "jsonrpc": "2.0", "id": index + 2, "method": "tools/call",
                "params": call["params"].clone()
            });
            let input = format!("{}\n{}\n", initialize_line(1), request);
            let (responses, _) = run(input);
            assert_eq!(responses[1]["error"]["code"], -32602, "{request}");
            assert!(responses[1].get("result").is_none(), "{request}");
        }
    }

    #[test]
    fn advertised_schema_failures_are_actionable_tool_results() {
        let calls = [
            json!({"name": "speak", "arguments": {}}),
            json!({"name": "listen", "arguments": {"seconds": 61}}),
            json!({"name": "status", "arguments": {"extra": true}}),
            json!({"name": "mute", "arguments": {"on": "yes"}}),
            json!({"name": "voices", "arguments": {"tts_engine": "bogus"}}),
        ];
        for (index, params) in calls.into_iter().enumerate() {
            let request = json!({
                "jsonrpc": "2.0", "id": index + 2, "method": "tools/call", "params": params
            });
            let mut session = Session::default();
            let init: Value = serde_json::from_str(&initialize_line(1)).unwrap();
            assert!(matches!(route(&init, &mut session), Route::Reply(_)));
            let Route::ToolCall {
                id,
                message,
                client,
            } = route(&request, &mut session)
            else {
                panic!("valid tools/call structure must reach execution: {request}");
            };
            let response = tools::tools_call_validated(
                Some(id.value()),
                &message,
                None,
                client,
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(response["result"]["isError"], true, "{request}");
            assert!(response.get("error").is_none(), "{request}");
        }
    }

    #[test]
    fn cancellation_is_observed_while_a_tool_call_is_running() {
        let writer = SharedWriter::default();
        let bytes = writer.0.clone();
        let executor: Arc<Executor> = Arc::new(|id, _, _, _, cancelled| {
            while !cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            ok(id, tool_result("cancelled".into(), true))
        });
        let input = format!(
            "{}\n{}\n{}\n",
            initialize_line(1),
            json!({"jsonrpc": "2.0", "id": "listen-1", "method": "tools/call", "params": {"name": "listen", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "listen-1", "reason": "test"}})
        );
        serve_on_with(io::Cursor::new(input), writer, None, || {}, executor);
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let responses: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            responses.len(),
            1,
            "a cancelled request must not receive a response"
        );
        assert_eq!(responses[0]["id"], 1);
    }

    #[test]
    fn worker_slots_are_strictly_bounded() {
        let active = AtomicUsize::new(MAX_IN_FLIGHT_TOOL_CALLS);
        assert!(!reserve_slot(&active));
        active.store(MAX_IN_FLIGHT_TOOL_CALLS - 1, Ordering::Release);
        assert!(reserve_slot(&active));
        assert_eq!(active.load(Ordering::Acquire), MAX_IN_FLIGHT_TOOL_CALLS);
    }

    #[test]
    fn stdio_boundary_applies_backpressure_to_concurrent_calls() {
        let writer = SharedWriter::default();
        let bytes = writer.0.clone();
        let executor: Arc<Executor> = Arc::new(|id, _, _, _, cancelled| {
            while !cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            ok(id, tool_result("cancelled".into(), true))
        });
        let mut input = format!("{}\n", initialize_line(1));
        for id in 0..=MAX_IN_FLIGHT_TOOL_CALLS {
            input.push_str(
                &json!({
                    "jsonrpc": "2.0", "id": format!("call-{id}"), "method": "tools/call",
                    "params": {"name": "status", "arguments": {}}
                })
                .to_string(),
            );
            input.push('\n');
        }
        for id in 0..MAX_IN_FLIGHT_TOOL_CALLS {
            input.push_str(
                &json!({
                    "jsonrpc": "2.0", "method": "notifications/cancelled",
                    "params": {"requestId": format!("call-{id}")}
                })
                .to_string(),
            );
            input.push('\n');
        }

        serve_on_with(io::Cursor::new(input), writer, None, || {}, executor);
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let responses: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(
            responses[1]["id"],
            format!("call-{MAX_IN_FLIGHT_TOOL_CALLS}")
        );
        assert_eq!(responses[1]["error"]["code"], -32000);
    }

    #[test]
    fn version_negotiation_and_client_identity_remain_compatible() {
        let message = json!({
            "params": {
                "protocolVersion": "2024-11-05",
                "clientInfo": {"name": "claude-code", "version": "1"}
            }
        });
        assert_eq!(initialize(&message)["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(
            client_from_initialize(&message),
            (ClientSource::ClaudeCode, "claude-code".to_string())
        );
    }

    #[test]
    fn tool_execution_errors_remain_successful_json_rpc_results() {
        let response = ok(
            Some(json!(1)),
            tool_result("engine unavailable".into(), true),
        );
        assert_eq!(response["result"]["isError"], true);
        assert!(response.get("error").is_none());
    }
}
