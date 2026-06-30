//! The RPC server thread + its request-dispatch arms.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_config::{DropSpeechKind, Paths, VoiceConfig};

use crate::downloads::{DownloadProg, start_download};
use crate::logging::log;
use crate::status::{EngineShared, model_status_json};
use crate::stt_test::TestSession;
use crate::ttsq::TtsQueue;

/// Host the RPC socket on a dedicated thread (blocking accept loop), dispatching
/// each request inline. A `Reload` (the MCP/GUI wrote settings.json and asks us to
/// apply it) flips `reload_requested` so the poll loop reloads config surgically
/// via `Engine::reload`; the other arms drive the TTS queue, model status, the STT
/// test, the provider switch, and speaker enroll/diarize.
pub(crate) fn spawn_ipc_server(
    shared: EngineShared,
    paths: Paths,
    running: Arc<AtomicBool>,
    stt_test: Arc<TestSession>,
    ttsq: Arc<TtsQueue>,
    reload_requested: Arc<AtomicBool>,
    downloads: DownloadProg,
) {
    let sock = paths.engine_sock.clone();
    std::thread::spawn(move || {
        let handler = move |req: ds_ipc::Request, emit: &mut dyn FnMut(&ds_ipc::Response)| {
            match req {
                ds_ipc::Request::Ping => emit(&ds_ipc::Response::Pong),
                ds_ipc::Request::Status => {
                    let (tts_active, queued, paused) = ttsq.snapshot();
                    emit(&ds_ipc::Response::Status {
                        tts_active,
                        queued,
                        paused,
                    });
                }
                ds_ipc::Request::EnsureKokoroVoices => {
                    // Single-flight, non-blocking: kick the voices-npz download only if
                    // absent; `start_download` no-ops when a download is already in flight,
                    // so the request joins the engine's background download
                    // instead of racing a second one.
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
                    ttsq.greet_session(session);
                    emit(&ds_ipc::Response::Done);
                }
                ds_ipc::Request::MarkActive { session } => {
                    // UserPromptSubmit → this terminal is now the active one. The queue
                    // speaks only its items and holds the rest until they're active.
                    ttsq.set_active_session(session.clone());
                    // `drop_speech_on` contains `text`: the UserPromptSubmit hook fires for
                    // EVERY submit (typed OR dictated), so this is the text-submit drop
                    // point. BUT a VOICE submit also pressed Enter via the engine — de-dup so
                    // that auto-Enter doesn't count as a text submit: if a voice submit
                    // just happened, this hook is its echo, so skip (the `voice` path handled
                    // it). Read config live so a runtime `set_config` change takes effect
                    // without an engine restart.
                    let was_voice = ttsq.take_recent_voice_submit();
                    if !was_voice
                        && VoiceConfig::load(&paths)
                            .drop_speech_on
                            .contains(&DropSpeechKind::Text)
                    {
                        ttsq.clear_session(session);
                    }
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
                ds_ipc::Request::SetProvider { which } => {
                    // set_provider restarts the warm child (which hosts BOTH Kokoro and
                    // Parakeet) and resets both engines' stats when the active provider
                    // actually changes — centralized in restart_child, so this path AND
                    // the set_config/config-reload path (apply_tts_provider) both get it.
                    shared.tts.set_provider(&which);
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
                    if let Some(ev) = ds_config::EarconEvent::parse(&event) {
                        // The configured sound IS the on/off: `resolve_cue` returns None when
                        // this event's sound is empty or unresolvable, so an unset cue is simply
                        // silent. Still honor global mute.
                        let cfg = VoiceConfig::load(&paths);
                        if !shared.tts.is_muted()
                            && let Some(path) = ds_config::resolve_cue(&cfg, ev)
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
                    let secs = seconds.clamp(1, 60);
                    match ttsq.diarize(secs) {
                        Ok(json) => match diarize_named_segments(&json, &paths) {
                            Ok(segments) => emit(&ds_ipc::Response::Diarization { segments }),
                            Err(e) => emit(&ds_ipc::Response::error(format!("diarize: {e}"))),
                        },
                        Err(e) => emit(&ds_ipc::Response::error(format!("diarize: {e}"))),
                    }
                }
                ds_ipc::Request::Enroll { name, seconds } => {
                    // Record a sample, extract a voiceprint, persist it under `name`.
                    let secs = seconds.clamp(1, 60);
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        emit(&ds_ipc::Response::error("enroll: name must not be empty"));
                    } else {
                        match ttsq.enroll(secs) {
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
                            Err(e) => emit(&ds_ipc::Response::error(format!("enroll: {e}"))),
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
