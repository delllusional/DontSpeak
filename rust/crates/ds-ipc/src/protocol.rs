//! The RPC wire protocol: one JSON [`Request`] per line in, one-or-more JSON
//! [`Response`] lines out. A streaming request (STT test-recognition) emits several
//! non-terminal `Response` lines and ends with a terminal one.
//!
//! Config is carried as a `serde_json::Value` — the `voice` object in the exact
//! shape `settings.json` uses (`ds_config::voice_to_value` / `voice_from_value`),
//! so neither side needs a parallel serializable mirror of `VoiceConfig`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A client → engine request. `#[serde(tag = "cmd")]` so each line is a small
/// self-describing object, e.g. `{"cmd":"ping"}` or `{"cmd":"speak","text":"hi"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Liveness/health probe → [`Response::Pong`].
    Ping,
    /// Snapshot of the TTS queue's playback state → [`Response::Status`]. Read-only.
    Status,
    /// Ensure the Kokoro voices npz (~28 MB) is present, downloading it in the background
    /// via the single-flight download manager if absent. Returns immediately — does NOT
    /// wait for the download. → [`Response::Done`].
    EnsureKokoroVoices,
    /// Set global MUTE (the tray checkbox; the Caps-tap toggles it engine-side). Muting
    /// silences playback WITHOUT stopping it — the queue keeps draining. → [`Response::Done`].
    SetMuted { on: bool },
    /// A terminal/session just opened (SessionStart hook). If `greet_on_open` is set,
    /// the engine claims this session's pool voice and speaks a short greeting in it.
    /// No-op when greeting is off. `session` is ambient (the hook's `session_id`).
    /// → [`Response::Done`].
    GreetSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Mark this session as the ACTIVE terminal — the one you just submitted a prompt
    /// to (UserPromptSubmit hook). The TTS queue then speaks only this session's items
    /// and HOLDS the others (paused, not dropped) until they become active, so
    /// narration follows the terminal you're working in. `session` is ambient (the
    /// hook's `session_id`); absent ⇒ the default/global session. → [`Response::Done`].
    MarkActive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Speak `text` as a Reply on the engine's TTS queue (survives a record-barge when
    /// the resume policy is set). Used by the MCP `speak` tool for explicit, model-driven
    /// speech; assistant-reply narration goes through `SpeakNarration` instead.
    /// `voice`/`rate` override config.
    Speak {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<f32>,
        /// The Claude session this reply belongs to (ambient; see [`Request::MarkActive`]).
        /// The engine tags the queued item with it so per-session playback routing (active
        /// window, pool voice) resolves correctly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Enqueue `text` as mid-turn NARRATION on the engine's TTS queue (dropped
    /// first on a record-barge / skip-ahead). The engine splits it into
    /// sentences and plays them on the warm child — replaces the old cold
    /// per-block spawn so there is no model reload between blocks.
    SpeakNarration {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Barge-in: stop in-flight speech. `session` scopes it to ONE window (Claude
    /// session): only that session's queued items are dropped, and the playing item
    /// is cancelled only if it belongs to that session — other windows keep talking.
    /// `None` (absent on the wire) is the GLOBAL hard barge: drop the whole queue and
    /// cancel whatever is playing (caps long-press / a non-session CLI caller).
    /// `session` is ambient (see [`Request::MarkActive`]), never a tool argument.
    StopSpeech {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// A window/terminal closed for good (Claude Code `SessionEnd`). Like a per-window
    /// [`StopSpeech`](Request::StopSpeech) (drop this session's queued + in-flight speech),
    /// but ALSO reclaims the session's transient voice state — its preferred-pool
    /// assignment — so that map doesn't grow one entry per session for the engine's
    /// lifetime. `None` (no session id) is the global hard barge, same as
    /// `StopSpeech { None }`, and forgets nothing session-scoped.
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Start a live Parakeet "test recognition" session. The engine streams
    /// [`Response::Listening`], ending with [`Response::Transcript`] (terminal)
    /// when the session stops.
    TestRecognitionStart,
    /// Stop the active test-recognition session (sent on a SECOND connection,
    /// since the first is busy streaming). The session then runs its final pass
    /// and emits its terminal `Transcript` on the streaming connection.
    TestRecognitionStop,
    /// One-shot speaker diarization: record the mic for `seconds`, then return who
    /// spoke when. Unlike test-recognition this is NOT streamed — the engine records
    /// a fixed window on the warm helper, runs the diarizer, and replies with a single
    /// terminal [`Response::Diarization`]. On-demand (the `diarize` MCP tool).
    Diarize { seconds: u64 },
    /// Enroll a voiceprint: record the mic for `seconds`, extract a WeSpeaker embedding,
    /// and persist it under `name` so future [`Diarize`](Request::Diarize) labels that
    /// person by name. Replies [`Response::Enrolled`]. (the `enroll` MCP tool).
    Enroll { name: String, seconds: u64 },
    /// Remove an enrolled voiceprint by name → [`Response::Done`] (no-op if absent).
    ForgetSpeaker { name: String },
    /// List enrolled speaker names → [`Response::Speakers`].
    ListSpeakers,
    /// Ask the engine for model presence + removability. The engine is the
    /// authority because it knows what it has loaded: a model is `removable` only
    /// if present AND not currently running in the engine (e.g. the warm Kokoro
    /// child). Download/delete file IO stays in the app. → [`Response::ModelStatus`].
    ModelStatus,
    /// Like [`ModelStatus`](Request::ModelStatus) but BLOCKS until the engine's status
    /// sequence differs from `since`, or `timeout_ms` elapses — then replies with the
    /// current [`Response::ModelStatus`] (whose JSON carries the new `seq`). This is the
    /// PUSH transport for the dictation overlay: the app calls it on a dedicated thread
    /// and re-renders the instant a partial lands, instead of polling on a timer. Pass
    /// `since = 0` for the first call (replies immediately with the current state + seq).
    WaitModelStatus { since: u64, timeout_ms: u64 },
    /// Set the TTS execution provider for THIS session and RESTART the warm Kokoro
    /// child so the new ONNX session uses it; the engine resets its TTS stats so the
    /// app's range bars reflect only the new provider. `which` = "cpu" | "cuda" |
    /// "coreml" | "ane" | "auto". Transient (not persisted). → [`Response::Done`].
    SetProvider { which: String },
    /// Ask the engine to exit cleanly over IPC; replies [`Response::Done`] just
    /// before shutting down. NOTE: the engine runs in-process inside the native
    /// app, so the real shutdown on quit is the FFI `ds_engine_stop` (clears
    /// the run flag, joins the thread) — no current client sends this request; the
    /// handler is kept for an out-of-process / socket-driven stop.
    Shutdown,
    /// Apply `settings.json` NOW — the explicit "reload" nudge. The MCP/GUI writes
    /// settings.json (still the source of truth), then sends this so the engine
    /// reloads immediately and surgically via `Engine::reload` instead of waiting
    /// for the mtime poll. Same effect as an mtime-triggered reload; debounced with it.
    /// → [`Response::Done`].
    Reload,
    /// Play an audible EARCON now (fire-and-forget). `event` is `"reply_done"` (the Stop
    /// hook — Claude finished its turn) or `"needs_input"` (the Notification hook — a
    /// permission prompt / idle). The engine resolves the configured-or-introspected sound
    /// and plays it on the warm helper's audio output, honoring the `earcon_enabled` config
    /// and global mute. Unknown/disabled ⇒ silent no-op. → [`Response::Done`].
    Earcon { event: String },
    /// Verify (and, if needed, REQUEST) authorization for the System STT engine
    /// (macOS on-device `SFSpeechRecognizer`). The engine prompts on first use — so the
    /// TCC prompt is attributed to DontSpeak.app — then re-checks on-device capability.
    /// `set_config stt_engine=system` sends this BEFORE persisting, and refuses to enable
    /// (no fallback) when it isn't usable. → [`Response::Done`] when usable, else
    /// [`Response::Error`] with the reason.
    AuthorizeSystemStt,
}

/// An engine → client response line. `#[serde(tag = "ok")]` keeps lines small and
/// unambiguous. Test-recognition emits `Listening`/`Partial` lines then a terminal
/// `Transcript` (or `Error`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::Status`] (TERMINAL): live TTS-queue playback state.
    /// `paused` is set during a record-barge hold; `muted` is the global mute
    /// (the same flag the tray checkbox / Caps-Lock toggle / `SetMuted` drive) —
    /// when true, output keeps queuing but plays SILENTLY.
    Status {
        tts_active: bool,
        queued: usize,
        paused: bool,
        muted: bool,
    },
    /// Generic success terminator for a request that returns no payload.
    Done,
    /// Test recognition: mic open, speak now (non-terminal).
    Listening,
    /// Test recognition: live partial transcript (non-terminal).
    Partial { text: String },
    /// Test recognition: final transcript (TERMINAL).
    Transcript { text: String },
    /// Diarization result (TERMINAL): `segments` is the JSON array
    /// `[{"speaker","start","end","name"?}, ...]` (seconds), in time order; `name` is
    /// the enrolled person a cluster matched, when present.
    Diarization { segments: Value },
    /// Enrollment succeeded (TERMINAL): echoes the enrolled `name`.
    Enrolled { name: String },
    /// Enrolled-speaker names (TERMINAL).
    Speakers { names: Vec<String> },
    /// Model presence + removability + per-subsystem running state (TERMINAL).
    /// `status` is a JSON object:
    /// `{ "kokoro": {"present":bool,"removable":bool}, "onnx": {...},
    ///    "parakeet": {"present":bool,"removable":bool},
    ///    "running": {"caps":bool,"kokoro":bool,"parakeet":bool} }`.
    ModelStatus { status: Value },
    /// Terminal error for any request.
    Error { message: String },
}

impl Response {
    /// Convenience constructor for an error terminator.
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }

    /// Is this a terminal line (client may stop reading)? `Listening`/`Partial`
    /// are STREAMING (non-terminal); `Transcript` ends a recognition session.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Response::Pong
                | Response::Status { .. }
                | Response::Done
                | Response::Transcript { .. }
                | Response::Diarization { .. }
                | Response::Enrolled { .. }
                | Response::Speakers { .. }
                | Response::ModelStatus { .. }
                | Response::Error { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_json_lines() {
        let cases = [
            Request::Ping,
            Request::Status,
            Request::Diarize { seconds: 10 },
            Request::Enroll {
                name: "Alex".into(),
                seconds: 15,
            },
            Request::ForgetSpeaker {
                name: "Alex".into(),
            },
            Request::ListSpeakers,
            Request::Speak {
                text: "hello".into(),
                voice: Some("af_sarah".into()),
                rate: Some(1.5),
                session: Some("sess-1".into()),
            },
            Request::SpeakNarration {
                text: "working on it".into(),
                session: None,
            },
            Request::StopSpeech { session: None },
            Request::StopSpeech {
                session: Some("sess-1".into()),
            },
            Request::TestRecognitionStart,
            Request::ModelStatus,
            Request::SetProvider {
                which: "coreml".into(),
            },
            Request::AuthorizeSystemStt,
            Request::Earcon {
                event: "reply_done".into(),
            },
            Request::Shutdown,
        ];
        for req in cases {
            let line = serde_json::to_string(&req).unwrap();
            assert!(!line.contains('\n'), "a request must be a single line");
            let back: Request = serde_json::from_str(&line).unwrap();
            // Re-serializing the parsed value must be byte-identical.
            assert_eq!(serde_json::to_string(&back).unwrap(), line);
        }
    }

    #[test]
    fn ping_uses_the_compact_tagged_form() {
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"cmd":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Pong).unwrap(),
            r#"{"ok":"pong"}"#
        );
    }

    #[test]
    fn terminal_classification() {
        assert!(Response::Pong.is_terminal());
        assert!(
            Response::Status {
                tts_active: false,
                queued: 0,
                paused: false,
                muted: false,
            }
            .is_terminal()
        );
        assert!(Response::Done.is_terminal());
        assert!(Response::error("x").is_terminal());
        assert!(
            Response::ModelStatus {
                status: serde_json::Value::Null
            }
            .is_terminal()
        );
        assert!(!Response::Listening.is_terminal());
        assert!(!Response::Partial { text: "x".into() }.is_terminal());
    }
}
