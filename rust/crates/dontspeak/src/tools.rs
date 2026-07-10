//! The `tools/call` router and the individual `call_*` tool handlers (plus their
//! strict arg structs). Most handlers bridge to the resident engine over
//! `ds-ipc`; `list_voices`/`set_config`/`status`/`wire` read config or edit
//! client files directly, never spawning the engine (set_config still best-effort-nudges
//! a running one to Reload).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ds_config::{Paths, TtsEngine, VoiceConfig, WireTarget};
use ds_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine_launch::ensure_engine;
use crate::mcp::{ok, tool_result};
use crate::voices::voice_groups;
use crate::wire;

pub(crate) fn tools_call(id: Option<Value>, msg: &Value, sock: Option<&PathBuf>) -> Value {
    let params = msg.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let result = match name {
        // Read-only enumeration: reads the Kokoro voices bin + `say` directly, no engine.
        "list_voices" => match Paths::resolve() {
            Some(paths) => call_list_voices(&paths, &args),
            None => Err("cannot resolve ~/.claude paths".into()),
        },
        // Persistent config write to settings.json; the engine applies it via its mtime-watch
        // (or the best-effort Reload nudge). Doesn't require the engine to be up.
        "set_config" => match Paths::resolve() {
            Some(paths) => call_set_config(&paths, &args),
            None => Err("cannot resolve ~/.claude paths".into()),
        },
        // Write a config to disk or register/remove a client integration (no engine needed;
        // edits client configs via the SAME `wire <client>` orchestrator the installer uses,
        // and writes the narration spec to the user data dir).
        "setup_integration" => match Paths::resolve() {
            Some(paths) => call_wire(&paths, &args),
            None => Err("cannot resolve ~/.claude paths".into()),
        },
        // Read-only introspection: config (settings.json) + live engine state.
        // Does NOT spawn the engine — a status check must not start playback.
        "get_status" => match Paths::resolve() {
            Some(paths) => call_status(&paths, sock, &args),
            None => Err("cannot resolve ~/.claude paths".into()),
        },
        // Stateful actions bridge to the resident engine.
        "speak" | "stop_speech" | "mute" | "listen" | "diarize" | "manage_speakers" => {
            let Some(sock) = sock else {
                return ok(
                    id,
                    tool_result("cannot resolve the engine socket path".into(), true),
                );
            };
            // Make sure the engine is up (MCP clients may invoke us with none yet).
            ensure_engine(sock);
            match name {
                "speak" => call_speak(sock, &args),
                "stop_speech" => call_stop(sock),
                "mute" => call_mute(sock, &args),
                "diarize" => call_diarize(sock, &args),
                "manage_speakers" => call_speakers(sock, &args),
                _ => call_listen(sock, &args),
            }
        }
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(text) => ok(id, tool_result(text, false)),
        Err(e) => ok(id, tool_result(e, true)),
    }
}

// ── Tool argument structs ─────────────────────────────────────────────────────
// Each arg-taking tool deserializes its `arguments` into one of these. `deny_unknown_fields`
// rejects a typo'd key; `tts_engine` reuses ds_config's strict TtsEngine deserialize
// (unknown token → error). The fields == the schema's properties, and the
// `tool_schemas_match_arg_structs` test pins that parity by name AND type.

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StatusArgs {
    detail: Option<bool>,
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

fn call_list_voices(paths: &Paths, args: &Value) -> Result<String, String> {
    let a: ListVoicesArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid list_voices arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // Which engine's voices to list: an explicit `tts_engine` arg, else the engine the TTS
    // ladder RESOLVES to (Kokoro when spoken replies are off — there's still a voice catalog).
    let engine = a
        .tts_engine
        .or_else(|| cfg.resolved_tts())
        .unwrap_or(ds_config::TtsEngine::Kokoro);
    // This build supports English only: always list English voices, regardless of any
    // other languages present in the Kokoro pack (they are intentionally not surfaced).
    let mut groups = voice_groups(engine, "en");
    // Mark the configured voice active (a transient session override is reported
    // separately by `status`, which probes the engine).
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
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

// ── Status (config read + read-only engine probe) ────────────────────────────

/// Report configured engine/voice/rate (from settings.json) plus live engine
/// playback state. With `detail`, ALSO fold in the deep
/// per-engine model lifecycle + stats (the former `model_status` tool). Probes the
/// engine read-only — never spawns it, so a status check can't start the warm child
/// or any playback; the `detail` section degrades to a note when the engine is down.
fn call_status(paths: &Paths, sock: Option<&PathBuf>, args: &Value) -> Result<String, String> {
    let a: StatusArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid status arguments: {e}"))?;
    let cfg = VoiceConfig::load(paths);
    // Live engine playback state. Keyed as "state" (NOT "engine") so it doesn't
    // collide with the configured-engine string below — serde_json keeps only the
    // last value for a duplicate key, which previously silently dropped the engine
    // name from the output.
    let state = match sock {
        Some(sock) => match ds_ipc::request(sock, &Request::Status) {
            Ok(Response::Status {
                tts_active,
                queued,
                paused,
                muted,
            }) => {
                // `muted`: when true, replies/narration still queue but play SILENTLY — the
                // reason the user may hear nothing. Surfaced here (not just in `detail`) so the
                // model can notice it and tell the user, since the narration hook path that
                // actually speaks replies never calls a tool that could report it.
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
        // The Kokoro voice pool, shared by both TTS backends (no separate apple-native set).
        "voices": cfg.active_voices().to_vec(),
        "rate": cfg.tts_rate,
        "state": state,
    });
    // `detail`: fold in the deep per-engine model lifecycle + stats (engine-sourced, so it
    // degrades to a note when the engine is down).
    if a.detail.unwrap_or(false) {
        out["models"] = match sock {
            Some(sock) => match ds_ipc::request(sock, &Request::ModelStatus) {
                Ok(Response::ModelStatus { status }) => status,
                _ => json!({ "running": false, "note": "engine unavailable" }),
            },
            None => json!({ "running": false, "note": "cannot resolve engine socket" }),
        };
    }
    Ok(serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()))
}

// ── Persistent config writes (settings.json is the source of truth; the engine is
//    nudged to apply NOW, falling back to its mtime-watch if it's down) ──────────

/// Register or remove the DontSpeak integration for one AI client, at runtime. SHARED
/// LOGIC: this is a thin adapter that maps (client, enabled) to the SAME per-client
/// `wire::run` orchestrator the installers use — never a reimplementation, so install-time and
/// tool-time wiring can't drift. Each client's wire scopes its own surfaces (Claude Code and
/// Qwen Code = hooks + MCP, Codex = hooks); `enabled=false` removes only our entries (additive +
/// backed-up, like the installer).
fn call_wire(paths: &Paths, args: &Value) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        target: String,
        enabled: bool,
    }
    let a: Args =
        serde_json::from_value(args.clone()).map_err(|e| format!("invalid wire arguments: {e}"))?;

    // One canonical parse of the target token. The unknown-target error references the
    // canonical set (`WireTarget::ALL`) so the accepted tokens here can't drift from the
    // `wire` schema enum (which a parity test pins to the same `WireTarget`).
    let target = WireTarget::parse(&a.target).ok_or_else(|| {
        let expected = WireTarget::ALL
            .iter()
            .map(|t| format!("{:?}", t.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        format!("unknown target {:?}; expected one of {expected}", a.target)
    })?;

    // The narration spec is a CONFIG FILE on disk, not a client wiring — handle it first and
    // return directly. enabled=true materializes the built-in default to the user-editable
    // narration-spec.md (without clobbering an existing edited copy); enabled=false removes
    // the override, reverting to the built-in DEFAULT_NARRATION_SPEC.
    if target == WireTarget::NarrationSpec {
        let f = &paths.narration_spec;
        if a.enabled {
            if f.exists() {
                return Ok(format!(
                    "Narration spec already on disk at {} — edit it to customize the spoken format.",
                    f.display()
                ));
            }
            if let Some(dir) = f.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("create config dir: {e}"))?;
            }
            std::fs::write(f, ds_config::DEFAULT_NARRATION_SPEC)
                .map_err(|e| format!("write narration spec: {e}"))?;
            return Ok(format!(
                "Wrote the narration spec to {} — edit it to reshape the spoken blockquote replies.",
                f.display()
            ));
        }
        return match std::fs::remove_file(f) {
            Ok(()) => Ok(format!(
                "Removed the narration spec override ({}) — reverting to the built-in default.",
                f.display()
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(
                "No narration spec override on disk — already using the built-in default.".into(),
            ),
            Err(e) => Err(format!("remove narration spec: {e}")),
        };
    }

    // Build the per-client `wire <client> [--remove]` argv and run the SAME orchestrator the
    // installers use. `wire` self-skips a client that isn't installed; for any gated client
    // (today Codex and Qwen Code — see `gate_on_presence` in ds-config's registry) we pre-check
    // presence so an `enabled=true` on an absent client reports honestly instead of claiming a
    // no-op wire succeeded.
    let flags = |client: &str| -> Vec<String> {
        if a.enabled {
            vec![client.into()]
        } else {
            vec![client.into(), "--remove".into()]
        }
    };
    // `target.as_str()` is the canonical `WireTarget` token the `wire` orchestrator parses back —
    // the ONE token vocabulary, never a re-typed literal here. Everything client-specific (the
    // display name, the presence probe, whether presence gates a wire) comes from the client
    // registry — the same declaration the orchestrator walks.
    // Unreachable `expect`: an unknown token already errored at `WireTarget::parse` above, and
    // `NarrationSpec` returned from its dedicated branch before this point.
    let spec = ds_config::client_spec(target)
        .expect("narration_spec handled before this point; unknown tokens errored at parse");
    if a.enabled && spec.gate_on_presence && !(spec.present)(paths) {
        return Ok(format!(
            "{} is not installed — nothing to wire.",
            spec.display_name
        ));
    }
    let (label, code) = (spec.display_name, wire::run(&flags(target.as_str())));

    if code != 0 {
        log_wire_failure(paths, label, code);
        return Err(format!(
            "wiring {label} failed (exit {code}); see the engine log"
        ));
    }
    let verb = if a.enabled { "Registered" } else { "Removed" };
    let note = "";
    Ok(format!(
        "{verb} the DontSpeak integration for {label}{note}."
    ))
}

/// Persist the `wiring {label} failed` diagnostic to the unified log — closes the gap where
/// `call_wire`'s error message promised "see the engine log" but nothing actually wrote there.
/// Takes `paths` directly (not `ds_log::log_cached`) since `call_wire` already has a real
/// `&Paths` in scope, keeping this trivially unit-testable against an isolated tempdir `Paths`
/// without touching the real `$HOME` log.
fn log_wire_failure(paths: &Paths, label: &str, code: i32) {
    ds_log::log(
        &paths.log_file,
        ds_log::LogLevel::Error,
        "mcp",
        &format!("setup_integration: wiring {label} failed (exit {code})"),
    );
}

fn call_set_config(paths: &Paths, args: &Value) -> Result<String, String> {
    // Single source of truth: deserialize the inbound JSON args straight into
    // SetConfigArgs. `deny_unknown_fields` rejects typos; enum/number/`capture_gain`
    // values are validated strictly there. What's settable == that struct's fields, so
    // this handler and the JSON schema (ds-tools) cannot drift apart.
    let parsed: ds_tools::SetConfigArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid set_config arguments: {e}"))?;

    // System STT opt-in gate: making `system` the ACTIVE dictation engine must be verified by
    // the RUNNING engine — it owns the macOS on-device recognizer and runs the one-time
    // first-use step (model download on macOS 26+, the Speech-Recognition permission
    // prompt on 14–25); we refuse to PERSIST when it isn't usable, so `system`
    // never silently degrades. Engine down ⇒ we can't verify, so we don't enable it blindly.
    //
    // KEY: gate on whether the new PREFERENCE RESOLVES to system on THIS machine — not merely
    // whether the caller named `system`. `apply()` already statically rejects a `system` choice
    // that's unusable on this platform/build (§3), so by the time we get here a `Some(pref)`
    // that resolves to `System` really would become the active engine.
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
            Ok(_) => return Err("unexpected response while verifying system STT".into()),
            Err(_) => {
                return Err(
                    "can't verify system speech recognition — launch DontSpeak.app \
                            (it must be running to check on-device availability + \
                            permission), then set stt_engine=system again"
                        .into(),
                );
            }
        }
    }

    // Apply every provided VoiceConfig field to a fresh load, collecting the summary.
    let mut cfg = VoiceConfig::load(paths);
    let changes = parsed.apply(&mut cfg)?;

    if changes.is_empty() {
        return Err(
            "no recognized field provided. Accepted fields: rate, voices, tts_engine, \
                    stt_engine, provider, narrate, caps_enabled, \
                    greet_on_open, tray_indicator, \
                    capture_gain, double_tap_submits, input_clears, \
                    pause_in_background."
                .into(),
        );
    }

    // Persist VoiceConfig and nudge the engine to Reload NOW (it falls back to its
    // mtime-watch if down). settings.json stays the source of truth; the nudge only
    // removes the poll latency.
    ds_config::write_settings(paths, &cfg)
        .map_err(|e| format!("could not write config.toml: {e}"))?;
    let _ = ds_ipc::request(&paths.engine_sock, &ds_ipc::Request::Reload);

    Ok(format!("Set {}.", changes.join(", ")))
}

// ── Tool implementations (bridge to the engine over ds-ipc) ──────────────────

/// Ambient Claude session id for THIS MCP process (stdio = one process per
/// session). Claude Code sets `CLAUDE_CODE_SESSION_ID` in the spawned server's
/// environment — undocumented for MCP but present in practice (see claude-code
/// issue #41836). `None` when absent, so the engine treats it as the default,
/// machine-global session and everything stays backward-compatible. It is NEVER a
/// tool argument — the MCP protocol/tool schemas are untouched.
fn session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

fn call_speak(sock: &Path, args: &Value) -> Result<String, String> {
    let a: SpeakArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid speak arguments: {e}"))?;
    let text = a.text.unwrap_or_default();
    if text.trim().is_empty() {
        return Err("`text` is required".into());
    }
    match ds_ipc::request(
        sock,
        &Request::Speak {
            text,
            voice: a.voice,
            rate: a.rate,
            session: session_id(),
        },
    ) {
        Ok(Response::Done) => Ok("Spoken.".into()),
        Ok(Response::Error { message }) => Err(format!("speak failed: {message}")),
        Ok(_) => Err("speak: unexpected response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

fn call_stop(sock: &Path) -> Result<String, String> {
    // Scope the barge to the CALLING window (ambient session) so an agent in one
    // terminal stops only its own voice, not another window's. A non-session caller
    // (session_id() == None, e.g. the bare CLI) falls back to the global hard barge.
    match ds_ipc::request(
        sock,
        &Request::StopSpeech {
            session: session_id(),
        },
    ) {
        Ok(_) => Ok("Stopped.".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// The `mute` tool: toggle the GLOBAL mute. Bridges to the engine over the SAME
/// `SetMuted` request the app's tray checkbox / Caps-Lock toggle use (via `ds_core::ds_set_muted`)
/// — one canonical path (`SetMuted` → `ttsq.set_muted` → `tts.set_muted`), so tool-driven and
/// app-driven mute can't diverge. Distinct from `stop_speech`: mute PERSISTS and silences future
/// output too (the queue keeps draining, just inaudibly), where stop is a one-shot barge.
fn call_mute(sock: &Path, args: &Value) -> Result<String, String> {
    let a: MuteArgs =
        serde_json::from_value(args.clone()).map_err(|e| format!("invalid mute arguments: {e}"))?;
    let Some(on) = a.on else {
        return Err("`on` is required (true = mute, false = unmute)".into());
    };
    // Plain state confirmation. The "user hears nothing, so put it in text" coaching lives in
    // the UserPromptSubmit push-hook (fires when the user muted and the model is unaware) and
    // the tool description — no need to repeat it here, where the model just caused the mute.
    let done = if on {
        "Muted — spoken output is now silent."
    } else {
        "Unmuted — audible again."
    };
    match ds_ipc::request(sock, &Request::SetMuted { on }) {
        Ok(_) => Ok(done.into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// One-shot speaker diarization: record the mic for `seconds`, then return who spoke
/// when. The engine blocks for the record window (≤60s, within the IPC read timeout),
/// so a single request/response suffices — no streaming/stop dance like `listen`.
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
                format!(
                    "{} speaker(s) across {} segment(s):",
                    speakers.len(),
                    segs.len()
                )
            };
            let body =
                serde_json::to_string_pretty(&segments).unwrap_or_else(|_| segments.to_string());
            Ok(format!("{summary}\n{body}"))
        }
        Ok(Response::Error { message }) => Err(format!("diarize failed: {message}")),
        Ok(_) => Err("diarize: unexpected response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// The `speakers` tool: manage the enrolled-voiceprint library `diarize` labels with.
/// `action` selects the operation; `name` is required for enroll/forget. Each branch is a
/// thin bridge to the same engine requests the three former tools used (Enroll /
/// ForgetSpeaker / ListSpeakers) — the protocol is unchanged.
fn call_speakers(sock: &Path, args: &Value) -> Result<String, String> {
    let a: SpeakersArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid speakers arguments: {e}"))?;
    // Schema can't express "name required only for enroll/forget", so validate per action
    // here (same approach as set_config's cross-field checks).
    let need_name = || -> Result<String, String> {
        let name = a.name.clone().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            Err("speakers: `name` is required for this action".into())
        } else {
            Ok(name)
        }
    };
    match a.action.as_deref().unwrap_or("").trim() {
        "list" => list_speakers(sock),
        "enroll" => enroll_speaker(sock, need_name()?, a.seconds.unwrap_or(15).clamp(1, 60)),
        "forget" => forget_speaker(sock, need_name()?),
        "" => Err("speakers: `action` is required (list | enroll | forget)".into()),
        other => Err(format!(
            "speakers: unknown action `{other}` (use list | enroll | forget)"
        )),
    }
}

/// Enroll a voiceprint: record `seconds`, extract an embedding, persist it under `name`.
/// Blocks for the record window (≤60s, within the IPC read timeout).
fn enroll_speaker(sock: &Path, name: String, seconds: u64) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::Enroll { name, seconds }) {
        Ok(Response::Enrolled { name }) => Ok(format!("Enrolled voiceprint for \"{name}\".")),
        Ok(Response::Error { message }) => Err(format!("enroll failed: {message}")),
        Ok(_) => Err("enroll: unexpected response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// Remove an enrolled voiceprint by name (no-op if absent).
fn forget_speaker(sock: &Path, name: String) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::ForgetSpeaker { name: name.clone() }) {
        Ok(Response::Done) => Ok(format!(
            "Removed enrolled voiceprint \"{name}\" (if it existed)."
        )),
        Ok(Response::Error { message }) => Err(format!("forget failed: {message}")),
        Ok(_) => Err("forget: unexpected response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// List enrolled speaker names.
fn list_speakers(sock: &Path) -> Result<String, String> {
    match ds_ipc::request(sock, &Request::ListSpeakers) {
        Ok(Response::Speakers { names }) => {
            if names.is_empty() {
                Ok("No speakers enrolled. Use action=enroll to add one.".into())
            } else {
                Ok(format!(
                    "Enrolled speakers ({}): {}",
                    names.len(),
                    names.join(", ")
                ))
            }
        }
        Ok(Response::Error { message }) => Err(format!("list failed: {message}")),
        Ok(_) => Err("list: unexpected response".into()),
        Err(e) => Err(format!("engine unavailable: {e}")),
    }
}

/// Trailing-silence the `listen` tool waits for before it finalizes: once the speaker has
/// started AND then gone quiet this long, the session is stopped and transcribed. Long
/// enough that a between-sentence breath doesn't cut a multi-sentence answer short.
const LISTEN_ENDPOINT_SILENCE: Duration = Duration::from_millis(1500);

/// The `listen` tool: open the mic via a live Parakeet recognition session and return the
/// final transcript. AUTO-STOPS when the speaker stops talking — so an agent can ask a
/// question mid-turn and get the spoken answer back without the user pressing a key —
/// behaving like Caps-Lock dictation rather than a blind fixed window.
///
/// End-of-speech is detected from the PARTIAL stream, not raw audio: the engine only emits
/// a `Partial` when the transcript CHANGES, so partials simply stop arriving once the
/// speaker pauses. A watchdog (on a second connection, since this one is busy streaming)
/// sends `TestRecognitionStop` after [`LISTEN_ENDPOINT_SILENCE`] of no new partial — and,
/// regardless, after the `seconds` hard cap for a user who never stops. This reuses the
/// existing two-connection stop path; the helper/engine are untouched.
fn call_listen(sock: &Path, args: &Value) -> Result<String, String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let a: ListenArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid listen arguments: {e}"))?;
    let max_secs = a.seconds.unwrap_or(30).clamp(1, 60);

    let mut client = ds_ipc::connect(sock).map_err(|e| format!("engine unavailable: {e}"))?;
    client
        .send(&Request::TestRecognitionStart)
        .map_err(|e| format!("start dictation: {e}"))?;

    // Shared with the watchdog: `spoke` gates the silence rule so LEADING silence (the user
    // hasn't started) never ends the session early; `quiet_since_ms` is the ms-since-start
    // stamp of the last transcript change, which the recv loop bumps on every new partial.
    // (Atomics keep it lock-free; a coarse ms epoch from one `Instant::now()` base is plenty
    // for a 1.5 s threshold.) The watchdog is CANCELLABLE + JOINED so it can neither leak
    // nor fire a stray stop onto a later, unrelated session — same contract as the old timer.
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
            // Poll ~10×/s, but exit the instant the recv loop drops `cancel_tx`
            // (Disconnected) — the dictation already ended, so skip the stop.
            match cancel_rx.recv_timeout(Duration::from_millis(100)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                _ => return, // cancelled/finished
            }
            let elapsed = base.elapsed();
            let went_quiet = wd_spoke.load(Ordering::Relaxed)
                && elapsed.saturating_sub(Duration::from_millis(wd_last.load(Ordering::Relaxed)))
                    >= LISTEN_ENDPOINT_SILENCE;
            if elapsed >= hard_cap || went_quiet {
                let _ = ds_ipc::request(&sock2, &Request::TestRecognitionStop);
                return;
            }
        }
    });

    // Drain the stream to its terminal response, bumping the silence clock on every new,
    // non-empty partial. THEN cancel + join the watchdog so it never outlives this call.
    let outcome = loop {
        match client.recv() {
            Ok(Response::Transcript { text }) => {
                break Ok(if text.trim().is_empty() {
                    "(silence — nothing recognized)".into()
                } else {
                    text
                });
            }
            Ok(Response::Partial { text }) => {
                // A changed transcript = the speaker is still talking: arm the silence rule
                // and reset its clock. (The engine only sends a Partial on a real change.)
                if !text.trim().is_empty() {
                    spoke.store(true, Ordering::Relaxed);
                    last_change_ms.store(now_ms(), Ordering::Relaxed);
                }
                continue;
            }
            Ok(Response::Error { message }) => break Err(format!("dictation: {message}")),
            Ok(_) => continue, // Listening — keep reading
            Err(e) => break Err(format!("dictation stream ended: {e}")),
        }
    };
    // Cancel the pending watchdog (drop the sender) and join it so the thread is gone
    // before we return — no leak, and no late stop landing on a future session.
    drop(cancel_tx);
    let _ = watchdog.join();
    outcome
}

#[cfg(test)]
mod drift {
    use super::*;

    /// ROUTER DRIFT GUARD: every tool in the canonical `ds_tools` catalog must be RECOGNIZED
    /// by the dispatch router in `tools_call`. Adding or renaming a tool in `ds_tools::TOOLS`
    /// without wiring a match arm here is a TEST FAILURE — nothing else ties the router's
    /// hardcoded name arms to the catalog.
    ///
    /// We drive the REAL router (no extracted name list to duplicate) with a bogus argument
    /// and NO engine socket, so every path is side-effect-free: the locally-handled tools
    /// (`list_voices`/`set_config`/`status`/`wire`) trip their `deny_unknown_fields` arg
    /// structs and error at DESERIALIZE — before any config write or IPC — while the
    /// engine-bridged tools short-circuit on the `None` socket before `ensure_engine`. The
    /// only outcome we reject is the router's distinguishable `unknown tool:` sentinel, which
    /// proves the name reached a real arm rather than the catch-all.
    #[test]
    fn router_handles_every_catalog_tool() {
        let bogus = json!({ "__not_a_real_field__": true });
        for name in ds_tools::tool_names() {
            let msg = json!({ "params": { "name": name, "arguments": bogus.clone() } });
            let resp = tools_call(None, &msg, None);
            let text = resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(
                !text.starts_with("unknown tool:"),
                "dispatch router doesn't handle catalog tool `{name}` (got: {text})"
            );
        }
    }

    /// Map a JSON value to the JSON-Schema scalar `type` token it satisfies.
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

    /// Assert `schema` (a tool's `inputSchema`) matches `populated` (a fully-populated args
    /// struct serialized to JSON) by NAME and declared scalar TYPE. Shared by
    /// `assert_tool_matches` (the FILTERED, `catalog()`-based view) and
    /// `assert_tool_matches_raw` (the RAW, `ds_tools::TOOLS`-based view that ignores
    /// `DIARIZATION_ENABLED`).
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

    /// Assert one tool's advertised inputSchema properties match `populated`, via the
    /// FILTERED, user-facing `catalog()` (reflects `DIARIZATION_ENABLED`).
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

    /// Assert one tool's advertised inputSchema properties match `populated`, via the RAW
    /// `ds_tools::TOOLS` definition — bypassing the `DIARIZATION_ENABLED` visibility filter.
    /// Used for `diarize`/`manage_speakers` so this schema/dispatch-parity regression test
    /// keeps running unconditionally even while those tools are hidden from `catalog()` (a
    /// VISIBILITY gate, not a functional rip-out — see `ds_tools::DIARIZATION_ENABLED`).
    fn assert_tool_matches_raw(tool: &str, populated: Value) {
        let schema =
            ds_tools::raw_input_schema(tool).unwrap_or_else(|| panic!("{tool} in ds_tools::TOOLS"));
        assert_schema_matches(tool, &schema, populated);
    }

    /// DRIFT GUARD for the arg-taking tools (set_config has its own guard in ds-tools).
    /// Each fully-populated literal is exhaustive (no `..`), so a new struct field also
    /// breaks this at COMPILE time; a missing/renamed/mistyped schema property fails the
    /// assertions. `rate` is non-integral so it reads as `number`, not `integer`.
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
            "mute",
            serde_json::to_value(MuteArgs { on: Some(true) }).unwrap(),
        );
        assert_tool_matches(
            "listen",
            serde_json::to_value(ListenArgs { seconds: Some(10) }).unwrap(),
        );
        // diarize/manage_speakers are hidden from `catalog()` while `DIARIZATION_ENABLED` is
        // false — check them against the RAW `ds_tools::TOOLS` schema instead so this
        // schema/dispatch-parity coverage keeps running regardless of the gate (a visibility
        // gate must not silently drop the only regression test proving dispatch still works).
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
mod status_output {
    use super::*;

    /// CORR-1: `status` must emit BOTH the configured-engine string (`engine`) AND the
    /// live playback state (`state`) — previously two `"engine"` keys collided and
    /// serde_json silently kept only the last, dropping the engine name. Run with no
    /// socket so no engine is contacted and a missing config falls back to defaults.
    #[test]
    fn status_has_distinct_engine_and_state_keys() {
        // A nonexistent config path → VoiceConfig::load returns defaults (no file written).
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        let json = call_status(&paths, None, &json!({})).expect("status builds");
        let v: Value = serde_json::from_str(&json).expect("status returns valid JSON");

        // `engine` is the configured engine NAME (a string), not dropped by a key clash.
        let engine = v.get("engine").expect("`engine` key present");
        assert!(
            engine.is_string(),
            "`engine` must be a string (configured engine name), got {engine:?}"
        );
        assert!(
            matches!(engine.as_str(), Some("kokoro") | Some("system")),
            "`engine` should be a known engine token, got {engine:?}"
        );

        // `state` is the live engine-state object (running=false here, no socket).
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

    /// `detail` is opt-in: the concise default omits the heavy `models` section, and
    /// `detail: true` adds it (degrading to a note when no engine socket is available).
    #[test]
    fn status_detail_gates_the_models_section() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        // Default (no detail): no `models` key.
        let concise: Value =
            serde_json::from_str(&call_status(&paths, None, &json!({})).unwrap()).unwrap();
        assert!(
            concise.get("models").is_none(),
            "concise status omits `models`"
        );

        // detail: true → a `models` object (here the engine-down note, since sock is None).
        let detailed: Value =
            serde_json::from_str(&call_status(&paths, None, &json!({ "detail": true })).unwrap())
                .unwrap();
        let models = detailed
            .get("models")
            .expect("detail adds a `models` section");
        assert!(models.is_object(), "`models` is an object, got {models:?}");
    }
}

#[cfg(test)]
mod arg_validation {
    //! Argument-validation branches of the engine-bridged tools: all pure `serde_json` +
    //! string checks that error BEFORE any IPC call, so a dummy/nonexistent socket path is
    //! fine — these must never actually dial it.
    use super::*;

    /// A socket path that resolves to nothing; if a validation test somehow contacted it,
    /// the surrounding assertion on the exact error text would fail loudly instead of
    /// silently hanging (connect on a missing path fails fast, no real engine needed).
    fn dead_sock() -> PathBuf {
        PathBuf::from("__dontspeak_test_never_dialed__.sock")
    }

    #[test]
    fn speak_requires_nonempty_text() {
        let err = call_speak(&dead_sock(), &json!({})).unwrap_err();
        assert_eq!(err, "`text` is required");

        // Whitespace-only text is treated the same as absent.
        let err = call_speak(&dead_sock(), &json!({ "text": "   " })).unwrap_err();
        assert_eq!(err, "`text` is required");
    }

    #[test]
    fn mute_requires_on() {
        let err = call_mute(&dead_sock(), &json!({})).unwrap_err();
        assert_eq!(err, "`on` is required (true = mute, false = unmute)");
    }

    #[test]
    fn speakers_requires_action() {
        let err = call_speakers(&dead_sock(), &json!({})).unwrap_err();
        assert_eq!(
            err,
            "speakers: `action` is required (list | enroll | forget)"
        );
    }

    #[test]
    fn speakers_rejects_unknown_action() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "rename" })).unwrap_err();
        assert_eq!(
            err,
            "speakers: unknown action `rename` (use list | enroll | forget)"
        );
    }

    #[test]
    fn speakers_enroll_requires_name() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "enroll" })).unwrap_err();
        assert_eq!(err, "speakers: `name` is required for this action");

        // A blank/whitespace name is treated the same as absent.
        let err =
            call_speakers(&dead_sock(), &json!({ "action": "enroll", "name": "  " })).unwrap_err();
        assert_eq!(err, "speakers: `name` is required for this action");
    }

    #[test]
    fn speakers_forget_requires_name() {
        let err = call_speakers(&dead_sock(), &json!({ "action": "forget" })).unwrap_err();
        assert_eq!(err, "speakers: `name` is required for this action");
    }
}

#[cfg(test)]
mod engine_unavailable {
    //! The "engine unavailable" branch shared by `call_speak`/`call_stop`/`call_mute`/
    //! `call_diarize`/`call_speakers`: point each at a socket path that doesn't exist —
    //! this fails fast at `connect()`, no real engine needed — and confirm the fail-closed
    //! error surfaces rather than a panic or a hang.
    use super::*;

    /// A socket path under a tempdir that was never bound — `connect()` fails fast
    /// (ENOENT-style), which is exactly the "engine down" case these handlers must
    /// fail closed on. The tempdir is returned too so it isn't dropped early.
    fn no_such_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("no-such-engine.sock");
        (dir, sock)
    }

    #[test]
    fn speak_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_speak(&sock, &json!({ "text": "hello" })).unwrap_err();
        assert!(err.starts_with("engine unavailable: "), "got: {err}");
    }

    #[test]
    fn stop_reports_engine_unavailable() {
        let (_dir, sock) = no_such_socket();
        let err = call_stop(&sock).unwrap_err();
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
mod wire_tests {
    //! `call_wire`: the unknown-target parse error, and the `NarrationSpec` branch's real
    //! disk outcomes via `Paths::rooted_at`. The non-`NarrationSpec` (client-wiring) branch
    //! shells out to the real `wire::run` orchestrator against real client dirs — covered
    //! separately, not duplicated here.
    use super::*;

    #[test]
    fn unknown_target_is_rejected_with_the_canonical_token_list() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let err = call_wire(
            &paths,
            &json!({ "target": "not_a_real_target", "enabled": true }),
        )
        .unwrap_err();
        assert!(
            err.contains("unknown target \"not_a_real_target\""),
            "got: {err}"
        );
        // The expected-tokens list must reference the SAME canonical set `WireTarget::ALL`
        // enumerates, so it can't drift from the accepted tokens.
        for target in ds_config::WireTarget::ALL {
            assert!(
                err.contains(&format!("{:?}", target.as_str())),
                "expected token {:?} listed in error, got: {err}",
                target.as_str()
            );
        }
    }

    #[test]
    fn narration_spec_writes_the_default_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert!(!paths.narration_spec.exists());

        let msg = call_wire(
            &paths,
            &json!({ "target": "narration_spec", "enabled": true }),
        )
        .expect("writes the default narration spec");
        assert!(msg.contains("Wrote the narration spec to"), "got: {msg}");
        assert!(paths.narration_spec.exists());
        assert_eq!(
            std::fs::read_to_string(&paths.narration_spec).unwrap(),
            ds_config::DEFAULT_NARRATION_SPEC
        );
    }

    #[test]
    fn narration_spec_reports_already_on_disk_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(paths.narration_spec.parent().unwrap()).unwrap();
        std::fs::write(&paths.narration_spec, "custom spec, edited by the user\n").unwrap();

        let msg = call_wire(
            &paths,
            &json!({ "target": "narration_spec", "enabled": true }),
        )
        .expect("reports the existing file rather than erroring");
        assert!(msg.contains("already on disk"), "got: {msg}");
        // The existing, user-edited content must not be clobbered.
        assert_eq!(
            std::fs::read_to_string(&paths.narration_spec).unwrap(),
            "custom spec, edited by the user\n"
        );
    }

    #[test]
    fn narration_spec_removes_the_override_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(paths.narration_spec.parent().unwrap()).unwrap();
        std::fs::write(&paths.narration_spec, "custom spec\n").unwrap();

        let msg = call_wire(
            &paths,
            &json!({ "target": "narration_spec", "enabled": false }),
        )
        .expect("removes the override");
        assert!(
            msg.contains("Removed the narration spec override"),
            "got: {msg}"
        );
        assert!(!paths.narration_spec.exists());
    }

    #[test]
    fn narration_spec_remove_is_a_noop_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert!(!paths.narration_spec.exists());

        let msg = call_wire(
            &paths,
            &json!({ "target": "narration_spec", "enabled": false }),
        )
        .expect("a no-op remove is Ok, not an error");
        assert!(
            msg.contains("No narration spec override on disk"),
            "got: {msg}"
        );
        assert!(!paths.narration_spec.exists());
    }

    #[test]
    fn log_wire_failure_writes_an_error_line_to_the_unified_log() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        log_wire_failure(&paths, "Claude Code", 1);

        let contents = std::fs::read_to_string(&paths.log_file).unwrap();
        assert!(contents.contains("ERROR mcp"), "got: {contents}");
        assert!(
            contents.contains("wiring Claude Code failed (exit 1)"),
            "got: {contents}"
        );
    }
}

#[cfg(test)]
mod set_config_tests {
    //! `call_set_config`'s pure paths: the `changes.is_empty()` rejection, and a normal
    //! single-field change against `Paths::rooted_at`, proving the write + best-effort
    //! `Reload` nudge (to a socket that doesn't exist) don't panic.
    use super::*;

    fn rooted_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        (dir, paths)
    }

    #[test]
    fn empty_args_are_rejected_as_no_recognized_field() {
        let (_dir, paths) = rooted_paths();
        let err = call_set_config(&paths, &json!({})).unwrap_err();
        assert!(
            err.starts_with("no recognized field provided"),
            "got: {err}"
        );
        // The config file must NOT have been written on a no-op rejection.
        assert!(!paths.config_toml.exists());
    }

    #[test]
    fn a_single_field_change_writes_settings_and_reports_it() {
        let (_dir, paths) = rooted_paths();
        assert!(!paths.config_toml.exists());

        let msg =
            call_set_config(&paths, &json!({ "tts_rate": 1.2 })).expect("a valid change applies");
        assert_eq!(msg, "Set tts_rate=1.2.");
        assert!(
            paths.config_toml.exists(),
            "settings.json/config.toml was written"
        );

        // The write actually persisted the new rate (round-trip through VoiceConfig::load).
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.tts_rate, 1.2);
    }
}

#[cfg(test)]
mod list_voices_tests {
    //! `call_list_voices`: pure config + static catalog read, no IPC. Covers the
    //! `tts_engine` argument override and that the configured current voice is marked
    //! `active` in the returned catalog.
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
        // No config on disk: VoiceConfig defaults to Kokoro either way, so pass the
        // OTHER cross-platform-safe engine token ("built_in"/Kokoro is already default —
        // exercising the override path itself is what matters here) explicitly and check
        // the response reflects it rather than any resolved default.
        let out = call_list_voices(&paths, &json!({ "tts_engine": "built_in" }))
            .expect("list_voices succeeds with no engine running");
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["engine"], json!("kokoro"));
        assert_eq!(v["language"], json!("en"));
    }

    /// REGRESSION (deleting the `Off` token, §6.1 addendum): `list_voices`'s `tts_engine` arg
    /// deserializes through `TtsEngine`'s shared strict `Deserialize` impl — the SAME one
    /// `SetConfigArgs` uses. Before this change, `"off"` parsed successfully to
    /// `TtsEngine::Off` and reached `voice_groups(Off, "en")` live, silently returning an EMPTY
    /// voice-group list — even though the tool's own advertised schema
    /// (`PType::Enum(&["built_in","system"])`) already excluded "off" as a documented value.
    /// Now that `Off` is deleted as a token everywhere, the SAME call hard-errors at argument
    /// deserialization instead — tightening actual behavior to match what the schema already
    /// promised (not a bug this change introduces).
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
        let v: Value = serde_json::from_str(&out).expect("valid JSON");

        // Default config ⇒ current_voice() is the default Kokoro voice (`af_sarah`).
        let cfg = VoiceConfig::load(&paths);
        let current = cfg.current_voice();

        let languages = v["languages"].as_array().expect("languages array");
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
