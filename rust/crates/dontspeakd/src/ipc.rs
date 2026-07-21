//! RPC server thread + dispatch arms, plus [`FrontendRegistry`] (Zed subscriptions
//! with acked transcript delivery).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{CancelSpeechScope, ClientSource, NarrateKind, Paths, VoiceConfig};
use ds_ipc::{Conn, DictationEvent, HandleOutcome};
use ds_narrate::{BatchPayload, StreamBatch};

use crate::downloads::{DownloadProg, start_download};
use crate::status::{EngineShared, model_status_json};
use crate::stt_test::TestSession;
use crate::ttsq::TtsQueue;

/// INFO log for a client-originated request (` client=<token>`). Always `log_from`
/// against `paths.log_file` — never `log_cached*` (that resolves real `$HOME`; issue #26).
fn log_client(paths: &Paths, client: ClientSource, msg: &str) {
    ds_log::log_from(
        &paths.log_file,
        ds_log::LogLevel::Info,
        "engine",
        client,
        msg,
    );
}

/// Cancel on MarkActive? Skip voice-submit echoes (engine already applied `clear_on_input`).
pub(crate) fn should_cancel_on_submit(was_voice: bool, scope_configured: bool) -> bool {
    !was_voice && scope_configured
}

/// Legacy hooks omit earcon session — keep cues on the active stream (not global).
fn earcon_session(ttsq: &TtsQueue, requested: Option<String>) -> Option<String> {
    requested.or_else(|| ttsq.active_session())
}

/// UserPromptSubmit MarkActive (typed/dictated, or `synthetic` harness continuation — #11).
///
/// `codex_sessions` nudge always runs (liveness + codex_stream re-discovery after restart).
/// Grok-only: also nudge `grok_sessions` so the updates.jsonl tail attaches.
/// Active-terminal claim + `clear_on_input` skipped when `synthetic` (no human "I've moved on").
/// Classifier: `dontspeak::hook_speak::is_synthetic_continuation`.
fn handle_mark_active(
    ttsq: &TtsQueue,
    codex_sessions: &crate::codex_stream::SessionRegistry,
    grok_sessions: &crate::grok_stream::SessionRegistry,
    paths: &Paths,
    session: Option<String>,
    synthetic: bool,
    source: ClientSource,
) {
    // Unconditional liveness nudge (+ re-discovery / negative-cache re-arm).
    if let Some(s) = &session {
        codex_sessions.nudge(s);
        if source == ClientSource::Grok {
            grok_sessions.nudge(s);
        }
    }
    if synthetic {
        return; // no active-terminal steal, no TTS queue touch
    }
    ttsq.set_active_session(session.clone());
    // Voice-submit echo: engine already applied clear_on_input; skip re-cancel.
    let was_voice = ttsq.take_recent_voice_submit();
    // Skip config read when was_voice already forces no cancel.
    if !was_voice {
        let scopes = VoiceConfig::load(paths).clear_on_input;
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Current)) {
            ttsq.clear_session(session.clone());
        }
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Other)) {
            // Use this request's session, not re-read active (concurrent MarkActive race).
            ttsq.cancel_for_submit(session.clone(), false, true);
        }
    }
}

/// NarrateBatch arm (testable with `TtsQueue::test_stub`). Cumulative panel-agent
/// text → [`ds_narrate::deliver_batch`] → narration enqueue. Live config; no-op when
/// narration off. `mic_active` = system-wide mic probe (not PTT `stt_active`).
///
/// Enqueue failures are logged and swallowed (caller still gets `Response::Done`) —
/// best-effort, same as hooks / `codex_stream` (panel must not hard-fail on TTS).
#[allow(clippy::too_many_arguments)]
fn handle_narrate_batch(
    paths: &Paths,
    ttsq: &TtsQueue,
    log: &dyn Fn(&str),
    session: &str,
    key: String,
    text: String,
    is_final: bool,
    mic_active: bool,
) {
    let cfg = VoiceConfig::load(paths);
    let digests_on = cfg.narrates(NarrateKind::Digests);
    let shorts_on = cfg.narrates(NarrateKind::Shorts);
    if !digests_on && !shorts_on {
        return;
    }
    if session.chars().any(char::is_control) {
        log("frontend: refused narrate-batch (session contains control characters)");
        return;
    }
    // Activity-log the session's FIRST batch. The per-session state file (the
    // streaming witness) doubles as the "already started" marker: it is created
    // by the first `deliver_batch` call below and reclaimed on SessionEnd, so
    // this logs once per narrate-batch session, not once per batch.
    // Wire has no frontend tag (`narrate_batch` is one-shot); session is the
    // attribution stand-in until a dedicated `ClientSource` (docs/ZED-FRONTEND.md).
    if !ds_narrate::witness_exists(paths, session) {
        log(&format!(
            "frontend: narrate-batch session='{session}' started"
        ));
    }
    let batch = StreamBatch {
        key,
        payload: BatchPayload::Cumulative { text },
        is_final,
    };
    if let Err(e) = ds_narrate::deliver_batch(
        paths,
        session,
        &batch,
        mic_active,
        digests_on,
        shorts_on,
        |utt| {
            ttsq.enqueue_narration(
                utt.text.clone(),
                ClientSource::Unknown,
                Some(session.to_string()),
                Some(utt.id.clone()),
                None,
            )
        },
    ) {
        log(&format!(
            "frontend: narrate-batch session='{session}' enqueue failed: {e}"
        ));
    }
}

/// SessionEnd session-scoped half (frontends call IPC after NarrateBatch).
/// Grok: drop updates.jsonl tail registration.
fn handle_session_end(
    paths: &Paths,
    ttsq: &TtsQueue,
    grok_sessions: &crate::grok_stream::SessionRegistry,
    session: Option<String>,
    source: ClientSource,
) {
    match session {
        None => ttsq.clear(),
        Some(session) => {
            ttsq.forget_narration_session(&session);
            if source == ClientSource::Grok {
                grok_sessions.forget(&session);
            }
            ttsq.end_session(Some(session.clone()));
            ds_narrate::clear_session_state(paths, &session);
        }
    }
}

/// RPC accept loop on a dedicated thread. `Reload` flips `reload_requested`; other
/// arms drive TTS/status/STT-test/provider. Returns [`FrontendRegistry`] for engine
/// emission/confirm wiring.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_ipc_server(
    shared: EngineShared,
    paths: Paths,
    running: Arc<AtomicBool>,
    stt_test: Arc<TestSession>,
    ttsq: Arc<TtsQueue>,
    reload_requested: Arc<AtomicBool>,
    downloads: DownloadProg,
    codex_sessions: Arc<crate::codex_stream::SessionRegistry>,
    grok_sessions: Arc<crate::grok_stream::SessionRegistry>,
    mic: ds_platform::MicState,
) -> Arc<FrontendRegistry> {
    let frontends = FrontendRegistry::new(&paths);
    let frontends_for_handler = Arc::clone(&frontends);
    let sock = paths.engine_sock.clone();
    std::thread::spawn(move || {
        // Bad-request sink: hooks discard the reply, so without this WARN deploy skew
        // (stale CLI missing `--client`) silently kills the voice loop. See log_client.
        let log_paths = paths.clone();
        let on_bad_request = move |detail: &str| {
            ds_log::log_from(
                &log_paths.log_file,
                ds_log::LogLevel::Warn,
                "engine",
                // Not attributable: the line that would have named the client is the very
                // line that failed to decode.
                ClientSource::Unknown,
                &format!(
                    "{detail} — caller and engine are out of sync; \
                     reinstall the CLI and restart the app"
                ),
            );
        };
        let capture_busy = Arc::new(AtomicBool::new(false));
        let frontends = frontends_for_handler;
        let handler = move |req: ds_ipc::Request,
                            mut conn: ds_ipc::Conn|
              -> std::io::Result<ds_ipc::HandleOutcome> {
            match req {
                ds_ipc::Request::Ping => conn.send(&ds_ipc::Response::Pong)?,

                ds_ipc::Request::EnsureKokoroFrontend => {
                    // Non-blocking: kick the shared-frontend download if any asset is absent.
                    // `start_download` is single-flight PER TARGET — if this target is
                    // already fetching, the request ATTACHES to it (and it runs in
                    // parallel with any other target's download, never queued behind one).
                    if !ds_model::is_kokoro_frontend_present() {
                        start_download(&downloads, ds_model::DownloadTarget::KokoroFrontend);
                    }
                    conn.send(&ds_ipc::Response::Done)?;
                }
                ds_ipc::Request::EnsureCodexStream => {
                    match codex_sessions.ensure_remote(std::time::Duration::from_secs(20)) {
                        Ok(endpoint) => {
                            conn.send(&ds_ipc::Response::CodexStreamReady { endpoint })?
                        }
                        Err(message) => conn.send(&ds_ipc::Response::error(message))?,
                    }
                }
                ds_ipc::Request::GreetSession { session, source } => {
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
                            "greet_session session={}",
                            session.as_deref().unwrap_or("-")
                        ),
                    );
                    if let Some(s) = &session {
                        codex_sessions.nudge(s);
                        if source == ClientSource::Grok {
                            grok_sessions.nudge(s);
                        }
                    }
                    ttsq.greet_session(source, session);
                    let _ = conn.send(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::MarkActive {
                    session,
                    synthetic,
                    source,
                } => {
                    log_client(
                        &paths,
                        source,
                        &format!(
                            "mark_active session={} synthetic={synthetic}",
                            session.as_deref().unwrap_or("-")
                        ),
                    );
                    handle_mark_active(
                        &ttsq,
                        &codex_sessions,
                        &grok_sessions,
                        &paths,
                        session,
                        synthetic,
                        source,
                    );
                    let _ = conn.send(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Speak {
                    text,
                    voice,
                    rate,
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
                        &format!(
                            "speak session={} chars={}",
                            session.as_deref().unwrap_or("-"),
                            text.chars().count()
                        ),
                    );
                    match ttsq.enqueue(text, voice, rate, source, session) {
                        Ok(()) => {
                            let _ = conn.send(&ds_ipc::Response::Done);
                        }
                        Err(e) => {
                            let _ = conn.send(&ds_ipc::Response::error(format!("speak: {e}")));
                        }
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
                    // Success is deliberately NOT logged: it fires once per blockquote and
                    // would spam the activity log. Identified retries return the same success
                    // without adding a second queue item.
                    match ttsq.enqueue_narration(
                        text,
                        source,
                        session,
                        narration_id,
                        detection_text,
                    ) {
                        Ok(()) => {
                            let _ = conn.send(&ds_ipc::Response::Done);
                        }
                        Err(e) => {
                            log_client(&paths, source, &format!("narration rejected: {e}"));
                            let _ = conn.send(&ds_ipc::Response::error(format!(
                                "speak narration: {e}"
                            )));
                        }
                    }
                }
                ds_ipc::Request::NarrateBatch {
                    session,
                    key,
                    text,
                    is_final,
                } => {
                    // Panel-agent narration (Zed's ACP-thread bridge) → the same
                    // ds_narrate pipeline + DisplayState dedup the hooks and the
                    // codex_stream subscriber use. Mic gating uses the SYSTEM-WIDE
                    // mic probe (`MicState`), not the engine's PTT-only stt_active —
                    // parity with hook_narrate and codex_stream.
                    // `ClientSource::Unknown`: wire has no source (docs/ZED-FRONTEND.md
                    // open follow-up for a Zed variant). Session is in every log line.
                    // Errors inside are best-effort (Done always — see handle_narrate_batch).
                    handle_narrate_batch(
                        &paths,
                        &ttsq,
                        // `log_from` against `paths.log_file`, never `log_cached*` — see
                        // `log_client` above.
                        &|msg: &str| {
                            ds_log::log_from(
                                &paths.log_file,
                                ds_log::LogLevel::Info,
                                "engine",
                                ClientSource::Unknown,
                                msg,
                            );
                        },
                        &session,
                        key,
                        text,
                        is_final,
                        mic.is_active(),
                    );
                    conn.send(&ds_ipc::Response::Done)?;
                }
                ds_ipc::Request::SetMuted { on } => {
                    // Global mute toggle (tray checkbox). Silences playback without stopping it.
                    ttsq.set_muted(on);
                    conn.send(&ds_ipc::Response::Done)?;
                }
                ds_ipc::Request::Stop { session, source } => {
                    // None = global hard barge (drop the whole queue + cancel the
                    // current item). Some(s) = per-window: prune only that session's
                    // items and cancel playback only if it's that session's, so one
                    // terminal's preempt/close never silences another's.
                    // Also clear the Grok sticky sibling (`grok-stop:<id>`): MarkActive
                    // current-clear leaves sticky intact by design, but an explicit stop
                    // must silence digests + ding co-queued under that tag.
                    log_client(
                        &paths,
                        source,
                        &format!("stop session={}", session.as_deref().unwrap_or("-")),
                    );
                    match session {
                        None => ttsq.clear(),
                        Some(s) => {
                            let sticky = format!("grok-stop:{s}");
                            ttsq.clear_session(Some(s));
                            ttsq.clear_session(Some(sticky));
                        }
                    }
                    let _ = conn.send(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SessionEnd { session, source } => {
                    // Window closed for good: per-window barge. The agent's voice assignment
                    // is keyed by client, not session, and deliberately survives.
                    // None (no session id) → global hard barge.
                    // Grok: also drop the updates.jsonl tail registration.
                    // Frontend NarrateBatch: also clear ds_narrate session state.
                    log_client(
                        &paths,
                        source,
                        &format!("session_end session={}", session.as_deref().unwrap_or("-")),
                    );
                    handle_session_end(&paths, &ttsq, &grok_sessions, session, source);
                    let _ = conn.send(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::TestRecognitionStart => {
                    // Streams Listening/Partial then a terminal Transcript. Handles
                    // its own write failures (aborts the listen when the streaming
                    // client hangs up), so nothing to propagate here.
                    stt_test.run(&mut conn);
                }
                ds_ipc::Request::TestRecognitionStop => {
                    stt_test.stop();
                    conn.send(&ds_ipc::Response::Done)?;
                }
                ds_ipc::Request::ModelStatus => {
                    let _ = conn.send(&ds_ipc::Response::ModelStatus {
                        status: model_status_json(&shared, &paths, || ttsq.tts_status_sample()),
                    });
                }
                ds_ipc::Request::WaitModelStatus { since, timeout_ms } => {
                    // PUSH transport: block this (dedicated) connection until the
                    // dictation status changes or the cap elapses, then reply with the
                    // fresh snapshot. One-thread-per-connection (see ipc server), so this
                    // never stalls the timer's ModelStatus / SetMuted on other connections.
                    let timeout = std::time::Duration::from_millis(timeout_ms.clamp(1, 60_000));
                    shared.gate.wait_changed(since, timeout);
                    let _ = conn.send(&ds_ipc::Response::ModelStatus {
                        status: model_status_json(&shared, &paths, || ttsq.tts_status_sample()),
                    });
                }
                ds_ipc::Request::SetProvider { provider } => {
                    // set_provider restarts the warm child (which hosts BOTH Kokoro and
                    // Parakeet) and resets both engines' stats when the active provider
                    // actually changes — centralized in restart_child, so this path AND
                    // the set_config/config-reload path (apply_tts_provider) both get it.
                    shared.tts.set_provider(&provider);
                    conn.send(&ds_ipc::Response::Done)?;
                }
                ds_ipc::Request::Reload => {
                    // The MCP/GUI wrote config.toml and asks us to apply it now.
                    // Flip the same flag SIGHUP uses; the poll loop reloads next tick
                    // (debounced, re-reading config.toml). No mtime wait.
                    reload_requested.store(true, Ordering::Relaxed);
                    conn.send(&ds_ipc::Response::Done)?;
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
                        Ok(()) => {
                            let _ = conn.send(&ds_ipc::Response::Done);
                        }
                        Err(e) => {
                            let _ = conn.send(&ds_ipc::Response::error(format!("earcon: {e}")));
                        }
                    }
                }
                ds_ipc::Request::AuthorizeSystemStt => {
                    // Opt-in gate for `stt_engine=system`: prompt for Speech Recognition
                    // authorization (attributed to this app process) + verify on-device
                    // capability. Done ⇒ usable; Error ⇒ the reason set_config relays so it
                    // refuses to enable rather than silently falling back.
                    match ds_stt::system_authorize() {
                        Ok(()) => conn.send(&ds_ipc::Response::Done)?,
                        Err(reason) => conn.send(&ds_ipc::Response::error(format!(
                            "system STT unavailable: {reason}"
                        )))?,
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
                            Ok(segments) => {
                                conn.send(&ds_ipc::Response::Diarization { segments })?
                            }
                            Err(e) => {
                                conn.send(&ds_ipc::Response::error(format!("diarize: {e}")))?
                            }
                        },
                        Err(e) => conn.send(&ds_ipc::Response::error(e))?,
                    }
                }
                ds_ipc::Request::Enroll { name, seconds } => {
                    // Record a sample, extract a voiceprint, persist it under `name`.
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        conn.send(&ds_ipc::Response::error("enroll: name must not be empty"))?;
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
                                    Ok(()) => conn.send(&ds_ipc::Response::Enrolled { name })?,
                                    Err(e) => conn.send(&ds_ipc::Response::error(format!(
                                        "enroll: save failed: {e}"
                                    )))?,
                                }
                            }
                            Err(e) => conn.send(&ds_ipc::Response::error(e))?,
                        }
                    }
                }
                ds_ipc::Request::ForgetSpeaker { name } => {
                    let mut store = ds_config::SpeakerStore::load(&paths.speakers_json);
                    store.remove(&name);
                    match store.save(&paths.speakers_json) {
                        Ok(()) => conn.send(&ds_ipc::Response::Done)?,
                        Err(e) => {
                            conn.send(&ds_ipc::Response::error(format!("forget_speaker: {e}")))?
                        }
                    }
                }
                ds_ipc::Request::ListSpeakers => {
                    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
                    conn.send(&ds_ipc::Response::Speakers {
                        names: store.names(),
                    })?;
                }
                ds_ipc::Request::SubscribeFrontend { app } => {
                    // Native frontend (e.g. Zed) registering for dictation events +
                    // acked transcript delivery. Config read LIVE (like Earcon /
                    // MarkActive) so flipping the `frontend_enabled` kill-switch
                    // takes effect on the next subscribe without an engine restart.
                    let enabled = VoiceConfig::load(&paths).frontend_enabled;
                    return handle_subscribe_frontend(&frontends, enabled, app, conn);
                }
                ds_ipc::Request::AckDeliver { .. } => {
                    // Acks belong INSIDE a subscription connection (the registry
                    // reads them during a deliver round-trip, after takeover) — one
                    // arriving through the normal request loop is a client-side
                    // protocol error. Answered (not connection-fatal) so a confused
                    // client's read doesn't hang.
                    conn.send(&ds_ipc::Response::error(
                        "ack_deliver outside a frontend subscription",
                    ))?;
                }
                ds_ipc::Request::Shutdown => {
                    // Ack first (best-effort — the stop must proceed even if the
                    // client already hung up), then ask the main loop to exit (it
                    // tears down the warm child, removes the pidfile + socket, and
                    // process::exits).
                    let _ = conn.send(&ds_ipc::Response::Done);
                    running.store(false, Ordering::Relaxed);
                }
            }
            Ok(ds_ipc::HandleOutcome::Done(conn))
        };
        if let Err(e) = ds_ipc::serve(&sock, handler, on_bad_request) {
            log::warn!(target: "engine", "IPC server exited: {e}");
        }
    });
    frontends
}

/// SubscribeFrontend arm (testable without full engine). Kill-switch off / unknown
/// tag → terminal error; else registry takeover. Tag must be in `FRONTEND_APPS` so
/// per-tag eviction bounds taken-over slots (unchecked tags would grow unboundedly).
fn handle_subscribe_frontend(
    frontends: &FrontendRegistry,
    enabled: bool,
    app: String,
    mut conn: Conn,
) -> std::io::Result<HandleOutcome> {
    // Reject controls before log interpolation (newline could forge a second entry).
    if app.chars().any(char::is_control) {
        (frontends.log)("frontend: refused subscribe (app tag contains control characters)");
        conn.send(&ds_ipc::Response::error(
            "frontend app tag must not contain control characters",
        ))?;
        return Ok(HandleOutcome::Done(conn));
    }
    if !enabled {
        (frontends.log)(&format!(
            "frontend: refused subscribe app='{app}' (frontend_enabled=false)"
        ));
        conn.send(&ds_ipc::Response::error(
            "frontend subscriptions are disabled (frontend_enabled = false in config.toml)",
        ))?;
        return Ok(HandleOutcome::Done(conn));
    }
    if !ds_platform::frontend_tag_known(&app) {
        (frontends.log)(&format!(
            "frontend: refused subscribe app='{app}' (unknown tag)"
        ));
        conn.send(&ds_ipc::Response::error(format!(
            "unknown frontend app tag '{app}'"
        )))?;
        return Ok(HandleOutcome::Done(conn));
    }
    // Sync broadcasts/delivers run on the engine tick thread — shrink from 5s RPC
    // default so a stopped-reading frontend can't stall for seconds on a single write.
    // Matches the deliver end-to-end budget (write + ack share one deadline below).
    conn.set_write_timeout(ACK_DELIVER_TIMEOUT);
    frontends.subscribe(app, conn)?;
    Ok(HandleOutcome::TookOver)
}

// ── Frontend subscriptions: registry + acknowledged delivery ────────────────

/// End-to-end budget for one frontend deliver (write **plus** ack wait) before paste
/// fallback. ~1 UI frame + local round-trip. Shared deadline — not two independent
/// 300 ms phases — because `deliver_to_frontmost` runs on the engine's single tick
/// thread (Caps poll, DOUBLE_TAP_MS, LED sync, other sessions stall for this window).
const ACK_DELIVER_TIMEOUT: Duration = Duration::from_millis(300);

/// One live subscription: app tag + taken-over conn. Conn is locked separately so
/// one ack-wait never blocks registry bookkeeping or another subscriber.
///
/// `shutdown` is lock-free ([`ds_ipc::Conn::shutdown_handle`]) so eviction can cut
/// short an in-flight write that still holds an `Arc` clone after registry remove.
/// `cancelled` is the portable wake for a blocked ack-read: on Windows AF_UNIX
/// (`uds_windows`), `shutdown(Both)` on a writer clone does **not** cancel an
/// in-progress `recv` on the reader handle (issue #111 M2) — `wait_for_ack` polls
/// short slices and checks this flag instead of relying on the OS wake.
struct Subscriber {
    app: String,
    conn: Mutex<Conn>,
    shutdown: ds_ipc::ShutdownHandle,
    cancelled: AtomicBool,
}

/// Live native-frontend subscriptions (≤1 per app tag). Sync writes; drop on first
/// failed write. Deliver succeeds only after matching ack — else paste fallback.
pub(crate) struct FrontendRegistry {
    subscribers: Mutex<Vec<Arc<Subscriber>>>,
    /// Monotonic seq on every `FrontendEvent` (deliver→ack correlation + order).
    next_seq: AtomicU64,
    /// Activity log: `log_from` vs `paths.log_file` (never `log_cached*`); injectable for tests.
    log: Box<dyn Fn(&str) + Send + Sync>,
}

/// [`FrontendRegistry::deliver_to_frontmost`]: acked → skip paste; else paste fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliverOutcome {
    Delivered,
    Failed,
}

impl FrontendRegistry {
    pub(crate) fn new(paths: &Paths) -> Arc<Self> {
        let log_file = paths.log_file.clone();
        // Mirrors `log_client`'s WARN/INFO split: call sites prefix a line with
        // `"WARN:"` for the eviction/drop cases, everything else is INFO.
        Self::with_logger(Box::new(move |s: &str| {
            let (level, msg) = match s.strip_prefix("WARN:") {
                Some(rest) => (ds_log::LogLevel::Warn, rest.trim_start()),
                None => (ds_log::LogLevel::Info, s),
            };
            ds_log::log_from(&log_file, level, "engine", ClientSource::Unknown, msg);
        }))
    }

    /// Injectable logger (tests use a no-op — never the real unified log).
    pub(crate) fn with_logger(log: Box<dyn Fn(&str) + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            subscribers: Mutex::new(Vec::new()),
            next_seq: AtomicU64::new(0),
            log,
        })
    }

    /// Register `conn` as `app`'s live subscription. ≤1 per tag: resubscribe evicts
    /// the previous conn (bounds taken-over slots). `Err` only if shutdown-handle clone fails.
    pub(crate) fn subscribe(&self, app: String, conn: Conn) -> std::io::Result<()> {
        let shutdown = conn.shutdown_handle()?;
        let evicted: Vec<Arc<Subscriber>> = {
            let mut subs = self.subscribers.lock().unwrap();
            let evicted: Vec<Arc<Subscriber>> =
                subs.iter().filter(|s| s.app == app).cloned().collect();
            subs.retain(|s| s.app != app);
            if !evicted.is_empty() {
                (self.log)(&format!(
                    "frontend: evicted previous '{app}' subscriber (resubscribe)"
                ));
            }
            (self.log)(&format!("frontend: subscribed app='{app}'"));
            subs.push(Arc::new(Subscriber {
                app,
                conn: Mutex::new(conn),
                shutdown,
                cancelled: AtomicBool::new(false),
            }));
            evicted
        };
        // Outside the registry lock: mark cancelled first so a short-poll
        // `wait_for_ack` notices even when OS shutdown does not wake recv
        // (Windows), then shut the socket for write-side fail + Unix EOF wake.
        for old in evicted {
            old.cancelled.store(true, Ordering::Release);
            old.shutdown.shutdown();
        }
        Ok(())
    }

    /// Any live subscriber's app frontmost? Injected probe keeps registry platform-free.
    /// Engine uses this at `start_recording` for frontend-owned tagging.
    pub(crate) fn any_subscriber_frontmost(&self, is_app_frontmost: &dyn Fn(&str) -> bool) -> bool {
        self.subscribers
            .lock()
            .unwrap()
            .iter()
            .any(|s| is_app_frontmost(&s.app))
    }

    /// Stream one event to every subscriber (next seq). Drop on write failure.
    /// Returns whether anyone remains (engine restores overlay if empty).
    pub(crate) fn broadcast(&self, event: DictationEvent) -> bool {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let line = ds_ipc::Response::FrontendEvent { event, seq };
        let subs: Vec<Arc<Subscriber>> = self.subscribers.lock().unwrap().clone();
        for sub in subs {
            let failed = sub.conn.lock().unwrap().send(&line).is_err();
            if failed {
                self.drop_subscriber(&sub, "event write failed");
            }
        }
        !self.subscribers.lock().unwrap().is_empty()
    }

    /// Deliver to the frontmost subscriber; write + matching ack share one
    /// [`ACK_DELIVER_TIMEOUT`] deadline (engine tick thread). Write error / nack /
    /// timeout / EOF → `Failed` + drop (paste fallback must not race a late insert).
    /// No frontmost subscriber → `Failed`, keep others.
    pub(crate) fn deliver_to_frontmost(
        &self,
        text: &str,
        submit: bool,
        is_app_frontmost: &dyn Fn(&str) -> bool,
    ) -> DeliverOutcome {
        let target = {
            let subs = self.subscribers.lock().unwrap();
            subs.iter().find(|s| is_app_frontmost(&s.app)).cloned()
        };
        let Some(sub) = target else {
            return DeliverOutcome::Failed; // not frontmost — classic path, keep subs
        };
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let line = ds_ipc::Response::FrontendEvent {
            event: DictationEvent::Deliver {
                text: text.to_string(),
                submit,
            },
            seq,
        };
        let result: Result<bool, String> = {
            let mut conn = sub.conn.lock().unwrap();
            // Shared end-to-end budget: time spent on send shrinks the ack wait.
            let deadline = Instant::now() + ACK_DELIVER_TIMEOUT;
            match conn.send(&line) {
                Err(e) => Err(format!("deliver write failed: {e}")),
                // Shared end-to-end deadline (M1) + cancel flag for Windows read wake (M2).
                Ok(()) => wait_for_ack(&mut conn, seq, deadline, &sub.cancelled),
            }
        };
        match result {
            Ok(true) => DeliverOutcome::Delivered,
            Ok(false) => {
                self.drop_subscriber(&sub, "frontend nacked the deliver");
                DeliverOutcome::Failed
            }
            Err(why) => {
                self.drop_subscriber(&sub, &why);
                DeliverOutcome::Failed
            }
        }
    }

    /// Drop by Arc identity (not tag — concurrent resubscribe may have a new peer).
    /// Always cancel + shutdown socket (idempotent if already evicted).
    fn drop_subscriber(&self, target: &Arc<Subscriber>, why: &str) {
        let mut subs = self.subscribers.lock().unwrap();
        let had = subs.len();
        subs.retain(|s| !Arc::ptr_eq(s, target));
        if subs.len() != had {
            (self.log)(&format!(
                "WARN: frontend: dropped '{}' subscriber ({why}) — delivery falls back to paste",
                target.app
            ));
        }
        drop(subs);
        target.cancelled.store(true, Ordering::Release);
        target.shutdown.shutdown();
    }

    /// Live subscriber count, for tests.
    #[cfg(test)]
    fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }
}

/// Max stall after cancel when OS `shutdown` does not wake a blocked recv
/// (Windows AF_UNIX). Small vs `ACK_DELIVER_TIMEOUT`; acks still return as soon
/// as the line arrives (timeout only bounds empty waits).
const ACK_POLL: Duration = Duration::from_millis(25);

/// Wait for `ack_deliver` matching `want_seq` until `deadline` (shared with the
/// preceding deliver write — remaining budget only). Stale/stray lines skipped.
/// `Ok(ok)` on match; `Err` on timeout/EOF/transport/cancel.
///
/// Polls in [`ACK_POLL`] slices so a concurrent resubscribe/`drop_subscriber`
/// that sets `cancelled` is noticed promptly even when `ShutdownHandle::shutdown`
/// fails to unblock the OS read (observed on Windows — issue #111 M2).
fn wait_for_ack(
    conn: &mut Conn,
    want_seq: u64,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<bool, String> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("subscriber shut down during ack wait".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("ack timed out".into());
        }
        match conn.recv_deadline(remaining.min(ACK_POLL)) {
            Ok(Some(ds_ipc::Request::AckDeliver { seq, ok })) if seq == want_seq => return Ok(ok),
            Ok(Some(_)) => continue, // stale ack / stray line — keep waiting for ours
            Ok(None) => return Err("frontend disconnected before acking".into()),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Slice elapsed — loop rechecks `cancelled` and the overall deadline.
                continue;
            }
            Err(e) => return Err(format!("ack read failed: {e}")),
        }
    }
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
    fn should_cancel_on_submit_decision_table() {
        // A voice submit's own echo: never treated as a separate submit, regardless of config.
        assert!(!should_cancel_on_submit(true, true));
        assert!(!should_cancel_on_submit(true, false));
        // A genuine submit: cancels only when the user opted into that scope.
        assert!(should_cancel_on_submit(false, true));
        assert!(!should_cancel_on_submit(false, false));
    }

    #[test]
    fn sessionless_legacy_earcon_inherits_active_session() {
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
        ttsq.enqueue("hi".into(), None, None, ClientSource::Unknown, Some("a".into()))
            .unwrap();

        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions,
            &paths,
            Some("a".into()),
            true,
            ClientSource::ClaudeCode,
        );

        assert_eq!(
            ttsq.active_session(),
            Some("other".into()),
            "a synthetic continuation must not steal active-terminal status"
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
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

        ttsq.enqueue("hi".into(), None, None, ClientSource::Unknown, Some("a".into()))
            .unwrap();

        handle_mark_active(
            &ttsq,
            &codex_sessions,
            &grok_sessions,
            &paths,
            Some("a".into()),
            false,
            ClientSource::ClaudeCode,
        );

        assert_eq!(ttsq.active_session(), Some("a".into()));
        assert_eq!(
            ttsq.tts_status_sample().2,
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
            Some("g1".into()),
            true,
            ClientSource::Grok,
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
            Some("c1".into()),
            true,
            ClientSource::ClaudeCode,
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

    // ── NarrateBatch: panel-agent narration through the shared pipeline ─────

    /// Drive `handle_narrate_batch` the way the IPC arm does, with a silent log
    /// sink (tests must not append to the REAL unified log — see `test_registry`).
    fn narrate(
        paths: &Paths,
        ttsq: &TtsQueue,
        session: &str,
        key: &str,
        text: &str,
        is_final: bool,
        mic_active: bool,
    ) {
        handle_narrate_batch(
            paths,
            ttsq,
            &|_| {},
            session,
            key.to_string(),
            text.to_string(),
            is_final,
            mic_active,
        );
    }

    #[test]
    fn narrate_batch_speaks_a_completed_blockquote_exactly_once_across_growing_batches() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // no config.toml → default narrate=[shorts,digests]

        // First cumulative cut: the blockquote run is still OPEN (no terminating
        // blank line, not final) — nothing speaks yet.
        narrate(
            &paths,
            &ttsq,
            "sess-1",
            "sess-1#0#2",
            "> Hello there.",
            false,
            false,
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
            0,
            "an open blockquote run must not speak early"
        );

        // Second cut: the run completed (body line after the blank) — exactly ONE utterance.
        narrate(
            &paths,
            &ttsq,
            "sess-1",
            "sess-1#0#2",
            "> Hello there.\n\nBody.",
            false,
            false,
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
            1,
            "the completed run speaks exactly once"
        );

        // A replayed / still-growing batch with the same completed run re-speaks NOTHING
        // (the on-disk DisplayState high-water mark makes resends harmless).
        narrate(
            &paths,
            &ttsq,
            "sess-1",
            "sess-1#0#2",
            "> Hello there.\n\nBody. More detail.",
            true,
            false,
        );
        assert_eq!(ttsq.tts_status_sample().2, 1, "dedup: the same run never re-speaks");
    }

    #[test]
    fn narrate_batch_final_short_reply_falls_back_to_shorts() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // default narrate includes shorts

        // Mid-stream, no blockquote → silent.
        narrate(
            &paths,
            &ttsq,
            "s",
            "m1",
            "Done — all three tests",
            false,
            false,
        );
        assert_eq!(ttsq.tts_status_sample().2, 0);
        // Final batch, still blockquote-less and short → the shorts fallback voices it whole.
        narrate(
            &paths,
            &ttsq,
            "s",
            "m1",
            "Done — all three tests pass.",
            true,
            false,
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
            1,
            "is_final=true fires the shorts fallback once"
        );
    }

    #[test]
    fn narrate_batch_is_suppressed_while_the_mic_is_active() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());

        narrate(
            &paths,
            &ttsq,
            "s",
            "m1",
            "> Spoken line.\n\nBody.",
            true,
            /*mic*/ true,
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
            0,
            "mic live at message start ⇒ the whole message stays gated"
        );
    }

    #[test]
    fn narrate_batch_logs_the_session_start_exactly_once() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let log = move |m: &str| sink.lock().unwrap().push(m.to_string());

        handle_narrate_batch(
            &paths,
            &ttsq,
            &log,
            "sess-9",
            "k1".into(),
            "> Hi.".into(),
            false,
            false,
        );
        handle_narrate_batch(
            &paths,
            &ttsq,
            &log,
            "sess-9",
            "k1".into(),
            "> Hi.\n\nBody.".into(),
            true,
            false,
        );
        let logged = lines.lock().unwrap().clone();
        assert_eq!(
            logged
                .iter()
                .filter(|l| l.contains("sess-9") && l.contains("narrate-batch"))
                .count(),
            1,
            "the activity log gets ONE session-start line, not one per batch: {logged:?}"
        );
    }

    #[test]
    fn narrate_batch_rejects_control_characters_in_the_logged_session() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let log = move |m: &str| sink.lock().unwrap().push(m.to_string());

        handle_narrate_batch(
            &paths,
            &ttsq,
            &log,
            "sess\n[WARN] forged line",
            "k1".into(),
            "> Hi.\n\nBody.".into(),
            true,
            false,
        );

        assert_eq!(
            *lines.lock().unwrap(),
            ["frontend: refused narrate-batch (session contains control characters)"]
        );
        assert_eq!(
            ttsq.tts_status_sample().2,
            0,
            "a refused batch must not enqueue speech"
        );
    }

    #[test]
    fn ipc_session_end_reclaims_frontend_narration_state() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        ds_narrate::seed_witness(&paths, "frontend-session");
        assert!(ds_narrate::witness_exists(&paths, "frontend-session"));

        let grok_sessions = crate::grok_stream::SessionRegistry::new();
        handle_session_end(
            &paths,
            &ttsq,
            &grok_sessions,
            Some("frontend-session".into()),
            ClientSource::ClaudeCode,
        );

        assert!(
            !ds_narrate::witness_exists(&paths, "frontend-session"),
            "the IPC SessionEnd path must reclaim state created by NarrateBatch"
        );
    }

    #[test]
    fn narrate_batch_does_nothing_when_narration_is_configured_off() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(paths.config_toml.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_toml, "narrate = []\n").unwrap();

        narrate(&paths, &ttsq, "s", "m1", "> Spoken.\n\nBody.", true, false);
        assert_eq!(ttsq.tts_status_sample().2, 0, "narrate=[] ⇒ the verb is a no-op");
        assert!(
            !ds_narrate::witness_exists(&paths, "s"),
            "a fully-off config must not even touch the per-session state file"
        );
    }

    // ── FrontendRegistry: subscription bookkeeping + acked delivery ─────────

    use std::io::{BufRead, BufReader, Write};

    /// A registry with a NO-OP log sink: `FrontendRegistry::new` would otherwise write
    /// through `ds_log::log_from` against the REAL per-OS log file — tests must not
    /// append there (see ds-log's test-isolation docs).
    fn test_registry() -> Arc<FrontendRegistry> {
        FrontendRegistry::with_logger(Box::new(|_| {}))
    }

    /// A (scripted client stream, server-side Conn) pair over a throwaway
    /// temp-dir socket, standing in for a subscribed frontend.
    fn conn_pair() -> (ds_ipc::transport::Stream, Conn) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let listener = ds_ipc::transport::bind(&path).expect("bind test socket");
        let accept = std::thread::spawn(move || listener.accept().expect("accept").0);
        let client = ds_ipc::transport::connect(&path).expect("connect test socket");
        let server = accept.join().expect("join accept thread");
        (client, Conn::new(server).expect("wrap server stream"))
    }

    /// Pre-write an `ack_deliver` line from the scripted frontend. Seqs are
    /// deterministic (a fresh registry stamps 1, 2, 3, …), so tests buffer the
    /// ack BEFORE calling deliver — the round-trip then has no timing at all.
    fn send_ack(client: &mut ds_ipc::transport::Stream, seq: u64, ok: bool) {
        let mut line = serde_json::to_string(&ds_ipc::Request::AckDeliver { seq, ok }).unwrap();
        line.push('\n');
        client.write_all(line.as_bytes()).unwrap();
    }

    /// Read one `FrontendEvent` line off the scripted frontend side.
    fn read_event(reader: &mut BufReader<ds_ipc::transport::Stream>) -> (DictationEvent, u64) {
        let mut line = String::new();
        reader.read_line(&mut line).expect("an event line");
        match serde_json::from_str::<ds_ipc::Response>(line.trim()) {
            Ok(ds_ipc::Response::FrontendEvent { event, seq }) => (event, seq),
            other => panic!("expected a FrontendEvent line, got {other:?} from {line:?}"),
        }
    }

    #[test]
    fn deliver_with_no_subscriber_fails_immediately() {
        let reg = test_registry();
        let started = std::time::Instant::now();
        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|_| true),
            DeliverOutcome::Failed,
            "no subscriber → the caller must fall back to paste"
        );
        assert!(
            started.elapsed() < ACK_DELIVER_TIMEOUT,
            "an empty registry must not wait on any ack deadline"
        );
    }

    #[test]
    fn deliver_skips_a_subscriber_whose_app_is_not_frontmost() {
        let reg = test_registry();
        let (client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");

        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|app| app == "someone-else"),
            DeliverOutcome::Failed,
            "a live but non-frontmost subscriber must not receive the transcript"
        );
        assert_eq!(
            reg.subscriber_count(),
            1,
            "not-frontmost is no fault of the subscriber — keep it"
        );
        // Nothing was written to the skipped subscriber: the FIRST line it sees
        // is a later broadcast, not a stray deliver.
        reg.broadcast(DictationEvent::RecordingStarted);
        let mut reader = BufReader::new(client);
        let (event, _) = read_event(&mut reader);
        assert_eq!(event, DictationEvent::RecordingStarted);
    }

    #[test]
    fn deliver_acked_ok_is_delivered_and_keeps_the_subscriber() {
        let reg = test_registry();
        let (mut client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");

        // Buffer the matching ack up front (deterministic seq: first event = 1).
        send_ack(&mut client, 1, true);
        assert_eq!(
            reg.deliver_to_frontmost("hello world", true, &|app| app == "zed"),
            DeliverOutcome::Delivered
        );
        assert_eq!(
            reg.subscriber_count(),
            1,
            "an acked deliver keeps the subscription alive"
        );

        // The frontend saw the documented deliver shape, seq 1.
        let mut reader = BufReader::new(client);
        let (event, seq) = read_event(&mut reader);
        assert_eq!(seq, 1);
        assert_eq!(
            event,
            DictationEvent::Deliver {
                text: "hello world".into(),
                submit: true,
            }
        );

        // Still subscribed: a later broadcast arrives, with a LARGER seq.
        reg.broadcast(DictationEvent::Cancelled);
        let (event, seq) = read_event(&mut reader);
        assert_eq!(event, DictationEvent::Cancelled);
        assert_eq!(seq, 2, "seq is monotonic across delivers and broadcasts");
    }

    #[test]
    fn deliver_nack_fails_and_drops_the_subscriber() {
        let reg = test_registry();
        let (mut client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");

        // The frontend answers "couldn't insert" (no active window, input refused …).
        send_ack(&mut client, 1, false);
        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|_| true),
            DeliverOutcome::Failed,
            "a nack must route the utterance to the paste fallback"
        );
        assert_eq!(reg.subscriber_count(), 0, "nacked subscriber dropped");

        // Its connection was closed: the frontend reads the deliver line, then EOF.
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // the deliver line
        line.clear();
        assert_eq!(
            reader.read_line(&mut line).unwrap(),
            0,
            "dropped subscriber's conn is closed (EOF), so it knows to resubscribe"
        );
    }

    #[test]
    fn deliver_times_out_fails_and_drops_the_subscriber_when_no_ack_arrives() {
        let reg = test_registry();
        let (client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");

        let started = std::time::Instant::now();
        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|_| true),
            DeliverOutcome::Failed,
            "a silent frontend must not swallow the utterance — paste fallback"
        );
        // Write is near-instant on a live socket; the shared end-to-end budget is
        // almost entirely the ack wait. Bound well under 2×ACK (the old independent
        // write+ack ceiling) so a second full timeout cannot regress unnoticed.
        assert!(
            started.elapsed() < ACK_DELIVER_TIMEOUT + Duration::from_millis(500),
            "deliver write+ack share one ~{ACK_DELIVER_TIMEOUT:?} budget, took {:?}",
            started.elapsed()
        );
        assert_eq!(reg.subscriber_count(), 0, "timed-out subscriber dropped");
        drop(client); // keep the frontend "alive but silent" until the deliver concluded
    }

    #[test]
    fn deliver_to_a_hung_up_subscriber_fails_and_drops_it() {
        let reg = test_registry();
        let (client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");
        drop(client); // frontend gone (Zed quit) before the confirm

        // Either the write fails outright, or it lands in the closed socket and
        // the ack read sees EOF — both must resolve to Failed + drop, promptly
        // (within the shared deliver budget, not a multi-second RPC timeout).
        let started = std::time::Instant::now();
        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|_| true),
            DeliverOutcome::Failed
        );
        assert!(
            started.elapsed() < ACK_DELIVER_TIMEOUT + Duration::from_millis(500),
            "hung-up deliver must finish within the shared budget, took {:?}",
            started.elapsed()
        );
        assert_eq!(reg.subscriber_count(), 0);
    }

    #[test]
    fn stale_acks_for_older_seqs_are_skipped_not_fatal() {
        let reg = test_registry();
        let (mut client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");

        // seq 1 goes to a broadcast; the deliver below is seq 2.
        reg.broadcast(DictationEvent::RecordingStarted);
        // A late (stale) NACK for some earlier deliver sits buffered ahead of
        // the real ack — it must be skipped, not fail the CURRENT deliver.
        send_ack(&mut client, 1, false);
        send_ack(&mut client, 2, true);

        assert_eq!(
            reg.deliver_to_frontmost("hello", false, &|_| true),
            DeliverOutcome::Delivered
        );
        assert_eq!(reg.subscriber_count(), 1);
    }

    #[test]
    fn broadcast_write_error_drops_the_dead_subscriber() {
        let reg = test_registry();
        let (client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");
        drop(client);

        // A broadcast has no read step, so only the write error reveals the
        // disconnect — and that can take a write or two to surface
        // (platform-dependent buffering). Bounded loop, like ds-ipc's own test.
        for _ in 0..100 {
            reg.broadcast(DictationEvent::Partial { text: "hi".into() });
            if reg.subscriber_count() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            reg.subscriber_count(),
            0,
            "a dead subscriber must be dropped once its event write fails"
        );
    }

    #[test]
    fn resubscribing_the_same_app_evicts_and_closes_the_previous_subscriber() {
        let reg = test_registry();
        let (client1, conn1) = conn_pair();
        let (client2, conn2) = conn_pair();
        reg.subscribe("zed".into(), conn1)
            .expect("subscribe a fresh test conn");
        reg.subscribe("zed".into(), conn2)
            .expect("subscribe a fresh test conn");
        assert_eq!(reg.subscriber_count(), 1, "one live subscriber per app tag");

        // The evicted connection is CLOSED (EOF), so a stale Zed instance
        // notices instead of silently never receiving events again.
        let mut r1 = BufReader::new(client1);
        let mut line = String::new();
        assert_eq!(r1.read_line(&mut line).unwrap(), 0, "evicted conn closed");

        // Events flow to the NEW subscriber.
        reg.broadcast(DictationEvent::RecordingStarted);
        let mut r2 = BufReader::new(client2);
        let (event, _) = read_event(&mut r2);
        assert_eq!(event, DictationEvent::RecordingStarted);
    }

    #[test]
    fn distinct_app_tags_coexist_and_both_receive_broadcasts() {
        let reg = test_registry();
        let (client_a, conn_a) = conn_pair();
        let (client_b, conn_b) = conn_pair();
        reg.subscribe("zed".into(), conn_a)
            .expect("subscribe a fresh test conn");
        reg.subscribe("other-editor".into(), conn_b)
            .expect("subscribe a fresh test conn");
        assert_eq!(reg.subscriber_count(), 2);

        reg.broadcast(DictationEvent::Cancelled);
        for client in [client_a, client_b] {
            let mut reader = BufReader::new(client);
            let (event, _) = read_event(&mut reader);
            assert_eq!(event, DictationEvent::Cancelled);
        }
    }

    #[test]
    fn any_subscriber_frontmost_requires_a_live_subscriber_whose_app_matches() {
        // The engine's per-dictation ownership probe (`start_recording`): an empty
        // registry is never frontmost-owned no matter what the platform reports…
        let reg = test_registry();
        assert!(!reg.any_subscriber_frontmost(&|_| true));

        // …a live subscriber owns the next dictation only when ITS app matches the
        // probe…
        let (_client, conn) = conn_pair();
        reg.subscribe("zed".into(), conn)
            .expect("subscribe a fresh test conn");
        assert!(reg.any_subscriber_frontmost(&|app| app == "zed"));
        assert!(!reg.any_subscriber_frontmost(&|app| app == "someone-else"));
        assert!(!reg.any_subscriber_frontmost(&|_| false));
    }

    #[test]
    fn subscribe_frontend_is_refused_when_the_kill_switch_is_off() {
        let reg = test_registry();
        let (client, conn) = conn_pair();

        let outcome = handle_subscribe_frontend(&reg, false, "zed".into(), conn)
            .expect("refusal writes one line — no transport error expected");
        assert!(
            matches!(outcome, HandleOutcome::Done(_)),
            "the connection goes BACK to the normal request loop, not taken over"
        );
        assert_eq!(reg.subscriber_count(), 0, "nothing registered");

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        match serde_json::from_str::<ds_ipc::Response>(line.trim()) {
            Ok(ds_ipc::Response::Error { message }) => assert!(
                message.contains("frontend_enabled"),
                "the refusal must name the kill-switch so the user can find it: {message}"
            ),
            other => panic!("expected a terminal Error line, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_frontend_takes_over_and_registers_when_enabled() {
        let reg = test_registry();
        let (client, conn) = conn_pair();

        let outcome = handle_subscribe_frontend(&reg, true, "zed".into(), conn).expect("no io");
        assert!(matches!(outcome, HandleOutcome::TookOver));
        assert_eq!(reg.subscriber_count(), 1);

        // The subscription is live: events stream to the client.
        reg.broadcast(DictationEvent::RecordingStarted);
        let mut reader = BufReader::new(client);
        let (event, seq) = read_event(&mut reader);
        assert_eq!(event, DictationEvent::RecordingStarted);
        assert_eq!(seq, 1);
    }

    #[test]
    fn subscribe_frontend_is_refused_for_an_unknown_app_tag() {
        // The bound on unbounded subscriber growth (a caller retrying with a
        // distinct tag each time) is THIS check: `FrontendRegistry::subscribe`
        // only evicts a same-tag entry, so an unchecked `app` would let the
        // registry grow one entry per distinct tag forever.
        let reg = test_registry();
        let (client, conn) = conn_pair();

        let outcome = handle_subscribe_frontend(&reg, true, "vscode".into(), conn)
            .expect("refusal writes one line — no transport error expected");
        assert!(
            matches!(outcome, HandleOutcome::Done(_)),
            "an unrecognized tag goes BACK to the normal request loop, not taken over"
        );
        assert_eq!(
            reg.subscriber_count(),
            0,
            "nothing registered for an unknown tag"
        );

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        match serde_json::from_str::<ds_ipc::Response>(line.trim()) {
            Ok(ds_ipc::Response::Error { message }) => assert!(
                message.contains("vscode"),
                "the refusal should name the rejected tag: {message}"
            ),
            other => panic!("expected a terminal Error line, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_frontend_rejects_control_characters_before_logging_the_tag() {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let reg = FrontendRegistry::with_logger(Box::new(move |line| {
            sink.lock().unwrap().push(line.to_string());
        }));
        let (client, conn) = conn_pair();

        let outcome = handle_subscribe_frontend(&reg, true, "zed\nFORGED".into(), conn)
            .expect("refusal writes one line");
        assert!(matches!(outcome, HandleOutcome::Done(_)));
        assert_eq!(reg.subscriber_count(), 0);
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .all(|line| !line.chars().any(char::is_control)),
            "untrusted app tags must never inject activity-log lines"
        );

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        match serde_json::from_str::<ds_ipc::Response>(line.trim()) {
            Ok(ds_ipc::Response::Error { message }) => {
                assert!(message.contains("control characters"));
                assert!(!message.contains("FORGED"));
            }
            other => panic!("expected a terminal Error line, got {other:?}"),
        }
    }

    #[test]
    fn resubscribe_shuts_down_a_stale_clone_held_across_the_eviction_race() {
        // Pins the race a reviewer flagged: `broadcast`/`deliver_to_frontmost`
        // clone the `Arc<Subscriber>` BEFORE locking its conn, so a concurrent
        // resubscribe's `retain` alone can't close the evicted socket — the
        // clone's refcount keeps it alive. `subscribe` must shut down evicted
        // sockets explicitly so a write through a stale clone FAILS instead of
        // silently reaching the client that already lost the subscription.
        let reg = test_registry();
        let (client1, conn1) = conn_pair();
        reg.subscribe("zed".into(), conn1)
            .expect("subscribe a fresh test conn");

        // Stand-in for the Arc clone `broadcast`/`deliver_to_frontmost` would
        // have taken before the resubscribe below runs.
        let stale = reg.subscribers.lock().unwrap()[0].clone();

        let (client2, conn2) = conn_pair();
        reg.subscribe("zed".into(), conn2)
            .expect("subscribe a fresh test conn");
        assert_eq!(
            reg.subscriber_count(),
            1,
            "only the new subscriber is registered"
        );

        let write_result = stale
            .conn
            .lock()
            .unwrap()
            .send(&ds_ipc::Response::FrontendEvent {
                event: DictationEvent::Cancelled,
                seq: 999,
            });
        assert!(
            write_result.is_err(),
            "the evicted subscriber's socket must already be closed, so a write \
             through a stale clone fails instead of reaching the replaced client"
        );

        // The NEW subscriber is unaffected.
        reg.broadcast(DictationEvent::RecordingStarted);
        let mut r2 = BufReader::new(client2);
        let (event, _) = read_event(&mut r2);
        assert_eq!(event, DictationEvent::RecordingStarted);
        drop(client1);
    }

    /// Companion to the write-side eviction race test above (issue #111 M2).
    ///
    /// Finding (Windows host, `uds_windows` AF_UNIX): `ShutdownHandle::shutdown`
    /// alone does **not** unblock a concurrent `recv_deadline` on the reader
    /// handle — the deliver thread sat out ~full remaining `ACK_DELIVER_TIMEOUT`.
    /// Unix typically returns EOF from `shutdown(Both)`; we can't rely on that.
    ///
    /// Mitigation: resubscribe sets `Subscriber::cancelled` and `wait_for_ack`
    /// short-polls, so eviction is noticed within ~`ACK_POLL` regardless of OS
    /// wake. Timing is measured from the resubscribe call (not deliver start)
    /// and must be well under half the ack deadline — a full remaining timeout
    /// fails this assert.
    #[test]
    fn resubscribe_unblocks_a_pending_ack_wait() {
        let reg = test_registry();
        let (client1, conn1) = conn_pair();
        reg.subscribe("zed".into(), conn1)
            .expect("subscribe a fresh test conn");

        let reg_deliver = Arc::clone(&reg);
        let deliver = std::thread::spawn(move || {
            reg_deliver.deliver_to_frontmost("hello", false, &|_| true)
        });

        // Deliver write completed ⇒ engine holds the conn lock inside
        // `wait_for_ack`. Keep client1 open so the wake comes from shutdown,
        // not peer hangup.
        let mut r1 = BufReader::new(client1);
        let (event, seq) = read_event(&mut r1);
        assert_eq!(seq, 1);
        assert_eq!(
            event,
            DictationEvent::Deliver {
                text: "hello".into(),
                submit: false,
            }
        );

        let (_client2, conn2) = conn_pair();
        let shutdown_at = std::time::Instant::now();
        reg.subscribe("zed".into(), conn2)
            .expect("resubscribe must evict + shut down the waiting peer");

        let outcome = deliver.join().expect("deliver thread");
        let since_shutdown = shutdown_at.elapsed();
        assert_eq!(
            outcome,
            DeliverOutcome::Failed,
            "eviction mid-ack must fail the deliver (paste fallback), not hang"
        );
        assert!(
            since_shutdown < ACK_DELIVER_TIMEOUT / 2,
            "shutdown must unblock recv_deadline well under ACK_DELIVER_TIMEOUT \
             (~{ACK_DELIVER_TIMEOUT:?}); took {since_shutdown:?} after resubscribe"
        );
        assert_eq!(
            reg.subscriber_count(),
            1,
            "the new subscriber stays registered after the old one's failed deliver"
        );
    }

    // `#[cfg(unix)]`: needs a send buffer small enough that an unread 32 MiB
    // write actually blocks. Linux/macOS's default `AF_UNIX` buffer is
    // comfortably under that; `uds_windows`' Windows backing socket does not
    // reliably block at any practical test payload size (see `ds-ipc`'s
    // matching test), and CI (`ci.yml`) is Linux-only anyway.
    #[cfg(unix)]
    #[test]
    fn subscription_writes_are_bounded_even_when_the_frontend_stalls() {
        // Without `set_write_timeout` in `handle_subscribe_frontend`, this conn
        // would keep `Conn::new`'s constructor default (5s RPC timeout) — so a
        // frontend that stops reading could block the engine tick thread for
        // seconds on a single write, well past the shared deliver budget.
        let reg = test_registry();
        let (client, conn) = conn_pair();
        let outcome = handle_subscribe_frontend(&reg, true, "zed".into(), conn).expect("no io");
        assert!(matches!(outcome, HandleOutcome::TookOver));

        // Never read from `client`. Repeated moderate events fill the socket
        // without making debug-mode JSON serialization dominate the timing; the
        // first blocked send must fail within the configured subscription bound.
        let event = DictationEvent::Partial {
            text: "x".repeat(256 * 1024),
        };
        let mut timed_out = false;
        for _ in 0..128 {
            let started = std::time::Instant::now();
            let subscriber_remains = reg.broadcast(event.clone());
            assert!(
                started.elapsed() < std::time::Duration::from_secs(1),
                "a stalled subscription write must be bounded near ACK_DELIVER_TIMEOUT \
                 (~300ms), not the constructor's 5s default; took {:?}",
                started.elapsed()
            );
            if !subscriber_remains {
                timed_out = true;
                break;
            }
        }
        assert!(
            timed_out,
            "the unread socket must eventually reach its write bound"
        );
        assert_eq!(
            reg.subscriber_count(),
            0,
            "the timed-out write must drop the wedged subscriber"
        );
        drop(client);
    }
}
