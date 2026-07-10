//! The RPC server thread + its request-dispatch arms.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_config::{CancelSpeechScope, Paths, VoiceConfig};

use crate::downloads::{DownloadProg, start_download};
use crate::logging::log;
use crate::status::{EngineShared, model_status_json};
use crate::stt_test::TestSession;
use crate::ttsq::TtsQueue;

/// Whether a `MarkActive` (UserPromptSubmit fires for EVERY submit, typed or dictated)
/// should cancel speech per the user's `input_clears` preference. `was_voice` is true
/// when a voice submit JUST pressed Enter via the engine itself — this hook is that
/// submit's own echo, so it must NOT also count as a separate, genuine submit (else it
/// would immediately re-cancel speech the voice path already handled directly). PURE.
pub(crate) fn should_cancel_on_submit(was_voice: bool, scope_configured: bool) -> bool {
    !was_voice && scope_configured
}

/// Apply a `MarkActive` ping (UserPromptSubmit hook — every submit: typed, dictated,
/// or, when `synthetic`, a harness-injected continuation Claude Code auto-re-invokes,
/// e.g. a background-task `<task-notification>` re-invocation — issue #11).
///
/// The `codex_sessions` nudge ALWAYS runs, `synthetic` or not: it's pure
/// session-liveness bookkeeping (this session id is still alive), not a claim about
/// human intent, and it doubles as `codex_stream` RE-discovery after an engine
/// restart.
///
/// Everything else is the "you just moved your attention here" half of `MarkActive`
/// — claiming active-terminal status (`set_active_session`) and applying
/// `input_clears` — and is skipped ENTIRELY when `synthetic`: a harness continuation
/// expresses no human "I've moved on" intent, so it must neither steal active-terminal
/// status from whatever real terminal the user is actually in, nor prune/cancel
/// in-flight speech the user may be mid-listen to. See
/// `dontspeak::hook_speak::is_synthetic_continuation`, the hook-side classifier that
/// sets this flag from the prompt body's shape.
fn handle_mark_active(
    ttsq: &TtsQueue,
    codex_sessions: &crate::codex_stream::SessionRegistry,
    paths: &Paths,
    session: Option<String>,
    synthetic: bool,
) {
    // The nudge doubles as codex_stream session RE-discovery after an engine restart
    // (SessionStart won't re-fire mid-session), and re-arms a negative-cached
    // resolution. Runs unconditionally: session-liveness, not human intent.
    if let Some(s) = &session {
        codex_sessions.nudge(s);
    }
    if synthetic {
        // A harness-injected continuation carries no "I've moved on" human intent:
        // don't steal active-terminal status, don't touch the TTS queue at all.
        return;
    }
    // UserPromptSubmit → this terminal is now the active one. The queue speaks only
    // its items and holds the rest until they're active.
    ttsq.set_active_session(session.clone());
    // The UserPromptSubmit hook fires for EVERY genuine submit (typed OR dictated), so
    // this is where a genuinely-typed submit is caught. BUT a VOICE submit also
    // pressed Enter via the engine — de-dup so that auto-Enter isn't treated as a
    // second, separate submit: if a voice submit just happened, this hook is its echo
    // (the voice path already applied `input_clears` directly), so skip. Read config
    // live so a runtime `set_config` change takes effect without an engine restart.
    let was_voice = ttsq.take_recent_voice_submit();
    // Short-circuit: skip the settings.json read entirely when `was_voice` already
    // decides it (`should_cancel_on_submit` would read `false` either way, but
    // there's no reason to load config to learn that).
    if !was_voice {
        let scopes = VoiceConfig::load(paths).input_clears;
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Current)) {
            ttsq.clear_session(session.clone());
        }
        if should_cancel_on_submit(was_voice, scopes.contains(&CancelSpeechScope::Other)) {
            // Pass this REQUEST's own `session` as the target directly, rather than
            // re-deriving "active" via `ttsq.active_session()` — `set_active_session`
            // above already set it to exactly this session, but re-reading it would
            // reopen a window for a concurrent MarkActive from another terminal to
            // land in between and be treated as "other" instead of this one.
            ttsq.cancel_for_submit(session.clone(), false, true);
        }
    }
}

/// Host the RPC socket on a dedicated thread (blocking accept loop), dispatching
/// each request inline. A `Reload` (the MCP/GUI wrote settings.json and asks us to
/// apply it) flips `reload_requested` so the poll loop reloads config surgically
/// via `Engine::reload`; the other arms drive the TTS queue, model status, the STT
/// test, the provider switch, and speaker enroll/diarize.
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
) {
    let sock = paths.engine_sock.clone();
    std::thread::spawn(move || {
        let handler = move |req: ds_ipc::Request, emit: &mut dyn FnMut(&ds_ipc::Response)| {
            match req {
                ds_ipc::Request::Ping => emit(&ds_ipc::Response::Pong),
                ds_ipc::Request::Status => {
                    let (tts_active, queued, paused, muted) = ttsq.snapshot();
                    emit(&ds_ipc::Response::Status {
                        tts_active,
                        queued,
                        paused,
                        muted,
                    });
                }
                ds_ipc::Request::EnsureKokoroVoices => {
                    // Non-blocking: kick the voices-npz download only if absent.
                    // `start_download` is single-flight PER TARGET — if this target is
                    // already fetching, the request ATTACHES to it (and it runs in
                    // parallel with any other target's download, never queued behind one).
                    let present = ds_model::model_path(ds_model::KOKORO_VOICES_FILE)
                        .is_some_and(|p| p.is_file());
                    if !present {
                        start_download(&downloads, ds_model::DownloadTarget::KokoroVoices);
                    }
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::GreetSession { session } => {
                    // New terminal opened → greet in its assigned pool voice (no-op unless
                    // `greet_on_open` is set). Claims the session's voice at open time.
                    // Also the codex_stream supervisor's session DISCOVERY: a session id
                    // the hooks vouch for may map to a codex app-server thread (CC/Qwen
                    // ids simply never match one).
                    if let Some(s) = &session {
                        codex_sessions.nudge(s);
                    }
                    ttsq.greet_session(session);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::MarkActive { session, synthetic } => {
                    handle_mark_active(&ttsq, &codex_sessions, &paths, session, synthetic);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Speak {
                    text,
                    voice,
                    rate,
                    session,
                } => {
                    // Explicit (MCP `speak` tool) reply → enqueue on the TTS queue (the
                    // single serializer onto the warm child). The queue worker picks the
                    // engine from live config (or this session's override) and gates on
                    // the mic.
                    ttsq.enqueue(text, voice, rate, session);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SpeakNarration { text, session } => {
                    // Mid-turn narration → enqueue onto the same FIFO as everything else
                    // (no kind, no cap). Warm path: no per-block model reload.
                    ttsq.enqueue(text, None, None, session);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SetMuted { on } => {
                    // Global mute toggle (tray checkbox). Silences playback without stopping it.
                    ttsq.set_muted(on);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::StopSpeech { session } => {
                    // None = global hard barge (drop the whole queue + cancel the
                    // current item). Some(s) = per-window: prune only that session's
                    // items and cancel playback only if it's that session's, so one
                    // terminal's preempt/close never silences another's.
                    match session {
                        None => ttsq.clear(),
                        Some(_) => ttsq.clear_session(session),
                    }
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::SessionEnd { session } => {
                    // Window closed for good: per-window barge AND forget this session's
                    // transient pool-voice assignment so it doesn't grow one entry per session forever.
                    // None (no session id) → global hard barge, nothing session-scoped to forget.
                    match session {
                        None => ttsq.clear(),
                        Some(_) => ttsq.end_session(session),
                    }
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
                        status: model_status_json(&shared, &paths, ttsq.is_tts_active()),
                    });
                }
                ds_ipc::Request::WaitModelStatus { since, timeout_ms } => {
                    // PUSH transport: block this (dedicated) connection until the
                    // dictation status changes or the cap elapses, then reply with the
                    // fresh snapshot. One-thread-per-connection (see ipc server), so this
                    // never stalls the timer's ModelStatus / SetMuted on other connections.
                    let timeout = std::time::Duration::from_millis(timeout_ms.clamp(1, 60_000));
                    shared.gate.wait_changed(since, timeout);
                    emit(&ds_ipc::Response::ModelStatus {
                        status: model_status_json(&shared, &paths, ttsq.is_tts_active()),
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
                    // The MCP/GUI wrote settings.json and asks us to apply it NOW.
                    // Flip the same flag SIGHUP uses; the poll loop reloads next tick
                    // (debounced, re-reading config from settings.json). No mtime wait.
                    reload_requested.store(true, Ordering::Relaxed);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::Earcon { event } => {
                    // Turn-end "ding" (Stop hook) / needs-input cue (Notification hook). Resolve
                    // the configured-or-introspected sound and play it on the warm child's audio
                    // path — OUTSIDE the TTS queue, so it never waits behind queued narration.
                    // Skipped when earcons are off or muted, or the sound can't be resolved.
                    if let Some(ev) = ds_earcon::EarconEvent::parse(&event) {
                        // The configured sound IS the on/off: `resolve_cue` returns None when
                        // this event's sound is empty or unresolvable, so an unset cue is simply
                        // silent. Still honor global mute.
                        let cfg = VoiceConfig::load(&paths);
                        if !shared.tts.is_muted()
                            && let Some(path) = ds_earcon::resolve_cue(
                                &cfg.earcon_reply_sound,
                                &cfg.earcon_needs_input_sound,
                                ev,
                            )
                        {
                            shared.tts.cue(&path.to_string_lossy());
                        }
                    }
                    emit(&ds_ipc::Response::Done);
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
                    match run_bounded_capture(&shared.stt_active, "diarize", seconds, move |secs| {
                        ttsq.diarize(secs)
                    }) {
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
                ds_ipc::Request::Shutdown => {
                    // Ack first, then ask the main loop to exit (it tears down the
                    // warm child, removes the pidfile + socket, and process::exits).
                    emit(&ds_ipc::Response::Done);
                    running.store(false, Ordering::Relaxed);
                }
            }
        };
        if let Err(e) = ds_ipc::serve(&sock, handler) {
            log(&format!("WARN: IPC server exited: {e}"));
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
    let secs = seconds.clamp(1, 60);
    // Bounded wait: `TtsManager::diarize`/`enroll` block on a condvar with no timeout of
    // their own, so a wedged/silent helper would otherwise hang THIS connection forever.
    let timeout = std::time::Duration::from_secs(secs + 30);
    call_with_timeout(timeout, move || f(secs)).map_err(|e| format!("{op_label}: {e}"))
}

/// Parse the helper's diarize JSON (`{segments, speakers}`), match each speaker cluster
/// to an enrolled voiceprint (cosine ≥ `speaker_threshold`), attach the matched name to
/// that cluster's segments, and return the segments as a JSON array. Unmatched clusters
/// keep their numeric id. No enrolled speakers ⇒ segments pass through unnamed.
fn diarize_named_segments(json: &str, paths: &Paths) -> Result<serde_json::Value, String> {
    let mut out = ds_stt::diarize::parse_output(json)?;
    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
    if !store.is_empty() {
        let threshold = VoiceConfig::load(paths).speaker_threshold;
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
    fn mark_active_synthetic_does_not_claim_active_or_cancel_speech() {
        let ttsq = TtsQueue::test_stub();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // no config.toml → default input_clears=[current]
        let codex_sessions = crate::codex_stream::SessionRegistry::new();

        ttsq.set_active_session(Some("other".into()));
        ttsq.enqueue("hi".into(), None, None, Some("a".into()));

        handle_mark_active(&ttsq, &codex_sessions, &paths, Some("a".into()), true);

        assert_eq!(
            ttsq.active_session(),
            Some("other".into()),
            "a synthetic continuation must not steal active-terminal status"
        );
        assert_eq!(
            ttsq.snapshot().1,
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

        ttsq.enqueue("hi".into(), None, None, Some("a".into()));

        handle_mark_active(&ttsq, &codex_sessions, &paths, Some("a".into()), false);

        assert_eq!(ttsq.active_session(), Some("a".into()));
        assert_eq!(
            ttsq.snapshot().1,
            0,
            "default input_clears=[current] still prunes a genuine submit's own queued item"
        );
    }

    #[test]
    fn run_bounded_capture_refuses_while_dictation_is_active() {
        let busy = AtomicBool::new(true);
        let result: Result<u32, String> = run_bounded_capture(&busy, "diarize", 5, |_secs| Ok(42));
        assert_eq!(
            result,
            Err("diarize: dictation is active; try again after it ends".to_string())
        );
    }

    #[test]
    fn run_bounded_capture_clamps_seconds_to_1_through_60() {
        let idle = AtomicBool::new(false);

        let seen = Arc::new(Mutex::new(0u64));
        let seen2 = seen.clone();
        assert_eq!(
            run_bounded_capture(&idle, "enroll", 999, move |secs| {
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
            run_bounded_capture(&idle, "enroll", 0, move |secs| {
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
        let result: Result<(), String> = run_bounded_capture(&idle, "diarize", 5, |_secs| {
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
