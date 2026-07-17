//! RPC wire protocol: one JSON [`Request`] per line in, one-or-more JSON [`Response`]
//! lines out. Streaming (STT test-recognition) emits non-terminal lines then a terminal.
//!
//! Config rides as `serde_json::Value` from `ds_config::voice_to_value` /
//! `voice_from_value` — no parallel `VoiceConfig` mirror.

use ds_client::ClientSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client → engine request (`#[serde(tag = "cmd")]`, snake_case).
///
/// ## `source` field
///
/// Seven **client-originated** variants require `source: ClientSource` (hook `--client` or
/// MCP `initialize` `clientInfo`): [`Self::GreetSession`], [`Self::MarkActive`], [`Self::SessionEnd`],
/// [`Self::StopSpeech`], [`Self::Speak`], [`Self::SpeakNarration`], [`Self::Earcon`]. No `#[serde(default)]` /
/// `Option` — absent field is a hard decode error (CLI/engine/wiring ship together; stale
/// hooks are rejected, not mis-attributed). Unrecognised *token* → `ClientSource::Unknown`
/// (forward-open, like [`Response::Unknown`]). Guard:
/// `request_without_source_is_a_hard_decode_error`.
///
/// All other variants deliberately omit `source` (app tray / engine self-talk / STT tools);
/// that keeps `ds-core` FFI constructors unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// → [`Response::Pong`].
    Ping,
    /// TTS queue snapshot → [`Response::Status`].
    Status,
    /// Ensure Kokoro frontend assets (voices, OOV G2P, ORT); returns immediately (download is
    /// single-flight background). → [`Response::Done`].
    EnsureKokoroFrontend,
    /// Ready the Codex app-server observation path for `dontspeak codex`. Returns only after
    /// the narration subscriber is attached (closes TUI-starts-before-narration race).
    /// → [`Response::CodexStreamReady`] / [`Response::Error`].
    EnsureCodexStream,
    /// Global mute (tray / Caps). Speech drains silently; cues suppressed. → [`Response::Done`].
    SetMuted { on: bool },
    /// SessionStart: optional pool-voice greeting when `greet_on_open`. → [`Response::Done`].
    GreetSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Required client origin — see enum-level `source` docs.
        source: ClientSource,
    },
    /// UserPromptSubmit: mark active terminal; other sessions' TTS is held (not dropped).
    MarkActive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Harness-injected continuation (issue #11), not a human submit — liveness only:
        /// must NOT claim active-terminal or apply `input_clears`. Classifier:
        /// `dontspeak::hook_speak::is_synthetic_continuation`.
        /// `#[serde(default)]` = wire compactness (omit when false), not legacy-hook compat
        /// (stale hooks fail on missing `source` first).
        #[serde(default)]
        synthetic: bool,
        source: ClientSource,
    },
    /// MCP `speak` tool: Reply on the TTS queue (survives record-barge when resume policy set).
    /// Narration uses [`Self::SpeakNarration`]. `voice`/`rate` override config.
    Speak {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<f32>,
        /// Ambient session for per-session playback routing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// Mid-turn narration (dropped first on barge/skip-ahead); sentence-split on warm child.
    SpeakNarration {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Producer id for admission dedup; `None` for older hooks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        narration_id: Option<String>,
        /// Required even though the engine does not log this variant (blockquote spam).
        source: ClientSource,
    },
    /// Barge-in. `Some(session)` scopes drop/cancel to that window; `None` = global hard barge.
    StopSpeech {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// SessionEnd: per-window [`Self::StopSpeech`] plus reclaim pool-voice map entry.
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// Stream Listening/partials, then terminal [`Response::Transcript`].
    TestRecognitionStart,
    /// Stop test-recognition on a *second* connection (first is busy streaming).
    TestRecognitionStop,
    /// One-shot diarization (not streamed) → [`Response::Diarization`].
    Diarize { seconds: u64 },
    /// Enroll WeSpeaker voiceprint → [`Response::Enrolled`].
    Enroll { name: String, seconds: u64 },
    /// → [`Response::Done`] (no-op if absent).
    ForgetSpeaker { name: String },
    /// → [`Response::Speakers`].
    ListSpeakers,
    /// Model presence/removability. Engine is authority (`removable` only if present and not
    /// loaded). File IO stays in the app. → [`Response::ModelStatus`].
    ModelStatus,
    /// Block until status `seq` differs from `since` or `timeout_ms` (push for dictation
    /// overlay). `since = 0` replies immediately.
    WaitModelStatus { since: u64, timeout_ms: u64 },
    /// Session-local TTS provider (`cpu`/`cuda`/`coreml`/`ane`/`auto`); restarts warm Kokoro,
    /// resets TTS stats. Not persisted. → [`Response::Done`].
    SetProvider { provider: String },
    /// IPC exit path; real quit is FFI `ds_engine_stop`. Kept for out-of-process stop.
    Shutdown,
    /// Explicit config reload (same as mtime poll, debounced with it). → [`Response::Done`].
    Reload,
    /// Earcon: `"reply_done"` (Stop) or `"needs_input"` (Notification). Unknown/disabled ⇒ no-op.
    Earcon {
        event: String,
        /// Session whose ordered speech this cue terminates. Optional for older hooks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// macOS System STT TCC + capability check before persisting `stt_engine=system`.
    /// → [`Response::Done`] / [`Response::Error`].
    AuthorizeSystemStt,
}

/// Engine → client response (`#[serde(tag = "ok")]`). Streaming: `Listening`/`Partial` then
/// terminal `Transcript`/`Error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Response {
    Pong,
    /// TTS queue snapshot. `paused` = record-barge hold; `muted` = silent play-through.
    Status {
        tts_active: bool,
        queued: usize,
        paused: bool,
        muted: bool,
    },
    Done,
    /// Codex TUI `--remote` endpoint after narration subscriber is attached.
    CodexStreamReady {
        endpoint: String,
    },
    /// Non-terminal: mic open.
    Listening,
    /// Non-terminal: live partial.
    Partial {
        text: String,
    },
    /// Terminal: final transcript.
    Transcript {
        text: String,
    },
    /// Terminal: `[{"speaker","start","end","name"?}, ...]` (seconds).
    Diarization {
        segments: Value,
    },
    Enrolled {
        name: String,
    },
    Speakers {
        names: Vec<String>,
    },
    /// Terminal model presence / removability / running map JSON.
    ModelStatus {
        status: Value,
    },
    Error {
        message: String,
    },
    /// Version-skew fallback: unknown `ok` tag. TERMINAL so `Client::recv` can clean up instead
    /// of hard-erroring mid-stream (e.g. never sending `TestRecognitionStop`). Decode-only
    /// (`#[serde(other)]`); never encoded by this crate.
    #[serde(other)]
    Unknown,
}

impl Response {
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }

    /// Terminal line (client may stop reading)? `Listening`/`Partial` are non-terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Response::Pong
                | Response::Status { .. }
                | Response::Done
                | Response::CodexStreamReady { .. }
                | Response::Transcript { .. }
                | Response::Diarization { .. }
                | Response::Enrolled { .. }
                | Response::Speakers { .. }
                | Response::ModelStatus { .. }
                | Response::Error { .. }
                | Response::Unknown
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
            Request::EnsureKokoroFrontend,
            Request::EnsureCodexStream,
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
                source: ClientSource::ClaudeCode,
            },
            Request::SpeakNarration {
                text: "working on it".into(),
                session: None,
                narration_id: Some("narration-1".into()),
                source: ClientSource::QwenCode,
            },
            Request::StopSpeech {
                session: None,
                source: ClientSource::Unknown,
            },
            Request::StopSpeech {
                session: Some("sess-1".into()),
                source: ClientSource::ClaudeCode,
            },
            Request::MarkActive {
                session: Some("sess-1".into()),
                synthetic: false,
                source: ClientSource::Codex,
            },
            Request::MarkActive {
                session: None,
                synthetic: true,
                source: ClientSource::ClaudeCode,
            },
            Request::GreetSession {
                session: Some("sess-1".into()),
                source: ClientSource::Grok,
            },
            Request::SessionEnd {
                session: Some("sess-1".into()),
                source: ClientSource::QwenCode,
            },
            Request::TestRecognitionStart,
            Request::ModelStatus,
            Request::SetProvider {
                provider: "coreml".into(),
            },
            Request::AuthorizeSystemStt,
            Request::Earcon {
                event: "reply_done".into(),
                session: Some("sess-1".into()),
                source: ClientSource::Grok,
            },
            Request::Shutdown,
        ];
        for req in cases {
            let line = serde_json::to_string(&req).unwrap();
            assert!(!line.contains('\n'), "a request must be a single line");
            let back: Request = serde_json::from_str(&line).unwrap();
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
    fn kokoro_frontend_request_uses_its_canonical_wire_name() {
        assert_eq!(
            serde_json::to_string(&Request::EnsureKokoroFrontend).unwrap(),
            r#"{"cmd":"ensure_kokoro_frontend"}"#
        );
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"ensure_kokoro_voices"}"#).is_err());
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

    /// Absent `synthetic` → false (issue #11; never assume continuation). See field docs.
    #[test]
    fn mark_active_synthetic_defaults_to_false_when_absent_on_the_wire() {
        let line = r#"{"cmd":"mark_active","session":"sess-1","source":"claude_code"}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(
            req,
            Request::MarkActive {
                session: Some(ref s),
                synthetic: false,
                source: ClientSource::ClaudeCode,
            } if s == "sess-1"
        ));
    }

    /// Guard: absent `source` is a hard decode error (not silent default). Unrecognised
    /// *token* fails open (`unknown_client_token_decodes_to_unknown`). Rejection is socket
    /// `bad request` + activity-log WARN via `on_bad_request` — hooks discard the reply.
    #[test]
    fn request_without_source_is_a_hard_decode_error() {
        let cases = [
            r#"{"cmd":"greet_session"}"#,
            r#"{"cmd":"mark_active","session":"sess-1"}"#,
            r#"{"cmd":"session_end","session":"sess-1"}"#,
            r#"{"cmd":"stop_speech"}"#,
            r#"{"cmd":"speak","text":"hi"}"#,
            r#"{"cmd":"speak_narration","text":"hi"}"#,
            r#"{"cmd":"earcon","event":"reply_done"}"#,
        ];
        for line in cases {
            let err = serde_json::from_str::<Request>(line)
                .expect_err("a client-originated request with no `source` must NOT decode");
            assert!(
                err.to_string().contains("source"),
                "the error must name the missing field for {line}, got: {err}"
            );
        }
    }

    /// Unrecognised `source` token → `Unknown` (forward-open); contrast absent-field fail-closed.
    #[test]
    fn unknown_client_token_decodes_to_unknown() {
        let req: Request = serde_json::from_str(r#"{"cmd":"greet_session","source":"gemini_cli"}"#)
            .expect("an unrecognised client token must decode to Unknown, not error out");
        assert!(matches!(
            req,
            Request::GreetSession {
                source: ClientSource::Unknown,
                ..
            }
        ));
        let req: Request =
            serde_json::from_str(r#"{"cmd":"earcon","event":"reply_done","source":"codex"}"#)
                .unwrap();
        assert!(matches!(
            req,
            Request::Earcon {
                source: ClientSource::Codex,
                ..
            }
        ));
    }

    #[test]
    fn earcon_session_roundtrips_and_old_lines_default_to_global() {
        let with_session = Request::Earcon {
            event: "needs_input".into(),
            session: Some("sess-1".into()),
            source: ClientSource::ClaudeCode,
        };
        let line = serde_json::to_string(&with_session).unwrap();
        assert!(line.contains(r#""session":"sess-1""#));
        assert!(matches!(
            serde_json::from_str::<Request>(&line).unwrap(),
            Request::Earcon {
                session: Some(ref session),
                ..
            } if session == "sess-1"
        ));

        let old: Request =
            serde_json::from_str(r#"{"cmd":"earcon","event":"reply_done","source":"codex"}"#)
                .unwrap();
        assert!(matches!(old, Request::Earcon { session: None, .. }));
    }

    /// Unknown `ok` tag → terminal `Response::Unknown` (see variant docs).
    #[test]
    fn unrecognized_response_tag_falls_back_to_unknown_instead_of_erroring() {
        let future_line = r#"{"ok":"some_future_variant","extra":"field","n":42}"#;
        let resp: Response = serde_json::from_str(future_line)
            .expect("an unrecognized `ok` tag must decode to Response::Unknown, not error out");
        assert!(matches!(resp, Response::Unknown));
        assert!(
            resp.is_terminal(),
            "Unknown must be terminal so callers don't spin forever"
        );
    }
}
