//! Tolerant serde DTOs for EXACTLY the codex app-server JSON-RPC subset the subscriber
//! consumes — hand-written on purpose (the official `codex-app-server-protocol` crate is
//! stale on crates.io and drags the full protocol surface; this is a client of an external
//! tool's socket, not codegen at our FFI boundary). Unknown methods and unknown fields are
//! IGNORED (`Incoming::Other`) — the same tolerance discipline as `ds_ipc::Response::Unknown`
//! — so codex version skew degrades to "less narration", never a parse panic.
//!
//! Shapes verified against the codex source of truth on 2026-07-08
//! (openai/codex `codex-rs/app-server-protocol`, and
//! <https://developers.openai.com/codex/app-server>):
//!   * `initialize` → `{clientInfo, capabilities.optOutNotificationMethods}` (v1.rs
//!     `InitializeParams`), then the `initialized` notification.
//!   * `thread/loaded/list` → `{data: [thread ids], nextCursor}` ("Thread ids for sessions
//!     currently loaded in memory" — `ThreadLoadedListResponse`).
//!   * `thread/resume` → params `{threadId}`; response `{thread: {id, …}}`.
//!   * `thread/unsubscribe` → params `{threadId}`.
//!   * `item/agentMessage/delta` → `{threadId, turnId, itemId, delta}` (all required —
//!     `AgentMessageDeltaNotification` JSON schema).
//!   * `item/completed` → `{threadId, turnId, item, completedAtMs}`; an agentMessage item is
//!     `{type: "agentMessage", id, text, phase?}` — the authoritative final text.
//!   * `turn/completed` → `{threadId, turn}` (we only read `threadId`, as a flush nudge).

use serde::Deserialize;
use serde_json::{Value, json};

/// Notification methods we opt OUT of at `initialize` — known-noisy delta streams we never
/// consume (reasoning, plan, exec/process output, raw response items). Names are exact wire
/// method names; an unknown name is ignored by the server, so this list is safe across
/// codex versions.
pub(crate) const OPT_OUT_METHODS: &[&str] = &[
    "item/reasoning/textDelta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/plan/delta",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "command/exec/outputDelta",
    "process/outputDelta",
    "rawResponseItem/completed",
    "turn/diff/updated",
];

/// The ONE correlation point between a Codex hook payload's `session_id` and an app-server
/// thread id: expected uuid passthrough — a root thread's id IS its rollout session id
/// (codex docs: forked threads keep the root's `sessionId`, and clients should read
/// `thread.sessionId` rather than derive it — we additionally verify the resume response's
/// `sessionId` when present, see `ThreadResumed`). Verified live on 2026-07-12 with Codex
/// 0.144.1 over `ws://` on Windows: `SessionStart` and `UserPromptSubmit` hooks both carried
/// the same id returned by `thread/loaded/list`, and `thread/resume` attached that id. A live
/// `thread/fork` check also returned a fresh thread whose `thread.id == thread.sessionId`
/// (with the root only in `forkedFromId`), so the passthrough holds for forked threads too.
pub(crate) fn session_for_thread(thread_id: &str) -> String {
    thread_id.to_string()
}

// ── Outgoing (rendered straight to JSON text — no structs needed) ────────────────

/// The `initialize` request: name ourselves and opt out of the noisy delta streams.
pub(crate) fn initialize_request(id: i64) -> String {
    json!({
        "method": "initialize",
        "id": id,
        "params": {
            "clientInfo": {
                "name": "dontspeak",
                "title": "DontSpeak voice narration",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "optOutNotificationMethods": OPT_OUT_METHODS,
            },
        },
    })
    .to_string()
}

/// The `initialized` notification, sent after the `initialize` response arrives.
pub(crate) fn initialized_notification() -> String {
    json!({ "method": "initialized", "params": {} }).to_string()
}

/// `thread/loaded/list` — the thread ids currently loaded in the shared server's memory.
pub(crate) fn thread_loaded_list_request(id: i64) -> String {
    json!({ "method": "thread/loaded/list", "id": id, "params": {} }).to_string()
}

/// `thread/resume` — rejoin a running thread by id; auto-subscribes this connection to the
/// thread's event stream (official multi-client fan-out).
pub(crate) fn thread_resume_request(id: i64, thread_id: &str) -> String {
    json!({ "method": "thread/resume", "id": id, "params": { "threadId": thread_id } }).to_string()
}

/// `thread/unsubscribe` — detach from a thread's event stream (we stop narrating it).
pub(crate) fn thread_unsubscribe_request(id: i64, thread_id: &str) -> String {
    json!({ "method": "thread/unsubscribe", "id": id, "params": { "threadId": thread_id } })
        .to_string()
}

// ── Incoming ─────────────────────────────────────────────────────────────────────

/// One parsed incoming message — the subset we act on. Anything else (unknown method,
/// unconsumed notification, malformed JSON) is [`Incoming::Other`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// A response to one of OUR requests (matched to a pending id by the caller).
    /// `result` is `None` when the server answered with an error.
    Response {
        id: i64,
        result: Option<Value>,
    },
    /// `item/agentMessage/delta` — a streamed chunk of an assistant message.
    AgentMessageDelta {
        thread_id: String,
        item_id: String,
        delta: String,
    },
    /// `item/completed` for an `agentMessage` item — the authoritative final text.
    AgentMessageCompleted {
        thread_id: String,
        item_id: String,
        text: String,
    },
    /// `turn/completed` — used only as a flush nudge for the thread's coalesce buffers.
    TurnCompleted {
        thread_id: String,
    },
    Other,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaParams {
    thread_id: String,
    item_id: String,
    delta: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemCompletedParams {
    thread_id: String,
    item: CompletedItem,
}

#[derive(Deserialize)]
struct CompletedItem {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnCompletedParams {
    thread_id: String,
}

/// Parse one raw JSON-RPC text frame, tolerantly. Never errors: anything we don't
/// consume — or can't parse — is [`Incoming::Other`].
pub(crate) fn parse_incoming(raw: &str) -> Incoming {
    let Ok(msg) = serde_json::from_str::<RawMessage>(raw) else {
        return Incoming::Other;
    };
    match (&msg.method, msg.id) {
        // A response (no method, an id): ours if the id matches a pending request.
        (None, Some(id)) => Incoming::Response {
            id,
            result: if msg.error.is_some() {
                None
            } else {
                Some(msg.result.unwrap_or(Value::Null))
            },
        },
        (Some(method), _) => {
            let params = msg.params.unwrap_or(Value::Null);
            match method.as_str() {
                "item/agentMessage/delta" => match serde_json::from_value::<DeltaParams>(params) {
                    Ok(p) => Incoming::AgentMessageDelta {
                        thread_id: p.thread_id,
                        item_id: p.item_id,
                        delta: p.delta,
                    },
                    Err(_) => Incoming::Other,
                },
                "item/completed" => match serde_json::from_value::<ItemCompletedParams>(params) {
                    // Only agentMessage items carry the assistant prose we narrate; every
                    // other item type (commandExecution, fileChange, plan, …) is ignored.
                    Ok(p) if p.item.kind == "agentMessage" && !p.item.id.is_empty() => {
                        Incoming::AgentMessageCompleted {
                            thread_id: p.thread_id,
                            item_id: p.item.id,
                            text: p.item.text,
                        }
                    }
                    _ => Incoming::Other,
                },
                "turn/completed" => match serde_json::from_value::<TurnCompletedParams>(params) {
                    Ok(p) => Incoming::TurnCompleted {
                        thread_id: p.thread_id,
                    },
                    Err(_) => Incoming::Other,
                },
                _ => Incoming::Other,
            }
        }
        _ => Incoming::Other,
    }
}

/// Pull the loaded thread ids out of a `thread/loaded/list` response's `result`
/// (`{data: ["thr_…", …]}`). Tolerant: anything malformed reads as empty.
pub(crate) fn loaded_thread_ids(result: &Value) -> Vec<String> {
    result
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `sessionId` a `thread/resume` response reports for the thread, when present
/// (`{thread: {id, sessionId, …}}`) — the authoritative correlation per the codex docs
/// (forked threads keep the root's session id). `None` when absent/malformed.
pub(crate) fn resumed_session_id(result: &Value) -> Option<String> {
    result
        .get("thread")
        .and_then(|t| t.get("sessionId"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_carries_client_info_and_the_opt_out_list() {
        let raw = initialize_request(7);
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["method"], "initialize");
        assert_eq!(v["id"], 7);
        assert_eq!(v["params"]["clientInfo"]["name"], "dontspeak");
        let opt_out = v["params"]["capabilities"]["optOutNotificationMethods"]
            .as_array()
            .expect("opt-out list present");
        assert!(
            opt_out.iter().any(|m| m == "item/reasoning/textDelta"),
            "reasoning deltas opted out"
        );
        assert!(
            opt_out
                .iter()
                .any(|m| m == "item/reasoning/summaryTextDelta"),
            "reasoning summary deltas opted out"
        );
        assert!(
            !opt_out.iter().any(|m| m == "item/agentMessage/delta"),
            "the ONE stream we consume must never be opted out"
        );
    }

    #[test]
    fn requests_render_the_documented_wire_shapes() {
        let v: Value = serde_json::from_str(&thread_resume_request(3, "thr_1")).unwrap();
        assert_eq!(v["method"], "thread/resume");
        assert_eq!(v["params"]["threadId"], "thr_1");
        let v: Value = serde_json::from_str(&thread_loaded_list_request(4)).unwrap();
        assert_eq!(v["method"], "thread/loaded/list");
        let v: Value = serde_json::from_str(&thread_unsubscribe_request(5, "thr_2")).unwrap();
        assert_eq!(v["method"], "thread/unsubscribe");
        assert_eq!(v["params"]["threadId"], "thr_2");
        let v: Value = serde_json::from_str(&initialized_notification()).unwrap();
        assert_eq!(v["method"], "initialized");
        assert!(v.get("id").is_none(), "a notification has no id");
    }

    #[test]
    fn parses_the_delta_notification_shape() {
        // The exact wire shape from AgentMessageDeltaNotification's JSON schema.
        let raw = r#"{"method":"item/agentMessage/delta","params":{
            "threadId":"thr_1","turnId":"turn_9","itemId":"item_3","delta":"> Spoken"}}"#;
        assert_eq!(
            parse_incoming(raw),
            Incoming::AgentMessageDelta {
                thread_id: "thr_1".into(),
                item_id: "item_3".into(),
                delta: "> Spoken".into(),
            }
        );
    }

    #[test]
    fn parses_item_completed_for_agent_messages_only() {
        let agent = r#"{"method":"item/completed","params":{
            "threadId":"thr_1","turnId":"t","completedAtMs":1730000000000,
            "item":{"type":"agentMessage","id":"item_3","text":"> Done.\n\nBody.","phase":"final_answer"}}}"#;
        assert_eq!(
            parse_incoming(agent),
            Incoming::AgentMessageCompleted {
                thread_id: "thr_1".into(),
                item_id: "item_3".into(),
                text: "> Done.\n\nBody.".into(),
            }
        );
        // A completed commandExecution item is NOT narration material.
        let cmd = r#"{"method":"item/completed","params":{
            "threadId":"thr_1","turnId":"t","completedAtMs":1,
            "item":{"type":"commandExecution","id":"item_4","command":"ls"}}}"#;
        assert_eq!(parse_incoming(cmd), Incoming::Other);
    }

    #[test]
    fn parses_turn_completed_and_responses() {
        assert_eq!(
            parse_incoming(
                r#"{"method":"turn/completed","params":{"threadId":"thr_1","turn":{}}}"#
            ),
            Incoming::TurnCompleted {
                thread_id: "thr_1".into()
            }
        );
        // A success response keeps its result; an error response reads result = None.
        assert_eq!(
            parse_incoming(r#"{"id":9,"result":{"data":["thr_1"]}}"#),
            Incoming::Response {
                id: 9,
                result: Some(serde_json::json!({"data":["thr_1"]})),
            }
        );
        assert_eq!(
            parse_incoming(r#"{"id":10,"error":{"code":-32600,"message":"nope"}}"#),
            Incoming::Response {
                id: 10,
                result: None
            }
        );
    }

    #[test]
    fn unknown_methods_and_junk_are_ignored() {
        for raw in [
            r#"{"method":"thread/started","params":{"thread":{"id":"thr_9"}}}"#,
            r#"{"method":"item/reasoning/textDelta","params":{"threadId":"t","delta":"x"}}"#,
            r#"{"method":"some/future/method","params":{"whatever":1}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"malformed":true}}"#,
            "not json at all",
            "{}",
        ] {
            assert_eq!(parse_incoming(raw), Incoming::Other, "must ignore: {raw}");
        }
    }

    #[test]
    fn result_helpers_are_tolerant() {
        let ok = serde_json::json!({"data": ["thr_1", "thr_2"], "nextCursor": null});
        assert_eq!(loaded_thread_ids(&ok), vec!["thr_1", "thr_2"]);
        assert!(loaded_thread_ids(&serde_json::json!({})).is_empty());
        assert!(loaded_thread_ids(&Value::Null).is_empty());

        let resumed = serde_json::json!({"thread": {"id": "thr_1", "sessionId": "sess-9"}});
        assert_eq!(resumed_session_id(&resumed).as_deref(), Some("sess-9"));
        assert_eq!(resumed_session_id(&Value::Null), None);
    }

    #[test]
    fn session_for_thread_is_uuid_passthrough() {
        // Pinned expectation (verified again in the §9 live capture): a root thread's id IS
        // the rollout session id the hooks report.
        assert_eq!(
            session_for_thread("0192aef2-4c3e-7b1a-9f00-abcdef012345"),
            "0192aef2-4c3e-7b1a-9f00-abcdef012345"
        );
    }
}
