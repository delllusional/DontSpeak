//! `tools/call` router and `call_*` handlers (strict arg structs). Most bridge to the
//! engine over `ds-ipc`; `voices`/`set_config`/`status`/`usage` are direct and never
//! spawn the engine (`set_config` still best-effort-nudges Reload).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ds_config::{ClientSource, Paths, TtsEngine, TtsModel, VoiceConfig};
use ds_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine_launch::ensure_engine;
use crate::mcp::{ok, structured_tool_result, tool_result};
use crate::voices::voice_groups;

/// Test helper; production uses [`tools_call_cancellable`].
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
        "voices" => match Paths::resolve() {
            Some(paths) => call_voices(&paths, &args).map(ToolSuccess::Structured),
            None => Err("Cannot resolve data paths.".into()),
        },
        // config.toml write; mtime-watch or Reload nudge. Engine need not be up.
        "set_config" => match Paths::resolve() {
            Some(paths) => call_set_config(&paths, &args).map(ToolSuccess::Text),
            None => Err("Cannot resolve data paths.".into()),
        },
        // Read-only; must not spawn engine / start playback.
        "status" => match Paths::resolve() {
            Some(paths) => call_status(&paths, sock, &args).map(ToolSuccess::Structured),
            None => Err("Cannot resolve data paths.".into()),
        },
        // Shared cache/provider logic with the Agents tab.
        "usage" => call_usage(&args).map(ToolSuccess::Structured),
        "speak" | "stop" | "mute" | "listen" | "diarize" | "manage_speakers" => {
            let Some(sock) = sock else {
                return ok(
                    id,
                    tool_result("Cannot resolve engine socket.".into(), true),
                );
            };
            ensure_engine(sock);
            match name {
                "speak" => call_speak(sock, &args, client).map(ToolSuccess::Text),
                "stop" => call_stop(sock, client).map(ToolSuccess::Text),
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

// Arg structs: deny_unknown_fields; fields == schema (pinned by tool_schemas_match_arg_structs).

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StatusArgs {
    detail: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UsageArgs {
    refresh: Option<bool>,
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
struct VoicesArgs {
    tts_engine: Option<TtsEngine>,
    tts_model: Option<TtsModel>,
    language: Option<String>,
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

fn call_voices(paths: &Paths, args: &Value) -> Result<Value, String> {
    let a: VoicesArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid voices arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // Explicit arg, else resolved TTS ladder (catalog still available when speech is off).
    let engine = a
        .tts_engine
        .or_else(|| cfg.resolved_tts())
        .unwrap_or(ds_config::TtsEngine::BuiltIn);
    let model = a.tts_model.unwrap_or(cfg.tts_model);
    // Normalize the explicit arg (parity with set_config); empty after trim = unset.
    let requested = a
        .language
        .as_deref()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty());
    let language = match (requested, engine) {
        (Some(l), _) => Some(l),
        (None, ds_config::TtsEngine::BuiltIn) => {
            Some(model.descriptor().default_language.to_string())
        }
        // System with no explicit language: NO filter — never inherit the built-in
        // model default (OmniVoice's "auto" would filter every system voice out).
        (None, ds_config::TtsEngine::System) => None,
    };
    if engine == ds_config::TtsEngine::BuiltIn {
        let lang = language.as_deref().unwrap_or_default();
        if !model.descriptor().supports_language(lang) {
            return Err(format!(
                "language `{lang}` is not supported by {}",
                model.as_str()
            ));
        }
    }
    let mut groups = voice_groups(engine, model, language.as_deref());
    let pool = match engine {
        ds_config::TtsEngine::BuiltIn => cfg.voices_for(model).to_vec(),
        ds_config::TtsEngine::System => cfg.tts_voices.system.clone(),
    };
    let languages: Vec<Value> = groups
        .iter_mut()
        .map(|(subtag, voices)| {
            for v in voices.iter_mut() {
                let id = v
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default()
                    .to_string();
                v["active"] = json!(pool.contains(&id));
            }
            json!({ "language": subtag, "voices": voices })
        })
        .collect();
    let out = json!({
        "engine": engine.as_str(),
        "model": (engine == ds_config::TtsEngine::BuiltIn).then(|| model.as_str()),
        "language": language,
        "languages": languages,
        "models": ds_config::TTS_MODELS.iter().map(|descriptor| json!({
            "id": descriptor.id,
            "name": descriptor.display_name,
            "default_language": descriptor.default_language,
            "languages": descriptor.languages,
            "providers": descriptor.providers.iter().map(|provider| provider.as_str()).collect::<Vec<_>>(),
            "supports_rate": descriptor.supports_rate,
            "supports_full_duplex": descriptor.supports_full_duplex,
        })).collect::<Vec<_>>(),
    });
    Ok(out)
}

/// Configured engine/voice/rate + live playback. `detail` adds model lifecycle/stats.
/// Read-only — never spawns the engine.
fn call_status(paths: &Paths, sock: Option<&PathBuf>, args: &Value) -> Result<Value, String> {
    let a: StatusArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid status arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // One round-trip feeds both the concise `state` block and `detail`.
    let probe = probe_engine(sock);
    // Keyed "state" not "engine" — serde_json keeps only the last of a duplicate key
    // (previously silently dropped the configured engine name).
    let state = match &probe {
        EngineProbe::Live(status) => {
            let activity = &status["activity"];
            // muted in concise status: narration path never calls a tool that reports it.
            json!({
                "running": true,
                "tts_active": activity["speaking"].as_bool().unwrap_or(false),
                "queued": status["stats"]["tts"]["queued"].as_u64().unwrap_or(0),
                "muted": activity["muted"].as_bool().unwrap_or(false),
            })
        }
        EngineProbe::Invalid => json!({ "running": true, "note": "unexpected engine response" }),
        EngineProbe::Unreachable => json!({ "running": false }),
        EngineProbe::Unresolved => {
            json!({ "running": false, "note": "cannot resolve engine socket" })
        }
    };
    let resolved_tts = cfg.resolved_tts();
    let voices = match resolved_tts {
        Some(ds_config::TtsEngine::BuiltIn) => cfg.active_voices().to_vec(),
        Some(ds_config::TtsEngine::System) => cfg.tts_voices.system.clone(),
        None => Vec::new(),
    };
    let mut out = json!({
        "engine": resolved_tts.map(|e| e.as_str()).unwrap_or("off"),
        "model": cfg.tts_model.as_str(),
        "language": "auto",
        "voices": voices,
        "rate": cfg.rate,
        "state": state,
    });
    if a.detail.unwrap_or(false) {
        out["status"] = match probe {
            EngineProbe::Live(status) => status,
            EngineProbe::Invalid => json!({ "running": false, "note": "invalid engine response" }),
            EngineProbe::Unreachable => json!({ "running": false, "note": "engine unavailable" }),
            EngineProbe::Unresolved => {
                json!({ "running": false, "note": "cannot resolve engine socket" })
            }
        };
    }
    Ok(out)
}

/// Outcome of the `status` tool's single `ModelStatus` probe. Read-only: an absent or
/// unreachable engine reports `running: false`, never an error.
enum EngineProbe {
    /// No socket path resolved.
    Unresolved,
    Unreachable,
    /// Engine answered, but not with a `model_status` object.
    Invalid,
    Live(Value),
}

fn probe_engine(sock: Option<&PathBuf>) -> EngineProbe {
    let Some(sock) = sock else {
        return EngineProbe::Unresolved;
    };
    match ds_ipc::request(sock, &Request::ModelStatus) {
        Ok(Response::ModelStatus { status }) if status.is_object() => EngineProbe::Live(status),
        Ok(_) => EngineProbe::Invalid,
        Err(_) => EngineProbe::Unreachable,
    }
}

fn call_usage(args: &Value) -> Result<Value, String> {
    call_usage_with(args, ds_agent_usage::snapshot)
}

fn call_usage_with(
    args: &Value,
    snapshot: impl FnOnce(bool) -> ds_agent_usage::UsageDeck,
) -> Result<Value, String> {
    let a: UsageArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid usage arguments: {e}"))?;
    serde_json::to_value(snapshot(a.refresh.unwrap_or(false)))
        .map_err(|e| format!("usage failed: {e}"))
}

// config.toml is source of truth; engine Reload nudge (mtime-watch if down).

fn call_set_config(paths: &Paths, args: &Value) -> Result<String, String> {
    // SetConfigArgs single settable surface — deny_unknown_fields + schema parity with ds-tools.
    let parsed: ds_tools::SetConfigArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid set_config arguments: {e}"))?;

    // System STT: only persist when *running* engine can authorize on-device. Gate on whether
    // the new preference *resolves* to System — not merely whether caller named `system`.
    // Engine down ⇒ refuse (never enable blindly). apply() rejects unusable static choices.
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

/// Ambient Claude session (stdio = one process per session). Claude sets
/// `CLAUDE_CODE_SESSION_ID` (undocumented for MCP; claude-code #41836). Never a tool arg.
/// `None` ⇒ machine-global.
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
    // Scope barge to ambient session; None (bare CLI) → global hard barge.
    let session = session_id();
    let scoped = session.is_some();
    let response = ds_ipc::request(
        sock,
        &Request::Stop {
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
        Response::Error { message } => Err(format!("stop failed: {message}")),
        _ => Err("stop failed: unexpected engine response".into()),
    }
}

/// Same `SetMuted` path as tray / Caps-Lock — tool- and app-driven mute cannot diverge.
/// Unlike `stop`, silences future output too until changed or restart.
fn call_mute(sock: &Path, args: &Value) -> Result<String, String> {
    let a: MuteArgs =
        serde_json::from_value(args.clone()).map_err(|e| format!("invalid mute arguments: {e}"))?;
    let Some(on) = a.on else {
        return Err("`on` is required.".into());
    };
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

/// One-shot diarization. Engine blocks ≤60s (within IPC timeout) — no stream.
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

/// Voiceprints for `diarize` labels. Schema can't express "name only for enroll/forget" —
/// validated per action.
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

/// Trailing silence before listen finalizes (after speech started). Long enough that a
/// between-sentence breath doesn't cut a multi-sentence answer short.
const LISTEN_ENDPOINT_SILENCE: Duration = Duration::from_millis(1500);

/// Live session → transcript. Auto-stops on end-of-speech (like Caps-Lock, not a fixed window).
/// EOS from Partial stream (engine emits Partial only when transcript *changes*); watchdog on
/// a second connection sends `TestRecognitionStop` after silence or hard cap. Cancellable +
/// joined so a late stop cannot leak onto a later session.
fn call_listen(sock: &Path, args: &Value, cancelled: Arc<AtomicBool>) -> Result<String, String> {
    use std::sync::atomic::AtomicU64;

    let a: ListenArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid listen arguments: {e}"))?;
    let max_secs = a.seconds.unwrap_or(30).clamp(1, 60);

    let mut client = ds_ipc::connect(sock).map_err(|e| format!("engine unavailable: {e}"))?;
    client
        .send(&Request::TestRecognitionStart)
        .map_err(|e| format!("listen failed to start: {e}"))?;

    // `spoke` gates silence so leading quiet never ends early; last_change_ms resets on Partial.
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
                _ => return,
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

    /// Drift guard: every catalog name must hit a real arm. Bogus args + no socket keep
    /// paths side-effect-free; reject only `unknown tool:`.
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

    /// Schema properties match populated args by name + scalar type.
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

    /// Raw TOOLS — keeps diarize/manage_speakers parity even when hidden.
    fn assert_tool_matches_raw(tool: &str, populated: Value) {
        let schema =
            ds_tools::raw_input_schema(tool).unwrap_or_else(|| panic!("{tool} in ds_tools::TOOLS"));
        assert_schema_matches(tool, &schema, populated);
    }

    /// Drift guard: exhaustive literals break compile on new fields; schema name/type mismatch
    /// fails asserts. `rate` non-integral so it is `number` not `integer`.
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
            "voices",
            serde_json::to_value(VoicesArgs {
                tts_engine: Some(TtsEngine::BuiltIn),
                tts_model: Some(TtsModel::Kokoro),
                language: Some("en".into()),
            })
            .unwrap(),
        );
        assert_tool_matches(
            "status",
            serde_json::to_value(StatusArgs { detail: Some(true) }).unwrap(),
        );
        assert_tool_matches(
            "usage",
            serde_json::to_value(UsageArgs {
                refresh: Some(true),
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
        let value = call_usage_with(&json!({ "refresh": true }), |refresh| {
            assert!(refresh);
            UsageDeck {
                cards: vec![UsageCard {
                    agent: ClientSource::Codex,
                    account: Some("dev@example.com".into()),
                    rows: vec![UsageRow {
                        period: Period::Week,
                        used_percent: 42.0,
                        resets_at_unix: 1_900_000_000,
                    }],
                    needs_auth: false,
                }],
            }
        })
        .expect("usage serializes");

        // needs_auth skip-when-false (legacy decks omit the key).
        assert!(
            !value["cards"][0]
                .as_object()
                .unwrap()
                .contains_key("needs_auth")
        );
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
    fn guarded_card_serializes_needs_auth() {
        let value = call_usage_with(&json!({}), |_| UsageDeck {
            cards: vec![UsageCard {
                agent: ClientSource::ClaudeCode,
                account: None,
                rows: Vec::new(),
                needs_auth: true,
            }],
        })
        .expect("usage serializes");

        assert_eq!(value["cards"][0]["needs_auth"], json!(true));
    }

    #[test]
    fn usage_defaults_to_the_soft_cache() {
        call_usage_with(&json!({}), |refresh| {
            assert!(!refresh);
            UsageDeck::empty()
        })
        .expect("empty deck serializes");
    }
}

#[cfg(test)]
mod status_output {
    use super::*;

    /// Regression: `engine` (config) and `state` (live) must both appear — duplicate keys
    /// used to drop the configured name.
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
            matches!(engine.as_str(), Some("built_in") | Some("system")),
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

    #[test]
    fn status_detail_gates_the_status_section() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        let concise = call_status(&paths, None, &json!({})).unwrap();
        assert!(
            concise.get("status").is_none(),
            "concise status omits nested `status`"
        );

        let detailed = call_status(&paths, None, &json!({ "detail": true })).unwrap();
        let status = detailed
            .get("status")
            .expect("detail adds a nested `status` section");
        assert!(status.is_object(), "`status` is an object, got {status:?}");
    }
}

#[cfg(test)]
mod arg_validation {
    //! Error before IPC — dummy socket must never be dialed.
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
            "stop failed: busy"
        );
        assert_eq!(
            stop_response(Response::Pong, true).unwrap_err(),
            "stop failed: unexpected engine response"
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
    fn speakers_enroll_and_forget_require_name() {
        for action in ["enroll", "forget"] {
            let err = call_speakers(&dead_sock(), &json!({ "action": action })).unwrap_err();
            assert_eq!(
                err, "manage_speakers: `name` is required for this action",
                "{action}"
            );
        }
        let err =
            call_speakers(&dead_sock(), &json!({ "action": "enroll", "name": "  " })).unwrap_err();
        assert_eq!(err, "manage_speakers: `name` is required for this action");
    }
}

#[cfg(test)]
mod engine_unavailable {
    //! Unbound socket → fail closed, not panic/hang.
    use super::*;

    fn no_such_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("no-such-engine.sock");
        (dir, sock)
    }

    #[test]
    fn every_ipc_tool_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let calls: &[(&str, Result<String, String>)] = &[
            (
                "speak",
                call_speak(&sock, &json!({ "text": "hello" }), ClientSource::ClaudeCode),
            ),
            ("stop", call_stop(&sock, ClientSource::ClaudeCode)),
            ("mute", call_mute(&sock, &json!({ "on": true }))),
            ("diarize", call_diarize(&sock, &json!({}))),
            (
                "speakers list",
                call_speakers(&sock, &json!({ "action": "list" })),
            ),
            (
                "speakers enroll",
                call_speakers(
                    &sock,
                    &json!({ "action": "enroll", "name": "Alex", "seconds": 5 }),
                ),
            ),
            (
                "speakers forget",
                call_speakers(&sock, &json!({ "action": "forget", "name": "Alex" })),
            ),
        ];
        for (name, result) in calls {
            let err = result.as_ref().unwrap_err();
            assert!(
                err.starts_with("engine unavailable: "),
                "{name}: got: {err}"
            );
        }
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

        let msg = call_set_config(&paths, &json!({ "rate": 1.2 })).expect("a valid change applies");
        assert_eq!(msg, "Updated rate=1.2.");
        assert!(paths.config_toml.exists(), "config.toml was written");

        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.rate, 1.2);
    }
}

#[cfg(test)]
mod voices_tests {
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
        let out = call_voices(&paths, &json!({ "tts_engine": "built_in" }))
            .expect("voices succeeds with no engine running");
        assert_eq!(out["engine"], json!("built_in"));
        assert_eq!(out["language"], json!("en"));
    }

    #[test]
    fn language_argument_is_trimmed_and_lowercased() {
        // Registry-only model (chatterbox) keeps this disk-free.
        let (_dir, paths) = rooted_paths();
        let out = call_voices(
            &paths,
            &json!({ "tts_engine": "built_in", "tts_model": "chatterbox", "language": " RU " }),
        )
        .expect("normalized language is accepted");
        assert_eq!(out["language"], json!("ru"));
    }

    #[test]
    fn every_pool_voice_is_marked_active() {
        // `active` = pool membership: ALL configured voices flag, not just entry 0 —
        // there is no privileged "default" slot in the pool.
        // Pool ids must come from `KOKORO_FALLBACK_IDS` — machines without the real
        // voices bin (CI) only catalog those, and the test must pass on both.
        let (_dir, paths) = rooted_paths();
        std::fs::write(
            &paths.config_toml,
            "[tts_voices]\nkokoro = [\"am_michael\", \"bf_emma\"]\n",
        )
        .unwrap();
        let out = call_voices(&paths, &json!({ "tts_engine": "built_in" }))
            .expect("voices succeeds with no engine running");
        let pool = VoiceConfig::load(&paths).active_voices().to_vec();
        assert_eq!(pool, vec!["am_michael", "bf_emma"]);

        let languages = out["languages"].as_array().expect("languages array");
        assert!(
            !languages.is_empty(),
            "expected at least one language group"
        );
        let mut active_seen = 0;
        for group in languages {
            for voice in group["voices"].as_array().expect("voices array") {
                let in_pool = voice["id"]
                    .as_str()
                    .is_some_and(|id| pool.iter().any(|p| p == id));
                assert_eq!(
                    voice["active"],
                    json!(in_pool),
                    "voice {:?} active flag should match pool membership",
                    voice["id"]
                );
                if in_pool {
                    active_seen += 1;
                }
            }
        }
        assert_eq!(
            active_seen,
            pool.len(),
            "every pool voice must appear (and be marked active) in the catalog"
        );
    }
}
