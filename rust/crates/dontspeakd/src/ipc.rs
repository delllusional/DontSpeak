//! Engine RPC accept loop + request dispatch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_config::{CancelSpeechScope, Paths, TtsArgPools, VoiceConfig, WiredAgent};

use crate::status::{EngineShared, model_status_json};
use crate::stt_test::TestSession;
use crate::ttsq::TtsQueue;

/// Request log with wired-client attribution when known.
fn log_client(paths: &Paths, client: Option<WiredAgent>, msg: &str) {
    match client {
        Some(client) => ds_log::log_from(
            &paths.log_file,
            ds_log::LogLevel::Info,
            "engine",
            client,
            msg,
        ),
        None => ds_log::log(&paths.log_file, ds_log::LogLevel::Info, "engine", msg),
    }
}

/// Cancel on MarkActive? Skip voice-submit echoes (`clear_on_input` already applied).
pub(crate) fn should_cancel_on_submit(was_voice: bool, scope_configured: bool) -> bool {
    !was_voice && scope_configured
}

/// Missing session → active stream (not global).
fn earcon_session(ttsq: &TtsQueue, requested: Option<String>) -> Option<String> {
    requested.or_else(|| ttsq.active_session())
}

/// Reject, don't clamp: `TtsArgPools::parse` already refuses an unsupported target language,
/// so an unroutable per-utterance voice must refuse too rather than silently substitute.
fn speak_tts_args(value: Option<serde_json::Value>) -> Result<Option<TtsArgPools>, String> {
    value
        .as_ref()
        .map(|value| {
            let pools = TtsArgPools::parse(value)?;
            ds_tts::enumerate::validate_speak_voices(&pools)?;
            Ok(pools)
        })
        .transpose()
        .map_err(|error: String| format!("invalid tts_args: {error}"))
}

fn models_response(
    paths: &Paths,
    downloads: &crate::downloads::DownloadProg,
    remove: Option<&str>,
) -> ds_ipc::Response {
    match ds_model::ModelRoots::ambient() {
        Some(roots) => crate::models::respond(paths, downloads, &roots, remove),
        None => ds_ipc::Response::error("models: cannot resolve the model directory"),
    }
}

fn agent_usage_response(refresh: bool) -> ds_ipc::Response {
    agent_usage_response_with(refresh, ds_agent_usage::snapshot)
}

fn agent_usage_response_with(
    refresh: bool,
    snapshot: impl FnOnce(bool) -> ds_agent_usage::UsageDeck,
) -> ds_ipc::Response {
    match serde_json::to_value(snapshot(refresh)) {
        Ok(deck) => ds_ipc::Response::AgentUsage { deck },
        Err(error) => ds_ipc::Response::error(format!("agent usage: {error}")),
    }
}

struct HookSessions {
    logical: Option<String>,
    queue: Option<String>,
}

/// UserPromptSubmit MarkActive. Always nudge codex sessions; Grok also. Skip terminal
/// claim + `clear_on_input` when `synthetic` (#11).
fn handle_mark_active(
    ttsq: &TtsQueue,
    codex_sessions: &crate::codex_stream::SessionRegistry,
    grok_sessions: &crate::grok_stream::SessionRegistry,
    paths: &Paths,
    sessions: HookSessions,
    synthetic: bool,
    source: Option<WiredAgent>,
) {
    // Unconditional liveness nudge (+ re-discovery / negative-cache re-arm).
    if let Some(s) = &sessions.logical {
        codex_sessions.nudge(s);
        if source == Some(WiredAgent::Grok) {
            grok_sessions.nudge(s);
        }
    }
    ttsq.link_sessions(
        sessions.logical.as_deref(),
        sessions.queue.as_deref(),
        source,
    );
    if synthetic {
        return; // no active-terminal steal, no TTS queue touch
    }
    ttsq.set_active_session(sessions.queue.clone());
    // Voice-submit echo: engine already applied clear_on_input; skip re-cancel.
    let was_voice = ttsq.take_recent_voice_submit();
    // Skip config read when was_voice already forces no cancel.
    if !was_voice {
        let scopes = VoiceConfig::load(paths).clear_on_input;
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Current)) {
            ttsq.clear_session(sessions.queue.clone());
        }
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Other)) {
            // Use this request's session, not re-read active (concurrent MarkActive race).
            ttsq.cancel_for_submit(sessions.queue, false, true);
        }
    }
}

/// RPC accept loop on a dedicated thread. `Reload` flips `reload_requested`; other arms
/// drive TTS queue, status, STT test, provider, enroll/diarize.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_ipc_server(
    shared: EngineShared,
    paths: Paths,
    stt_test: Arc<TestSession>,
    ttsq: Arc<TtsQueue>,
    reload_requested: Arc<AtomicBool>,
    codex_sessions: Arc<crate::codex_stream::SessionRegistry>,
    grok_sessions: Arc<crate::grok_stream::SessionRegistry>,
) {
    let sock = paths.engine_sock.clone();
    std::thread::spawn(move || {
        // Bad-request sink: hooks discard the reply, so without this WARN deploy skew
        // (stale CLI missing `--client`) silently kills the voice loop. See log_client.
        let log_paths = paths.clone();
        let on_bad_request = move |detail: &str| {
            ds_log::log(
                &log_paths.log_file,
                ds_log::LogLevel::Warn,
                "engine",
                &format!(
                    "{detail} — caller and engine are out of sync; \
                     reinstall the CLI and restart the app"
                ),
            );
        };
        let capture_busy = Arc::new(AtomicBool::new(false));
        let handler = move |req: ds_ipc::Request, emit: &mut dyn FnMut(&ds_ipc::Response)| {
            match req {
                ds_ipc::Request::Ping => emit(&ds_ipc::Response::Pong),
                ds_ipc::Request::EnsureCodexStream { codex_bin } => {
                    match codex_sessions
                        .ensure_remote(codex_bin.into(), std::time::Duration::from_secs(20))
                    {
                        Ok(endpoint) => emit(&ds_ipc::Response::CodexStreamReady { endpoint }),
                        Err(message) => emit(&ds_ipc::Response::error(message)),
                    }
                }
                ds_ipc::Request::GreetSession {
                    session,
                    queue_session,
                    source,
                } => {
                    // New terminal opened → greet in its agent's assigned voice (no-op unless
                    // `greet` is set). Claims the agent's voice at open time.
                    // Also the codex_stream supervisor's session DISCOVERY: a session id
                    // the hooks vouch for may map to a codex app-server thread (CC/Qwen
                    // ids simply never match one).
                    //
                    // Every client-originated arm below logs at INFO through `log_from`, which
                    // renders the trailing `client=<token>` — so the activity log always names
                    // WHICH client caused the line. `paths` is in scope here (a tempdir-rooted
                    // one under test), which is exactly why these are `log_from` calls and never
                    // `ds_log::log_cached*` (that resolves the REAL per-OS `$HOME` path).
                    log_client(
                        &paths,
                        source,
                        &format!(
                            "greet_session session={} queue_session={}",
                            session.as_deref().unwrap_or("-"),
                            queue_session.as_deref().unwrap_or("-")
                        ),
                    );
                    if let Some(s) = &session {
                        codex_sessions.nudge(s);
                        if source == Some(WiredAgent::Grok) {
                            grok_sessions.nudge(s);
                        }
                    }
                    ttsq.link_sessions(session.as_deref(), queue_session.as_deref(), source);
                    ttsq.greet_session(source, queue_session);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::MarkActive {
                    session,
                    queue_session,
                    synthetic,
                    source,
                } => {
                    log_client(
                        &paths,
                        source,
                        &format!(
                            "mark_active session={} queue_session={} synthetic={synthetic}",
                            session.as_deref().unwrap_or("-"),
                            queue_session.as_deref().unwrap_or("-")
                        ),
                    );
                    handle_mark_active(
                        &ttsq,
                        &codex_sessions,
                        &grok_sessions,
                        &paths,
                        HookSessions {
                            logical: session,
                            queue: queue_session,
                        },
                        synthetic,
                        source,
                    );
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Speak {
                    text,
                    tts_args,
                    session,
                    source,
                } => {
                    // Explicit (MCP `speak` tool) reply → enqueue on the TTS queue (the
                    // single serializer onto the warm child). The queue worker picks the
                    // engine from live config (or this session's override) and gates on
                    // the mic.
                    log_client(
                        &paths,
                        source,
                        &format!("speak session={} chars={}", session, text.chars().count()),
                    );
                    match speak_tts_args(tts_args)
                        .and_then(|args| ttsq.enqueue(text, args, source, Some(session)))
                    {
                        // Blank text is accepted and says nothing, so there is no handle.
                        Ok(Some(id)) => emit(&ds_ipc::Response::Utterance { id }),
                        Ok(None) => emit(&ds_ipc::Response::Done),
                        Err(e) => emit(&ds_ipc::Response::error(format!("speak: {e}"))),
                    }
                }
                ds_ipc::Request::SpeakNarration {
                    text,
                    detection_text,
                    session,
                    narration_id,
                    source,
                } => {
                    // Mid-turn narration → enqueue onto the same bounded FIFO as everything
                    // else (no kind). Warm path: no per-block model reload.
                    //
                    // Success is deliberately NOT logged: it fires once per blockquote line and
                    // would spam the activity log. Identified retries return the same success
                    // without adding a second queue item.
                    match ttsq.enqueue_narration(
                        text,
                        source,
                        session,
                        narration_id,
                        detection_text,
                    ) {
                        Ok(()) => emit(&ds_ipc::Response::Done),
                        Err(e) => {
                            log_client(&paths, source, &format!("narration rejected: {e}"));
                            emit(&ds_ipc::Response::error(format!("speak narration: {e}")));
                        }
                    }
                }
                ds_ipc::Request::SetMuted { on } => {
                    // Global mute: built-in drains silently; system TTS skips new speech
                    // and kills any in-flight OS synthesizer (no fade).
                    ttsq.set_muted(on);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SetDictationUiReceiver { receiver_id, ttl_ms } => {
                    let ttl = std::time::Duration::from_millis(ttl_ms.clamp(500, 60_000));
                    *shared
                        .dictation_ui_lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now() + ttl);
                    shared.gate.bump();
                    log_client(&paths, None, &format!("dictation UI receiver={receiver_id}"));
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Stop { session, source } => {
                    // MCP stop is per-window: prune only that session's items and cancel
                    // playback only if it is that session's, so one terminal never
                    // silences another.
                    // Also clear the Grok sticky sibling (`grok-stop:<id>`): MarkActive
                    // current-clear leaves sticky intact by design, but an explicit stop
                    // must silence digests + ding co-queued under that tag.
                    log_client(&paths, source, &format!("stop session={session}"));
                    let sticky = format!("grok-stop:{session}");
                    ttsq.clear_session(Some(session));
                    ttsq.clear_session(Some(sticky));
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SessionEnd {
                    session,
                    queue_session,
                    source,
                } => {
                    // Window closed for good: per-window barge. The agent's voice assignment
                    // is keyed by client, not session, and deliberately survives.
                    // Missing queue identity → global hard barge for sessionless hooks.
                    // Grok: also drop the updates.jsonl tail registration.
                    log_client(
                        &paths,
                        source,
                        &format!(
                            "session_end session={} queue_session={}",
                            session.as_deref().unwrap_or("-"),
                            queue_session.as_deref().unwrap_or("-")
                        ),
                    );
                    if let Some(session) = session.as_deref() {
                        ttsq.forget_narration_session(session);
                        if source == Some(WiredAgent::Grok) {
                            grok_sessions.forget(session);
                        }
                    }
                    if queue_session.is_some() {
                        ttsq.end_session(queue_session.clone());
                    } else {
                        ttsq.clear();
                    }
                    ttsq.unlink_sessions(session.as_deref(), queue_session.as_deref());
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::TestRecognitionStart => {
                    // Streams Listening/Partial then a terminal Transcript.
                    stt_test.run(emit);
                }
                ds_ipc::Request::TestRecognitionStop => {
                    stt_test.stop();
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::ModelStatus => {
                    emit(&ds_ipc::Response::ModelStatus {
                        status: model_status_json(&shared, &paths, || ttsq.tts_status_sample()),
                    });
                }
                ds_ipc::Request::AgentUsage { refresh } => {
                    emit(&agent_usage_response(refresh));
                }
                ds_ipc::Request::WaitModelStatus { since, timeout_ms } => {
                    let expired = {
                        let mut lease = shared
                            .dictation_ui_lease
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if lease.is_some_and(|expires| expires <= std::time::Instant::now()) {
                            *lease = None;
                            true
                        } else {
                            false
                        }
                    };
                    if expired {
                        shared.gate.bump();
                    }
                    // PUSH transport: block this (dedicated) connection until the
                    // dictation status changes or the cap elapses, then reply with the
                    // fresh snapshot. One-thread-per-connection (see ipc server), so this
                    // never stalls the timer's ModelStatus / SetMuted on other connections.
                    let timeout = std::time::Duration::from_millis(timeout_ms.clamp(1, 60_000));
                    shared.gate.wait_changed(since, timeout);
                    emit(&ds_ipc::Response::ModelStatus {
                        status: model_status_json(&shared, &paths, || ttsq.tts_status_sample()),
                    });
                }
                ds_ipc::Request::SetProvider { provider } => {
                    // set_provider restarts the warm child (which hosts BOTH Kokoro and
                    // Parakeet) and resets both engines' stats when the active provider
                    // actually changes — centralized in restart_child, so this path AND
                    // the set_config/config-reload path (apply_tts_provider) both get it.
                    shared.tts.set_provider(&provider);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Reload => {
                    // The MCP/GUI wrote config.toml and asks us to apply it now.
                    // Flip the same flag SIGHUP uses; the poll loop reloads next tick
                    // (debounced, re-reading config.toml). No mtime wait.
                    reload_requested.store(true, Ordering::Relaxed);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Earcon {
                    event,
                    session,
                    source,
                } => {
                    // Completion cues share the session FIFO with speech, so Stop/Notification
                    // can never overtake narration already admitted for that turn — except the
                    // needs-input bypass while the background focus hold has playback idle
                    // (see `TtsQueue::dispatch_earcon`).
                    let session = earcon_session(&ttsq, session);
                    log_client(
                        &paths,
                        source,
                        &format!(
                            "earcon event={} session={}",
                            event.as_str(),
                            session.as_deref().unwrap_or("-")
                        ),
                    );
                    match ttsq.dispatch_earcon(event, source, session) {
                        Ok(()) => emit(&ds_ipc::Response::Done),
                        Err(e) => emit(&ds_ipc::Response::error(format!("earcon: {e}"))),
                    }
                }
                ds_ipc::Request::AuthorizeSystemStt => {
                    // Opt-in gate for `stt_engine=system`: prompt for Speech Recognition
                    // authorization (attributed to this app process) + verify on-device
                    // capability. Done ⇒ usable; Error ⇒ the reason set_config relays so it
                    // refuses to enable rather than silently falling back.
                    match ds_stt::system_authorize() {
                        Ok(()) => emit(&ds_ipc::Response::Done),
                        Err(reason) => emit(&ds_ipc::Response::error(format!(
                            "system STT unavailable: {reason}"
                        ))),
                    }
                }
                ds_ipc::Request::Diarize { seconds } => {
                    // One-shot record-then-diarize on the warm helper. Blocks this
                    // connection for ~`seconds`, then returns the segments (labelled with
                    // enrolled names where a cluster matches a stored voiceprint).
                    let ttsq = ttsq.clone();
                    match run_bounded_capture(
                        &shared.stt_active,
                        &capture_busy,
                        "diarize",
                        seconds,
                        move |secs| ttsq.diarize(secs),
                    ) {
                        Ok(json) => match diarize_named_segments(&json, &paths) {
                            Ok(segments) => emit(&ds_ipc::Response::Diarization { segments }),
                            Err(e) => emit(&ds_ipc::Response::error(format!("diarize: {e}"))),
                        },
                        Err(e) => emit(&ds_ipc::Response::error(e)),
                    }
                }
                ds_ipc::Request::Enroll { name, seconds } => {
                    // Record a sample, extract a voiceprint, persist it under `name`.
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        emit(&ds_ipc::Response::error("enroll: name must not be empty"));
                    } else {
                        let ttsq = ttsq.clone();
                        match run_bounded_capture(
                            &shared.stt_active,
                            &capture_busy,
                            "enroll",
                            seconds,
                            move |secs| ttsq.enroll(secs),
                        ) {
                            Ok(emb) => {
                                let mut store = ds_config::SpeakerStore::load(&paths.speakers_json);
                                store.upsert(name.clone(), emb);
                                match store.save(&paths.speakers_json) {
                                    Ok(()) => emit(&ds_ipc::Response::Enrolled { name }),
                                    Err(e) => emit(&ds_ipc::Response::error(format!(
                                        "enroll: save failed: {e}"
                                    ))),
                                }
                            }
                            Err(e) => emit(&ds_ipc::Response::error(e)),
                        }
                    }
                }
                ds_ipc::Request::ForgetSpeaker { name } => {
                    let mut store = ds_config::SpeakerStore::load(&paths.speakers_json);
                    store.remove(&name);
                    match store.save(&paths.speakers_json) {
                        Ok(()) => emit(&ds_ipc::Response::Done),
                        Err(e) => emit(&ds_ipc::Response::error(format!("forget_speaker: {e}"))),
                    }
                }
                ds_ipc::Request::ListSpeakers => {
                    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
                    emit(&ds_ipc::Response::Speakers {
                        names: store.names(),
                    });
                }
                // The model root is resolved ONCE here; `ds_model::inventory` is entirely
                // root-parameterized so no test can reach the real cache through it.
                ds_ipc::Request::ListModels => {
                    emit(&models_response(&paths, &shared.downloads, None));
                }
                ds_ipc::Request::RemoveModel { id } => {
                    emit(&models_response(&paths, &shared.downloads, Some(&id)));
                }
            }
        };
        if let Err(e) = ds_ipc::serve(&sock, handler, on_bad_request) {
            log::warn!(target: "engine", "IPC server exited: {e}");
        }
    });
}

/// Run a blocking warm-helper call (diarize/enroll) with a bounded wait. Both
/// `TtsManager::diarize` and `TtsManager::enroll` block this thread on a condvar with
/// no timeout of their own, so a wedged or unresponsive (not crashed) `ds-helper` child
/// would otherwise leave the calling IPC connection blocked indefinitely. Runs `f` on a
/// detached thread and waits at most `timeout` for its result; past the deadline this
/// returns a timeout error and lets the connection reply instead of hanging — the
/// spawned thread finishes (or stays wedged) on its own, off this connection.
fn call_with_timeout<T: Send + 'static>(
    timeout: std::time::Duration,
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out waiting for the warm helper",
        ))
    })
}

/// Shared plumbing for the `Diarize`/`Enroll` arms: both refuse while Caps-Lock dictation
/// owns the warm helper's ONE capture thread, both clamp the requested duration into
/// `1..=60` seconds, and both run the actual capture through [`call_with_timeout`] with a
/// `+30s` grace window. `op_label` (`"diarize"` / `"enroll"`) prefixes both the busy-refusal
/// and the propagated capture error so callers see the same messages as before this was
/// factored out. `f` receives the CLAMPED seconds, never the raw request value.
fn run_bounded_capture<T: Send + 'static>(
    stt_active: &AtomicBool,
    capture_busy: &AtomicBool,
    op_label: &str,
    seconds: u64,
    f: impl FnOnce(u64) -> std::io::Result<T> + Send + 'static,
) -> Result<T, String> {
    if stt_active.load(Ordering::Relaxed) {
        // The warm helper is documented as ONE capture thread (mutually exclusive with
        // speak/listen) — Caps-Lock dictation already owns the mic. Racing it here would
        // silently steal/drop audio on whichever side loses the child's last-write-wins
        // job slot, so refuse up front instead of contending for the mic.
        return Err(format!(
            "{op_label}: dictation is active; try again after it ends"
        ));
    }
    if capture_busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(format!(
            "{op_label}: another diarize or enroll capture is already active"
        ));
    }
    struct CaptureGuard<'a>(&'a AtomicBool);
    impl Drop for CaptureGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _guard = CaptureGuard(capture_busy);
    let secs = seconds.clamp(1, 60);
    // Bounded wait: `TtsManager::diarize`/`enroll` block on a condvar with no timeout of
    // their own, so a wedged/silent helper would otherwise hang THIS connection forever.
    let timeout = std::time::Duration::from_secs(secs + 30);
    call_with_timeout(timeout, move || f(secs)).map_err(|e| format!("{op_label}: {e}"))
}

/// Parse the helper's diarize JSON (`{segments, speakers}`), match each speaker cluster
/// to an enrolled voiceprint (cosine ≥ `match_threshold`), attach the matched name to
/// that cluster's segments, and return the segments as a JSON array. Unmatched clusters
/// keep their numeric id. No enrolled speakers ⇒ segments pass through unnamed.
fn diarize_named_segments(json: &str, paths: &Paths) -> Result<serde_json::Value, String> {
    let mut out = ds_stt::diarize::parse_output(json)?;
    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
    if !store.is_empty() {
        let threshold = VoiceConfig::load(paths).match_threshold;
        let mut id_to_name: std::collections::HashMap<String, String> = Default::default();
        for (id, emb) in &out.speakers {
            if let Some(name) = ds_stt::diarize::match_speaker(emb, &store, threshold) {
                id_to_name.insert(id.clone(), name);
            }
        }
        for seg in &mut out.segments {
            if let Some(n) = id_to_name.get(&seg.speaker) {
                seg.name = Some(n.clone());
            }
        }
    }
    serde_json::to_value(&out.segments).map_err(|e| format!("serialize segments: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn agent_usage_response_forwards_refresh_and_serializes_the_deck() {
        let response = agent_usage_response_with(true, |refresh| {
            assert!(refresh);
            ds_agent_usage::UsageDeck::empty()
        });

        match response {
            ds_ipc::Response::AgentUsage { deck } => {
                assert_eq!(deck, serde_json::json!({ "cards": [] }));
            }
            other => panic!("expected agent usage response, got {other:?}"),
        }
    }

    #[test]
    fn speak_tts_args_are_validated_before_queue_admission() {
        let parsed = speak_tts_args(Some(serde_json::json!({
            "qwen": { "voice": "ryan", "language": "ja", "repetition_penalty": 1.2 }
        })))
        .unwrap()
        .unwrap();
        let qwen = parsed
            .for_target(ds_config::TtsEngine::BuiltIn, ds_config::TtsModel::Qwen)
            .unwrap();
        assert_eq!(qwen.voice(), Some("ryan"));
        assert_eq!(qwen.language(), Some("ja"));
        assert!(
            speak_tts_args(Some(serde_json::json!({
                "qwen": { "exaggeration": 1.0 }
            })))
            .unwrap_err()
            .contains("invalid tts_args")
        );
        // A voice locked to a language this build cannot route is refused at admit, not
        // clamped: the utterance would otherwise speak through a foreign speaker embedding.
        assert!(
            speak_tts_args(Some(serde_json::json!({
                "kokoro": { "voice": "jf_alpha" }
            })))
            .unwrap_err()
            .contains("cannot route")
        );
        assert!(
            speak_tts_args(Some(serde_json::json!({
                "kokoro": { "voice": "if_sara" }
            })))
            .is_ok()
        );
        assert!(speak_tts_args(None).unwrap().is_none());
    }

    #[test]
    fn should_cancel_on_submit_decision_table() {
        // A voice submit's own echo: never treated as a separate submit, regardless of config.
        assert!(!should_cancel_on_submit(true, true));
        assert!(!should_cancel_on_submit(true, false));
        // A genuine submit: cancels only when the user opted into that scope.
        assert!(should_cancel_on_submit(false, true));
        assert!(!should_cancel_on_submit(false, false));
    }

    #[test]
    fn sessionless_earcon_inherits_active_session() {
        let ttsq = TtsQueue::test_stub();
        ttsq.set_active_session(Some("active".into()));

        assert_eq!(earcon_session(&ttsq, None), Some("active".into()));
        assert_eq!(
            earcon_session(&ttsq, Some("explicit".into())),
            Some("explicit".into()),
            "a session sent by a current hook must remain authoritative"
        );
    }

    #[test]
    fn mark_active_synthetic_does_not_claim_active_or_cancel_speech() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // no config.toml → default clear_on_input=[current]
        let codex_sessions = crate::codex_stream::SessionRegistry::new();
        let grok_sessions = crate::grok_stream::SessionRegistry::new();

        ttsq.set_active_session(Some("other".into()));
        ttsq.enqueue("hi".into(), None, None, Some("a".into()))
            .unwrap();

        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions,
            &paths,
            HookSessions {
                logical: Some("a".into()),
                queue: Some("a".into()),
            },
            true,
            Some(WiredAgent::ClaudeCode),
        );

        assert_eq!(
            ttsq.active_session(),
            Some("other".into()),
            "a synthetic continuation must not steal active-terminal status"
        );
        assert_eq!(
            ttsq.tts_status_sample().queued,
            1,
            "a synthetic continuation must not cancel queued speech"
        );
    }

    #[test]
    fn mark_active_genuine_submit_still_claims_active_and_cancels_current_scope() {
        // Regression guard: proves the fix didn't change real-submit behavior.
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let codex_sessions = crate::codex_stream::SessionRegistry::new();
        let grok_sessions = crate::grok_stream::SessionRegistry::new();

        ttsq.enqueue("hi".into(), None, None, Some("window-a".into()))
            .unwrap();

        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions,
            &paths,
            HookSessions {
                logical: Some("logical-a".into()),
                queue: Some("window-a".into()),
            },
            false,
            Some(WiredAgent::ClaudeCode),
        );

        assert_eq!(ttsq.active_session(), Some("window-a".into()));
        assert_eq!(
            ttsq.tts_status_sample().queued,
            0,
            "default clear_on_input=[current] still prunes a genuine submit's own queued item"
        );
    }

    #[test]
    fn mark_active_grok_nudges_grok_registry_only() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let codex_sessions = crate::codex_stream::SessionRegistry::new();
        let grok_sessions = crate::grok_stream::SessionRegistry::new();

        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions,
            &paths,
            HookSessions {
                logical: Some("g1".into()),
                queue: Some("window-g".into()),
            },
            true,
            Some(WiredAgent::Grok),
        );
        assert!(
            grok_sessions.contains("g1"),
            "Grok MarkActive must register for updates.jsonl tail"
        );

        let grok_sessions2 = crate::grok_stream::SessionRegistry::new();
        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions2,
            &paths,
            HookSessions {
                logical: Some("c1".into()),
                queue: Some("window-c".into()),
            },
            true,
            Some(WiredAgent::ClaudeCode),
        );
        assert!(
            !grok_sessions2.contains("c1"),
            "non-Grok clients must not enter the Grok registry"
        );
    }

    #[test]
    fn run_bounded_capture_refuses_while_dictation_is_active() {
        let busy = AtomicBool::new(true);
        let capture_busy = AtomicBool::new(false);
        let result: Result<u32, String> =
            run_bounded_capture(&busy, &capture_busy, "diarize", 5, |_secs| Ok(42));
        assert_eq!(
            result,
            Err("diarize: dictation is active; try again after it ends".to_string())
        );
    }

    #[test]
    fn run_bounded_capture_clamps_seconds_to_1_through_60() {
        let idle = AtomicBool::new(false);
        let capture_busy = AtomicBool::new(false);

        let seen = Arc::new(Mutex::new(0u64));
        let seen2 = seen.clone();
        assert_eq!(
            run_bounded_capture(&idle, &capture_busy, "enroll", 999, move |secs| {
                *seen2.lock().unwrap() = secs;
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(
            *seen.lock().unwrap(),
            60,
            "seconds must clamp to the 60s ceiling"
        );

        let seen = Arc::new(Mutex::new(0u64));
        let seen2 = seen.clone();
        assert_eq!(
            run_bounded_capture(&idle, &capture_busy, "enroll", 0, move |secs| {
                *seen2.lock().unwrap() = secs;
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(
            *seen.lock().unwrap(),
            1,
            "seconds must clamp to the 1s floor"
        );
    }

    #[test]
    fn run_bounded_capture_propagates_the_inner_error_with_the_op_label() {
        let idle = AtomicBool::new(false);
        let capture_busy = AtomicBool::new(false);
        let result: Result<(), String> =
            run_bounded_capture(&idle, &capture_busy, "diarize", 5, |_secs| {
                Err(std::io::Error::other("boom"))
            });
        assert_eq!(result, Err("diarize: boom".to_string()));
    }

    /// Seed the store with one enrolled speaker ("Alex") and return `Paths` rooted at a
    /// fresh tempdir whose `speakers_json` holds it. The tempdir is returned too so it
    /// isn't dropped (and cleaned up) before the test uses `paths`.
    fn paths_with_enrolled_alex() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let mut store = ds_config::SpeakerStore::load(&paths.speakers_json);
        store.upsert("Alex", vec![1.0, 0.0, 0.0]);
        store.save(&paths.speakers_json).unwrap();
        (dir, paths)
    }

    #[test]
    fn diarize_named_segments_relabels_matches_and_leaves_the_rest_unlabeled() {
        let (_dir, paths) = paths_with_enrolled_alex();
        // Cluster "1"'s embedding is almost exactly Alex's direction (matches, above the
        // default 0.65 threshold); cluster "2" is orthogonal to everyone (no match).
        let json = r#"{
            "segments": [
                {"speaker": "1", "start": 0.0, "end": 1.0},
                {"speaker": "2", "start": 1.0, "end": 2.0}
            ],
            "speakers": {
                "1": [0.99, 0.05, 0.0],
                "2": [0.0, 0.0, 1.0]
            }
        }"#;
        let value = diarize_named_segments(json, &paths).expect("valid input");
        let segments: Vec<ds_stt::diarize::SpeakerSegment> =
            serde_json::from_value(value).expect("segments array");
        assert_eq!(segments[0].speaker, "1");
        assert_eq!(segments[0].name.as_deref(), Some("Alex"));
        assert_eq!(segments[1].speaker, "2");
        assert_eq!(segments[1].name, None);
    }

    #[test]
    fn diarize_named_segments_passes_through_unnamed_with_no_enrolled_speakers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // speakers.json never written ⇒ empty store.
        let json = r#"{
            "segments": [{"speaker": "1", "start": 0.0, "end": 1.0}],
            "speakers": {"1": [0.99, 0.05, 0.0]}
        }"#;
        let value = diarize_named_segments(json, &paths).expect("valid input");
        let segments: Vec<ds_stt::diarize::SpeakerSegment> =
            serde_json::from_value(value).expect("segments array");
        assert_eq!(segments[0].speaker, "1");
        assert_eq!(
            segments[0].name, None,
            "no enrolled speakers ⇒ segments pass through unnamed"
        );
    }

    #[test]
    fn diarize_named_segments_leaves_below_threshold_clusters_unlabeled() {
        let (_dir, paths) = paths_with_enrolled_alex();
        // Similar-ish to Alex's direction but not close enough to clear the 0.65 default
        // threshold (cosine ≈ 0.45), so it must stay unmatched even though a speaker IS
        // enrolled.
        let json = r#"{
            "segments": [{"speaker": "1", "start": 0.0, "end": 1.0}],
            "speakers": {"1": [0.45, 0.89, 0.0]}
        }"#;
        let value = diarize_named_segments(json, &paths).expect("valid input");
        let segments: Vec<ds_stt::diarize::SpeakerSegment> =
            serde_json::from_value(value).expect("segments array");
        assert_eq!(segments[0].name, None);
    }

    #[test]
    fn diarize_named_segments_rejects_malformed_json_without_panicking() {
        let (_dir, paths) = paths_with_enrolled_alex();
        assert!(diarize_named_segments("not json", &paths).is_err());
        assert!(diarize_named_segments("", &paths).is_err());
    }

    #[test]
    fn diarize_named_segments_tolerates_empty_segments_and_no_speakers_map() {
        let (_dir, paths) = paths_with_enrolled_alex();
        // Older-shim shape: no `speakers` map, no segments — must still degrade sensibly
        // (empty output) rather than error, even with an enrolled speaker in the store.
        let value = diarize_named_segments(r#"{"segments":[]}"#, &paths).expect("valid input");
        let segments: Vec<ds_stt::diarize::SpeakerSegment> =
            serde_json::from_value(value).expect("segments array");
        assert!(segments.is_empty());
    }
}
