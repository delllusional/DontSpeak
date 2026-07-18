//! `tools/call` router and `call_*` handlers (strict arg structs). Most bridge to the
//! engine over `ds-ipc`; `list_voices`/`set_config`/`get_status`/`get_usage` are direct
//! and never spawn the engine (`set_config` still best-effort-nudges Reload).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ds_config::{ClientSource, Paths, TtsEngine, VoiceConfig};
use ds_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine_launch::ensure_engine;
use crate::mcp::{ok, structured_tool_result, tool_result};
use crate::voices::voice_groups;

/// `client` from MCP `initialize` `clientInfo.name` (see `mcp::client_from_initialize`),
/// stamped on every engine request so the activity log attributes tool-driven speech.
#[cfg(test)]
pub(crate) fn tools_call(
    id: Option<Value>,
    msg: &Value,
    sock: Option<&PathBuf>,
    client: ClientSource,
) -> Value {
    tools_call_cancellable(id, msg, sock, client, Arc::new(AtomicBool::new(false)))
}

/// Validate `tools/call` shape + advertised schema before any handler/FS/IPC work.
pub(crate) fn validate_tools_call(msg: &Value) -> Result<(), String> {
    let params = msg
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| "params must be an object".to_string())?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "params.name must be a non-empty string".to_string())?;
    if !ds_tools::tool_names().any(|candidate| candidate == name) {
        return Err(format!("unknown tool: {name}"));
    }
    if params
        .get("arguments")
        .is_some_and(|arguments| !arguments.is_object())
    {
        return Err("params.arguments must be an object".into());
    }
    Ok(())
}

pub(crate) fn tools_call_validated(
    id: Option<Value>,
    msg: &Value,
    sock: Option<&PathBuf>,
    client: ClientSource,
    cancelled: Arc<AtomicBool>,
) -> Value {
    let name = msg["params"]["name"].as_str().unwrap_or_default();
    let arguments = msg["params"]
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Err(reason) = ds_tools::validate_arguments(name, &arguments) {
        return ok(
            id,
            tool_result(format!("invalid {name} arguments: {reason}"), true),
        );
    }
    tools_call_cancellable(id, msg, sock, client, cancelled)
}

pub(crate) fn tools_call_cancellable(
    id: Option<Value>,
    msg: &Value,
    sock: Option<&PathBuf>,
    client: ClientSource,
    cancelled: Arc<AtomicBool>,
) -> Value {
    let params = msg.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result: Result<ToolSuccess, String> = match name {
        // Config-direct; no engine.
        "list_voices" => match Paths::resolve() {
            Some(paths) => call_list_voices(&paths, &args).map(ToolSuccess::Structured),
            None => Err("Cannot resolve data paths.".into()),
        },
        // config.toml write; engine mtime-watch or Reload nudge. Engine need not be up.
        "set_config" => match Paths::resolve() {
            Some(paths) => call_set_config(&paths, &args).map(ToolSuccess::Text),
            None => Err("Cannot resolve data paths.".into()),
        },
        // Read-only; must not spawn engine / start playback.
        "get_status" => match Paths::resolve() {
            Some(paths) => call_status(&paths, sock, &args).map(ToolSuccess::Structured),
            None => Err("Cannot resolve data paths.".into()),
        },
        // Read-only subscription quotas; shared cache/provider logic with the Usage tab.
        "get_usage" => call_usage(&args).map(ToolSuccess::Structured),
        "speak" | "stop_speech" | "mute" | "listen" | "diarize" | "manage_speakers" => {
            let Some(sock) = sock else {
                return ok(
                    id,
                    tool_result("Cannot resolve engine socket.".into(), true),
                );
            };
            ensure_engine(sock);
            match name {
                "speak" => call_speak(sock, &args, client).map(ToolSuccess::Text),
                "stop_speech" => call_stop(sock, client).map(ToolSuccess::Text),
                "mute" => call_mute(sock, &args).map(ToolSuccess::Text),
                "diarize" => call_diarize(sock, &args).map(ToolSuccess::Text),
                "manage_speakers" => call_speakers(sock, &args).map(ToolSuccess::Text),
                _ => call_listen(sock, &args, cancelled).map(ToolSuccess::Text),
            }
        }
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(ToolSuccess::Text(text)) => ok(id, tool_result(text, false)),
        Ok(ToolSuccess::Structured(value)) => ok(id, structured_tool_result(value)),
        Err(e) => ok(id, tool_result(e, true)),
    }
}

enum ToolSuccess {
    Text(String),
    Structured(Value),
}

// Arg structs: deny_unknown_fields; fields == schema properties (pinned by
// tool_schemas_match_arg_structs). tts_engine reuses ds_config's strict TtsEngine.

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StatusArgs {
    detail: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UsageArgs {
    force_refresh: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SpeakArgs {
    text: Option<String>,
    voice: Option<String>,
    rate: Option<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct MuteArgs {
    on: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ListVoicesArgs {
    tts_engine: Option<TtsEngine>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ListenArgs {
    seconds: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DiarizeArgs {
    seconds: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SpeakersArgs {
    action: Option<String>,
    name: Option<String>,
    seconds: Option<u64>,
}

fn call_list_voices(paths: &Paths, args: &Value) -> Result<Value, String> {
    let a: ListVoicesArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid list_voices arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // Explicit arg, else resolved TTS ladder (still a catalog when spoken replies are off).
    let engine = a
        .tts_engine
        .or_else(|| cfg.resolved_tts())
        .unwrap_or(ds_config::TtsEngine::Kokoro);
    // English-only build: never surface other Kokoro pack languages.
    let mut groups = voice_groups(engine, "en");
    let current = cfg.current_voice();
    let languages: Vec<Value> = groups
        .iter_mut()
        .map(|(subtag, voices)| {
            for v in voices.iter_mut() {
                let id = v
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string();
                v["active"] = json!(id == current);
            }
            json!({ "language": subtag, "voices": voices })
        })
        .collect();
    let out = json!({
        "engine": engine.brand(),
        "language": "en",
        "languages": languages,
    });
    Ok(out)
}

/// Configured engine/voice/rate + live playback. `detail` adds model lifecycle/stats
/// (former `model_status`). Read-only probe — never spawns the engine.
fn call_status(paths: &Paths, sock: Option<&PathBuf>, args: &Value) -> Result<Value, String> {
    let a: StatusArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid get_status arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // Keyed "state" not "engine" — serde_json keeps only the last of a duplicate key
    // (previously silently dropped the configured engine name).
    let state = match sock {
        Some(sock) => match ds_ipc::request(sock, &Request::Status) {
            Ok(Response::Status {
                tts_active,
                queued,
                paused,
                muted,
            }) => {
                // muted: queue still drains silently. Surfaced in concise status (not only
                // detail) because the narration path never calls a tool that could report it.
                json!({ "running": true, "tts_active": tts_active, "queued": queued, "paused": paused, "muted": muted })
            }
            Ok(_) => json!({ "running": true, "note": "unexpected engine response" }),
            Err(_) => json!({ "running": false }),
        },
        None => json!({ "running": false, "note": "cannot resolve engine socket" }),
    };
    let mut out = json!({
        "engine": cfg.resolved_tts().map(|e| e.brand()).unwrap_or("off"),
        "voice": cfg.current_voice(),
        // Shared Kokoro voice pool for both TTS backends.
        "voices": cfg.active_voices().to_vec(),
        "rate": cfg.tts_rate,
        "state": state,
    });
    if a.detail.unwrap_or(false) {
        out["models"] = match sock {
            Some(sock) => match ds_ipc::request(sock, &Request::ModelStatus) {
                Ok(Response::ModelStatus { status }) if status.is_object() => status,
                Ok(Response::ModelStatus { .. }) => {
                    json!({ "running": false, "note": "invalid engine response" })
                }
                _ => json!({ "running": false, "note": "engine unavailable" }),
            },
            None => json!({ "running": false, "note": "cannot resolve engine socket" }),
        };
    }
    Ok(out)
}

fn call_usage(args: &Value) -> Result<Value, String> {
    call_usage_with(args, ds_agent_usage::snapshot)
}

fn call_usage_with(
    args: &Value,
    snapshot: impl FnOnce(bool) -> ds_agent_usage::UsageDeck,
) -> Result<Value, String> {
    let a: UsageArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid get_usage arguments: {e}"))?;
    serde_json::to_value(snapshot(a.force_refresh.unwrap_or(false)))
        .map_err(|e| format!("get_usage failed: {e}"))
}

// config.toml is source of truth; engine Reload nudge (mtime-watch if down).

fn call_set_config(paths: &Paths, args: &Value) -> Result<String, String> {
    // SetConfigArgs is the single settable surface — deny_unknown_fields + schema parity
    // with ds-tools so handler and catalog cannot drift.
    let parsed: ds_tools::SetConfigArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid set_config arguments: {e}"))?;

    // System STT opt-in: only persist when the *running* engine can authorize on-device
    // (model download / Speech permission). Gate on whether the new preference *resolves*
    // to System on this machine — not merely whether the caller named `system`. Engine down
    // ⇒ refuse (never enable blindly). apply() already rejects unusable static choices.
    let would_run_system = parsed.stt_engine.as_ref().is_some_and(|pref| {
        VoiceConfig {
            stt_engine: Some(pref.clone()),
            ..VoiceConfig::default()
        }
        .resolved_stt()
            == Some(ds_config::SttEngine::System)
    });
    if would_run_system {
        match ds_ipc::request(&paths.engine_sock, &ds_ipc::Request::AuthorizeSystemStt) {
            Ok(ds_ipc::Response::Done) => {}
            Ok(ds_ipc::Response::Error { message }) => return Err(message),
            Ok(_) => return Err("set_config failed: unexpected engine response".into()),
            Err(_) => {
                return Err(
                    "can't verify system STT — launch DontSpeak (needs on-device availability \
                     + permission), then set stt_engine=system again"
                        .into(),
                );
            }
        }
    }

    let mut cfg = VoiceConfig::load(paths);
    let changes = parsed.apply(&mut cfg)?;

    if changes.is_empty() {
        return Err("At least one setting required.".into());
    }

    ds_config::write_settings(paths, &cfg).map_err(|e| format!("set_config write failed: {e}"))?;
    let _ = ds_ipc::request(&paths.engine_sock, &ds_ipc::Request::Reload);

    Ok(format!("Updated {}.", changes.join(", ")))
}

/// Ambient Claude session for this MCP process (stdio = one process per session).
/// Claude Code sets `CLAUDE_CODE_SESSION_ID` in the server env — undocumented for MCP but
/// present in practice (claude-code #41836). Never a tool argument. `None` ⇒ machine-global.
fn session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

fn call_speak(sock: &Path, args: &Value, client: ClientSource) -> Result<String, String> {
    let a: SpeakArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid speak arguments: {e}"))?;
    let text = a.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Err("`text` is required.".into());
    }
    match ds_ipc::request(
        sock,
        &Request::Speak {
            text,
            voice: a.voice,
            rate: a.rate,
            session: session_id(),
            source: client,
        },
    ) {
        Ok(Response::Done) => Ok("Queued.".into()),
        Ok(Response::Error { message }) => Err(format!("speak failed: {message}")),
        Ok(_) => Err("speak failed: unexpected engine response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn call_stop(sock: &Path, client: ClientSource) -> Result<String, String> {
    // Scope barge to ambient session so one terminal doesn't stop another's voice.
    // session_id() None (e.g. bare CLI) → global hard barge.
    let session = session_id();
    let scoped = session.is_some();
    let response = ds_ipc::request(
        sock,
        &Request::StopSpeech {
            session,
            source: client,
        },
    );
    match response {
        Ok(response) => stop_response(response, scoped),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn stop_response(response: Response, scoped: bool) -> Result<String, String> {
    match response {
        Response::Done if scoped => Ok("Stopped this session's speech.".into()),
        Response::Done => Ok("Stopped all speech.".into()),
        Response::Error { message } => Err(format!("stop_speech failed: {message}")),
        _ => Err("stop_speech failed: unexpected engine response".into()),
    }
}

/// Global mute via the same `SetMuted` path as tray / Caps-Lock (`ds_core::ds_set_muted`) —
/// one canonical path so tool- and app-driven mute cannot diverge. Unlike `stop_speech`,
/// mute silences future output too (queue drains inaudibly) until changed or restart.
fn call_mute(sock: &Path, args: &Value) -> Result<String, String> {
    let a: MuteArgs =
        serde_json::from_value(args.clone()).map_err(|e| format!("invalid mute arguments: {e}"))?;
    let Some(on) = a.on else {
        return Err("`on` is required.".into());
    };
    // Plain confirmation; "put it in text" coaching is UserPromptSubmit + tool description.
    match ds_ipc::request(sock, &Request::SetMuted { on }) {
        Ok(response) => mute_response(response, on),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn mute_response(response: Response, on: bool) -> Result<String, String> {
    match response {
        Response::Done if on => Ok("Muted.".into()),
        Response::Done => Ok("Unmuted.".into()),
        Response::Error { message } => Err(format!("mute failed: {message}")),
        _ => Err("mute failed: unexpected engine response".into()),
    }
}

/// One-shot diarization: record `seconds`, return who spoke when. Engine blocks ≤60s
/// (within IPC timeout) — single request/response, no listen-style stream.
fn call_diarize(sock: &Path, args: &Value) -> Result<String, String> {
    let a: DiarizeArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid diarize arguments: {e}"))?;
    let seconds = a.seconds.unwrap_or(10).clamp(1, 60);
    match ds_ipc::request(sock, &Request::Diarize { seconds }) {
        Ok(Response::Diarization { segments }) => {
            let segs = segments.as_array().cloned().unwrap_or_default();
            let speakers: std::collections::BTreeSet<&str> = segs
                .iter()
                .filter_map(|s| s.get("speaker").and_then(|v| v.as_str()))
                .collect();
            let summary = if segs.is_empty() {
                "No speech detected.".to_string()
            } else {
                format!("{} speaker(s), {} segment(s):", speakers.len(), segs.len())
            };
            let body =
                serde_json::to_string_pretty(&segments).unwrap_or_else(|_| segments.to_string());
            Ok(format!("{summary}\n{body}"))
        }
        Ok(Response::Error { message }) => Err(format!("diarize failed: {message}")),
        Ok(_) => Err("diarize failed: unexpected engine response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// Enrolled voiceprints for `diarize` labels. Thin bridge to Enroll / ForgetSpeaker /
/// ListSpeakers. Schema can't express "name only for enroll/forget" — validated per action.
fn call_speakers(sock: &Path, args: &Value) -> Result<String, String> {
    let a: SpeakersArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid manage_speakers arguments: {e}"))?;
    let need_name = || -> Result<String, String> {
        let name = a.name.clone().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            Err("manage_speakers: `name` is required for this action".into())
        } else {
            Ok(name)
        }
    };
    match a.action.as_deref().unwrap_or("").trim() {
        "list" => list_speakers(sock),
        "enroll" => enroll_speaker(sock, need_name()?, a.seconds.unwrap_or(15).clamp(1, 60)),
        "forget" => forget_speaker(sock, need_name()?),
        "" => Err("manage_speakers: `action` is required (list | enroll | forget)".into()),
        other => Err(format!(
            "manage_speakers: unknown action `{other}` (use list | enroll | forget)"
        )),
    }
}

/// Record `seconds`, extract embedding, persist under `name`. Blocks ≤60s.
fn enroll_speaker(sock: &Path, name: String, seconds: u64) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::Enroll { name, seconds }) {
        Ok(Response::Enrolled { name }) => Ok(format!("Enrolled \"{name}\".")),
        Ok(Response::Error { message }) => Err(format!("manage_speakers failed: {message}")),
        Ok(_) => Err("manage_speakers failed: unexpected engine response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn forget_speaker(sock: &Path, name: String) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::ForgetSpeaker { name: name.clone() }) {
        Ok(Response::Done) => Ok(format!("Removed \"{name}\" (if present).")),
        Ok(Response::Error { message }) => Err(format!("manage_speakers failed: {message}")),
        Ok(_) => Err("manage_speakers failed: unexpected engine response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn list_speakers(sock: &Path) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::ListSpeakers) {
        Ok(Response::Speakers { names }) => {
            if names.is_empty() {
                Ok("No speakers enrolled (action=enroll).".into())
            } else {
                Ok(format!("Enrolled ({}): {}", names.len(), names.join(", ")))
            }
        }
        Ok(Response::Error { message }) => Err(format!("manage_speakers failed: {message}")),
        Ok(_) => Err("manage_speakers failed: unexpected engine response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// Trailing silence before listen finalizes (after speech has started). Long enough that a
/// between-sentence breath doesn't cut a multi-sentence answer short.
const LISTEN_ENDPOINT_SILENCE: Duration = Duration::from_millis(1500);

/// Live Parakeet session → final transcript. Auto-stops on end-of-speech (like Caps-Lock
/// dictation, not a blind fixed window). EOS from the Partial stream (engine emits Partial
/// only when transcript *changes*); watchdog on a second connection sends
/// `TestRecognitionStop` after [`LISTEN_ENDPOINT_SILENCE`] of no new partial, or after the
/// `seconds` hard cap. Cancellable + joined so it cannot leak a late stop onto a later session.
fn call_listen(sock: &Path, args: &Value, cancelled: Arc<AtomicBool>) -> Result<String, String> {
    use std::sync::atomic::AtomicU64;

    let a: ListenArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid listen arguments: {e}"))?;
    let max_secs = a.seconds.unwrap_or(30).clamp(1, 60);

    let mut client = ds_ipc::connect(sock).map_err(|e| format!("engine unavailable: {e}"))?;
    client
        .send(&Request::TestRecognitionStart)
        .map_err(|e| format!("listen failed to start: {e}"))?;

    // `spoke` gates silence so leading quiet never ends early; last_change_ms resets on
    // each non-empty Partial.
    let base = std::time::Instant::now();
    let now_ms = move || base.elapsed().as_millis() as u64;
    let spoke = Arc::new(AtomicBool::new(false));
    let last_change_ms = Arc::new(AtomicU64::new(0));
    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
    let sock2 = sock.to_path_buf();
    let (wd_spoke, wd_last) = (spoke.clone(), last_change_ms.clone());
    let watchdog = std::thread::spawn(move || {
        let hard_cap = Duration::from_secs(max_secs);
        loop {
            match cancel_rx.recv_timeout(Duration::from_millis(100)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                _ => return, // cancelled/finished
            }
            let elapsed = base.elapsed();
            if cancelled.load(Ordering::Acquire) {
                let _ = ds_ipc::request(&sock2, &Request::TestRecognitionStop);
                return;
            }
            let went_quiet = wd_spoke.load(Ordering::Relaxed)
                && elapsed.saturating_sub(Duration::from_millis(wd_last.load(Ordering::Relaxed)))
                    >= LISTEN_ENDPOINT_SILENCE;
            if elapsed >= hard_cap || went_quiet {
                let _ = ds_ipc::request(&sock2, &Request::TestRecognitionStop);
                return;
            }
        }
    });

    let outcome = loop {
        match client.recv() {
            Ok(Response::Transcript { text }) => {
                break Ok(if text.trim().is_empty() {
                    "No speech recognized.".into()
                } else {
                    text
                });
            }
            Ok(Response::Partial { text }) => {
                if !text.trim().is_empty() {
                    spoke.store(true, Ordering::Relaxed);
                    last_change_ms.store(now_ms(), Ordering::Relaxed);
                }
                continue;
            }
            Ok(Response::Error { message }) => break Err(format!("listen failed: {message}")),
            Ok(_) => continue,
            Err(e) => break Err(format!("listen stream ended: {e}")),
        }
    };
    drop(cancel_tx);
    let _ = watchdog.join();
    outcome
}

#[cfg(test)]
mod drift {
    use super::*;

    /// Drift guard: every `ds_tools` catalog name must hit a real `tools_call` arm.
    /// Bogus args + no socket keep paths side-effect-free; reject only `unknown tool:`.
    #[test]
    fn router_handles_every_catalog_tool() {
        let bogus = json!({ "__not_a_real_field__": true });
        for name in ds_tools::tool_names() {
            let msg = json!({ "params": { "name": name, "arguments": bogus.clone() } });
            let resp = tools_call(None, &msg, None, ClientSource::Unknown);
            let text = resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(
                !text.starts_with("unknown tool:"),
                "dispatch router doesn't handle catalog tool `{name}` (got: {text})"
            );
        }
    }

    fn json_type_of(v: &Value) -> &'static str {
        match v {
            Value::Bool(_) => "boolean",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Number(n) => {
                if n.is_f64() {
                    "number"
                } else {
                    "integer"
                }
            }
            Value::Null => "null",
        }
    }

    /// Schema properties match populated args by name + scalar type (catalog or raw TOOLS).
    fn assert_schema_matches(tool: &str, schema: &Value, populated: Value) {
        let props = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool} inputSchema has properties"));
        let fields = populated
            .as_object()
            .expect("args struct serializes to an object");

        let mut schema_keys: Vec<&String> = props.keys().collect();
        let mut struct_keys: Vec<&String> = fields.keys().collect();
        schema_keys.sort();
        struct_keys.sort();
        assert_eq!(
            schema_keys, struct_keys,
            "{tool}: inputSchema properties and args struct fields are out of sync"
        );

        for (name, prop) in props {
            if let Some(decl) = prop.get("type").and_then(|t| t.as_str()) {
                let actual = json_type_of(&fields[name]);
                assert_eq!(
                    decl, actual,
                    "{tool}.{name}: schema type `{decl}` != struct field type `{actual}`"
                );
            }
        }
    }

    /// Via filtered `catalog()` (respects `DIARIZATION_ENABLED`).
    fn assert_tool_matches(tool: &str, populated: Value) {
        let cat = ds_tools::catalog();
        let entry = cat
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == tool)
            .unwrap_or_else(|| panic!("{tool} in catalog"));
        assert_schema_matches(tool, &entry["inputSchema"], populated);
    }

    /// Via raw `ds_tools::TOOLS` — keeps diarize/manage_speakers parity even when hidden.
    fn assert_tool_matches_raw(tool: &str, populated: Value) {
        let schema =
            ds_tools::raw_input_schema(tool).unwrap_or_else(|| panic!("{tool} in ds_tools::TOOLS"));
        assert_schema_matches(tool, &schema, populated);
    }

    /// Drift guard: exhaustive literals (no `..`) break compile on new fields; schema
    /// name/type mismatch fails asserts. `rate` non-integral so it is `number` not `integer`.
    #[test]
    fn tool_schemas_match_arg_structs() {
        assert_tool_matches(
            "speak",
            serde_json::to_value(SpeakArgs {
                text: Some("hi".into()),
                voice: Some("af_sarah".into()),
                rate: Some(1.25),
            })
            .unwrap(),
        );
        assert_tool_matches(
            "list_voices",
            serde_json::to_value(ListVoicesArgs {
                tts_engine: Some(TtsEngine::Kokoro),
            })
            .unwrap(),
        );
        assert_tool_matches(
            "get_status",
            serde_json::to_value(StatusArgs { detail: Some(true) }).unwrap(),
        );
        assert_tool_matches(
            "get_usage",
            serde_json::to_value(UsageArgs {
                force_refresh: Some(true),
            })
            .unwrap(),
        );
        assert_tool_matches(
            "mute",
            serde_json::to_value(MuteArgs { on: Some(true) }).unwrap(),
        );
        assert_tool_matches(
            "listen",
            serde_json::to_value(ListenArgs { seconds: Some(10) }).unwrap(),
        );
        // Hidden from catalog when DIARIZATION_ENABLED is false — still parity-check raw.
        assert_tool_matches_raw(
            "diarize",
            serde_json::to_value(DiarizeArgs { seconds: Some(10) }).unwrap(),
        );
        assert_tool_matches_raw(
            "manage_speakers",
            serde_json::to_value(SpeakersArgs {
                action: Some("enroll".into()),
                name: Some("Alex".into()),
                seconds: Some(15),
            })
            .unwrap(),
        );
    }
}

#[cfg(test)]
mod usage_output {
    use super::*;
    use ds_agent_usage::{Period, UsageCard, UsageDeck, UsageRow};

    #[test]
    fn usage_returns_the_shared_deck_and_forwards_refresh() {
        let value = call_usage_with(&json!({ "force_refresh": true }), |force_refresh| {
            assert!(force_refresh);
            UsageDeck {
                cards: vec![UsageCard {
                    agent: ClientSource::Codex,
                    account: Some("dev@example.com".into()),
                    rows: vec![UsageRow {
                        period: Period::Week,
                        used_percent: 42.0,
                        resets_at_unix: 1_900_000_000,
                    }],
                }],
            }
        })
        .expect("usage serializes");

        assert_eq!(
            value,
            json!({
                "cards": [{
                    "agent": "codex",
                    "account": "dev@example.com",
                    "rows": [{
                        "period": "week",
                        "used_percent": 42.0,
                        "resets_at_unix": 1_900_000_000
                    }]
                }]
            })
        );
    }

    #[test]
    fn usage_defaults_to_the_soft_cache() {
        call_usage_with(&json!({}), |force_refresh| {
            assert!(!force_refresh);
            UsageDeck::empty()
        })
        .expect("empty deck serializes");
    }
}

#[cfg(test)]
mod status_output {
    use super::*;

    /// Regression: `engine` (config) and `state` (live) must both appear — two `"engine"`
    /// keys used to collide and drop the configured name.
    #[test]
    fn status_has_distinct_engine_and_state_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        let v = call_status(&paths, None, &json!({})).expect("status builds");

        let engine = v.get("engine").expect("`engine` key present");
        assert!(
            engine.is_string(),
            "`engine` must be a string (configured engine name), got {engine:?}"
        );
        assert!(
            matches!(engine.as_str(), Some("kokoro") | Some("system")),
            "`engine` should be a known engine token, got {engine:?}"
        );

        let state = v.get("state").expect("`state` key present");
        assert!(
            state.is_object(),
            "`state` must be an object, got {state:?}"
        );
        assert_eq!(
            state.get("running"),
            Some(&Value::Bool(false)),
            "with no socket the engine reports not running"
        );
    }

    /// `detail` is opt-in; engine-down degrades `models` to a note.
    #[test]
    fn status_detail_gates_the_models_section() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        let concise = call_status(&paths, None, &json!({})).unwrap();
        assert!(
            concise.get("models").is_none(),
            "concise status omits `models`"
        );

        let detailed = call_status(&paths, None, &json!({ "detail": true })).unwrap();
        let models = detailed
            .get("models")
            .expect("detail adds a `models` section");
        assert!(models.is_object(), "`models` is an object, got {models:?}");
    }
}

#[cfg(test)]
mod arg_validation {
    //! Validation branches that error before IPC — dummy socket must never be dialed.
    use super::*;

    fn dead_sock() -> PathBuf {
        PathBuf::from("__dontspeak_test_never_dialed__.sock")
    }

    #[test]
    fn speak_requires_nonempty_text() {
        let err = call_speak(&dead_sock(), &json!({}), ClientSource::ClaudeCode).unwrap_err();
        assert_eq!(err, "`text` is required.");

        let err = call_speak(
            &dead_sock(),
            &json!({ "text": "   " }),
            ClientSource::ClaudeCode,
        )
        .unwrap_err();
        assert_eq!(err, "`text` is required.");
    }

    #[test]
    fn mute_requires_on() {
        let err = call_mute(&dead_sock(), &json!({})).unwrap_err();
        assert_eq!(err, "`on` is required.");
    }

    #[test]
    fn stop_accepts_only_done_and_reports_its_scope() {
        assert_eq!(
            stop_response(Response::Done, true).unwrap(),
            "Stopped this session's speech."
        );
        assert_eq!(
            stop_response(Response::Done, false).unwrap(),
            "Stopped all speech."
        );
        assert_eq!(
            stop_response(Response::error("busy"), true).unwrap_err(),
            "stop_speech failed: busy"
        );
        assert_eq!(
            stop_response(Response::Pong, true).unwrap_err(),
            "stop_speech failed: unexpected engine response"
        );
    }

    #[test]
    fn mute_accepts_only_done() {
        assert_eq!(mute_response(Response::Done, true).unwrap(), "Muted.");
        assert_eq!(mute_response(Response::Done, false).unwrap(), "Unmuted.");
        assert_eq!(
            mute_response(Response::error("busy"), true).unwrap_err(),
            "mute failed: busy"
        );
        assert_eq!(
            mute_response(Response::Pong, true).unwrap_err(),
            "mute failed: unexpected engine response"
        );
    }

    #[test]
    fn speakers_requires_action() {
        let err = call_speakers(&dead_sock(), &json!({})).unwrap_err();
        assert_eq!(
            err,
            "manage_speakers: `action` is required (list | enroll | forget)"
        );
    }

    #[test]
    fn speakers_rejects_unknown_action() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "rename" })).unwrap_err();
        assert_eq!(
            err,
            "manage_speakers: unknown action `rename` (use list | enroll | forget)"
        );
    }

    #[test]
    fn speakers_enroll_requires_name() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "enroll" })).unwrap_err();
        assert_eq!(err, "manage_speakers: `name` is required for this action");

        let err =
            call_speakers(&dead_sock(), &json!({ "action": "enroll", "name": "  " })).unwrap_err();
        assert_eq!(err, "manage_speakers: `name` is required for this action");
    }

    #[test]
    fn speakers_forget_requires_name() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "forget" })).unwrap_err();
        assert_eq!(err, "manage_speakers: `name` is required for this action");
    }
}

#[cfg(test)]
mod engine_unavailable {
    //! Unbound socket → connect fails fast; handlers must fail closed, not panic/hang.
    use super::*;

    fn no_such_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("no-such-engine.sock");
        (dir, sock)
    }

    #[test]
    fn speak_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err =
            call_speak(&sock, &json!({ "text": "hello" }), ClientSource::ClaudeCode).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn stop_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_stop(&sock, ClientSource::ClaudeCode).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn mute_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_mute(&sock, &json!({ "on": true })).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn diarize_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_diarize(&sock, &json!({})).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn speakers_list_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_speakers(&sock, &json!({ "action": "list" })).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn speakers_enroll_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_speakers(
            &sock,
            &json!({ "action": "enroll", "name": "Alex", "seconds": 5 }),
        )
        .unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn speakers_forget_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_speakers(&sock, &json!({ "action": "forget", "name": "Alex" })).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }
}

#[cfg(test)]
mod set_config_tests {
    //! Empty-args rejection + single-field write; Reload to missing socket must not panic.
    use super::*;

    fn rooted_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        (dir, paths)
    }

    #[test]
    fn empty_args_require_at_least_one_setting() {
        let (_dir, paths) = rooted_paths();
        let err = call_set_config(&paths, &json!({})).unwrap_err();
        assert_eq!(err, "At least one setting required.");
        assert!(!paths.config_toml.exists());
    }

    #[test]
    fn a_single_field_change_writes_config_and_reports_it() {
        let (_dir, paths) = rooted_paths();
        assert!(!paths.config_toml.exists());

        let msg =
            call_set_config(&paths, &json!({ "tts_rate": 1.2 })).expect("a valid change applies");
        assert_eq!(msg, "Updated tts_rate=1.2.");
        assert!(paths.config_toml.exists(), "config.toml was written");

        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.tts_rate, 1.2);
    }
}

#[cfg(test)]
mod list_voices_tests {
    //! Pure config + catalog read (no IPC): engine override and `active` marking.
    use super::*;

    fn rooted_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        (dir, paths)
    }

    #[test]
    fn tts_engine_argument_overrides_the_configured_engine() {
        let (_dir, paths) = rooted_paths();
        let out = call_list_voices(&paths, &json!({ "tts_engine": "built_in" }))
            .expect("list_voices succeeds with no engine running");
        assert_eq!(out["engine"], json!("kokoro"));
        assert_eq!(out["language"], json!("en"));
    }

    /// Regression: `"off"` used to deserialize to deleted `TtsEngine::Off` and return an
    /// empty catalog; schema never listed it — now hard-errors at deserialize (parity).
    #[test]
    fn tts_engine_argument_off_is_now_a_hard_error_not_an_empty_list() {
        let (_dir, paths) = rooted_paths();
        let err = call_list_voices(&paths, &json!({ "tts_engine": "off" }))
            .expect_err("\"off\" is no longer a recognized tts_engine token");
        assert!(
            err.contains("invalid list_voices arguments") && err.contains("must be one of"),
            "got: {err}"
        );
    }

    #[test]
    fn current_voice_is_marked_active() {
        let (_dir, paths) = rooted_paths();
        let out = call_list_voices(&paths, &json!({ "tts_engine": "built_in" }))
            .expect("list_voices succeeds with no engine running");
        let cfg = VoiceConfig::load(&paths);
        let current = cfg.current_voice();

        let languages = out["languages"].as_array().expect("languages array");
        assert!(
            !languages.is_empty(),
            "expected at least one language group"
        );
        let mut found_active = false;
        for group in languages {
            for voice in group["voices"].as_array().expect("voices array") {
                let is_current = voice["id"].as_str() == Some(current.as_str());
                assert_eq!(
                    voice["active"],
                    json!(is_current),
                    "voice {:?} active flag should match whether it's the current voice",
                    voice["id"]
                );
                if is_current {
                    found_active = true;
                }
            }
        }
        assert!(
            found_active,
            "the current voice `{current}` must appear (and be marked active) in the catalog"
        );
    }
}
