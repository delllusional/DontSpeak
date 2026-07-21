//! Wire: one JSON [`Request`] per line in; one-or-more JSON [`Response`] lines out.
//! Streaming (test-recognition) ends on a terminal line.
//!
//! Config as `serde_json::Value` (`ds_config` voice_to/from_value) ΓÇö no VoiceConfig mirror.

use ds_client::ClientSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client ΓåÆ engine (`#[serde(tag = "cmd")]`, snake_case).
///
/// ## `source` field
///
/// Required on: GreetSession, MarkActive, SessionEnd, Stop, Speak, SpeakNarration, Earcon.
/// Absent field = hard decode error (stale hooks rejected). Unrecognised token ΓåÆ
/// `ClientSource::Unknown`. Guard: `request_without_source_is_a_hard_decode_error`.
/// Tray / engine self-talk / STT tools omit `source` (FFI unchanged).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Kokoro frontend assets; returns immediately (single-flight download).
    EnsureKokoroFrontend,
    /// Codex app-server ready after narration subscriber attaches (TUI race).
    EnsureCodexStream,
    /// Global mute; speech drains silently, cues suppressed.
    SetMuted {
        on: bool,
    },
    /// SessionStart greeting when `greet`.
    GreetSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// UserPromptSubmit: mark active terminal; other sessions held.
    MarkActive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Issue #11 harness continuation ΓÇö liveness only (no active claim / clear_on_input).
        /// Classifier: `dontspeak::hook_speak::is_synthetic_continuation`.
        #[serde(default)]
        synthetic: bool,
        source: ClientSource,
    },
    /// MCP speak (reply; survives record-barge when resume policy set). Overrides optional.
    Speak {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rate: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// Mid-turn narration (barge/skip drops first); sentence-split on warm child.
    SpeakNarration {
        text: String,
        /// Reconstructed turn text so far, capped by the engine. Backs this chunk's
        /// language only when the chunk itself is too short to classify; absent/empty ΓåÆ
        /// the chunk stands on its own text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detection_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Admission dedup id; `None` for older hooks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        narration_id: Option<String>,
        /// Required for uniform source contract (engine skips logging this variant).
        source: ClientSource,
    },
    /// Barge-in. `Some(session)` scopes; `None` = global.
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// Per-window stop; agent voice assignment survives.
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// Stream Listening/partials ΓåÆ terminal Transcript.
    TestRecognitionStart,
    /// Stop via second connection (first is streaming).
    TestRecognitionStop,
    /// One-shot diarization.
    Diarize {
        seconds: u64,
    },
    Enroll {
        name: String,
        seconds: u64,
    },
    ForgetSpeaker {
        name: String,
    },
    ListSpeakers,
    /// Presence/removability; engine is authority. File IO in app.
    ModelStatus,
    /// Block until `seq` Γëá `since` or timeout. `since = 0` immediate.
    WaitModelStatus {
        since: u64,
        timeout_ms: u64,
    },
    /// Session-local TTS provider; restarts Kokoro, resets stats. Not persisted.
    SetProvider {
        provider: String,
    },
    /// IPC exit; real quit is FFI `ds_engine_stop`.
    Shutdown,
    /// Same as mtime poll (shared debounce).
    Reload,
    /// Earcon (Stop / Notification). Disabled cue ΓçÆ no-op.
    Earcon {
        event: ds_earcon::EarconEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        source: ClientSource,
    },
    /// macOS System STT TCC + capability before persisting `stt_engine=system`.
    AuthorizeSystemStt,
    /// Take over this conn as `app`'s native-frontend subscription (Zed-style).
    /// Streams [`Response::FrontendEvent`]; client replies with [`Request::AckDeliver`].
    /// ≤1 live sub per tag (resubscribe evicts). Error if `frontend_enabled` is off.
    SubscribeFrontend { app: String },
    /// Panel-agent cumulative batch → same `ds_narrate::deliver_batch` pipeline as hooks.
    /// `key` is stable per message; `is_final` ends the turn. → [`Response::Done`].
    NarrateBatch {
        session: String,
        key: String,
        text: String,
        is_final: bool,
    },
    /// Ack a `deliver` event (`seq` match) on the subscribed conn. `ok:false` / timeout
    /// → paste fallback. Late acks after the deadline are ignored.
    AckDeliver { seq: u64, ok: bool },
}

/// Dictation lifecycle inside [`Response::FrontendEvent`] (`event` tag). Wire shapes
/// pinned by tests + Zed fixtures (docs/ZED-FRONTEND.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DictationEvent {
    /// Frontend-owned PTT started (mic open).
    RecordingStarted,
    /// Live partial (cumulative replace).
    Partial { text: String },
    /// Recording ended; await confirm.
    AwaitingConfirm { text: String },
    /// Insert `text` (submit when true). Must [`Request::AckDeliver`] this `seq`.
    Deliver { text: String, submit: bool },
    /// Nothing to deliver (cancel / empty / teardown) ΓÇö clear marked text.
    Cancelled,
    /// Start refused (e.g. models not ready).
    Refused,
    /// Unknown event tag from a newer daemon ΓÇö ignore, keep subscription.
    #[serde(other)]
    Unknown,
}


/// Engine ΓåÆ client (`#[serde(tag = "ok")]`). Streaming: Listening/Partial then terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Done,
    /// Codex TUI `--remote` after narration subscriber attached.
    CodexStreamReady {
        endpoint: String,
    },
    Listening,
    Partial {
        text: String,
    },
    Transcript {
        text: String,
    },
    /// `[{"speaker","start","end","name"?}, ΓÇª]` (seconds).
    Diarization {
        segments: Value,
    },
    Enrolled {
        name: String,
    },
    Speakers {
        names: Vec<String>,
    },
    ModelStatus {
        status: Value,
    },
    Error {
        message: String,
    },
    /// Dictation lifecycle on a subscription (non-terminal). seq for [Request::AckDeliver].
    FrontendEvent {
        #[serde(flatten)]
        event: DictationEvent,
        seq: u64,
    },
    /// Unknown ok tag. Terminal so Client::recv cleans up mid-stream. Decode-only
    /// (#[serde(other)]); this crate does not encode it.
    #[serde(other)]
    Unknown,
}

impl Response {
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }

    /// Client may stop reading. Listening/Partial are non-terminal.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Response::Pong
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
                detection_text: Some("full turn so far for language".into()),
                session: None,
                narration_id: Some("narration-1".into()),
                source: ClientSource::QwenCode,
            },
            Request::SpeakNarration {
                text: "legacy digest only".into(),
                detection_text: None,
                session: Some("sess-1".into()),
                narration_id: None,
                source: ClientSource::ClaudeCode,
            },
            Request::Stop {
                session: None,
                source: ClientSource::Unknown,
            },
            Request::Stop {
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
                provider: "mlx".into(),
            },
            Request::AuthorizeSystemStt,
            Request::Earcon {
                event: ds_earcon::EarconEvent::ReplyDone,
                session: Some("sess-1".into()),
                source: ClientSource::Grok,
            },
                        Request::SubscribeFrontend { app: "zed".into() },
            Request::AckDeliver { seq: 4, ok: true },
            Request::AckDeliver { seq: 5, ok: false },
            Request::NarrateBatch {
                session: "s1".into(),
                key: "k1".into(),
                text: "hello".into(),
                is_final: false,
            },
            Request::NarrateBatch {
                session: "s1".into(),
                key: "k1".into(),
                text: "hello world".into(),
                is_final: true,
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

    /// Issue #11: absent `synthetic` ΓåÆ false.
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

    /// Absent `source` = hard decode error; unrecognised token is open (`Unknown`).
    #[test]
    fn request_without_source_is_a_hard_decode_error() {
        let cases = [
            r#"{"cmd":"greet_session"}"#,
            r#"{"cmd":"mark_active","session":"sess-1"}"#,
            r#"{"cmd":"session_end","session":"sess-1"}"#,
            r#"{"cmd":"stop"}"#,
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
            event: ds_earcon::EarconEvent::NeedsInput,
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

    #[test]
    fn speak_narration_optional_detection_fields_roundtrip_and_legacy_decodes() {
        let with_fields = Request::SpeakNarration {
            text: "digest".into(),
            detection_text: Some("full so-far corpus".into()),
            session: Some("sess-1".into()),
            narration_id: Some("n1".into()),
            source: ClientSource::Codex,
        };
        let line = serde_json::to_string(&with_fields).unwrap();
        assert!(line.contains(r#""detection_text":"full so-far corpus""#));
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back,
            Request::SpeakNarration {
                ref text,
                detection_text: Some(ref det),
                ..
            } if text == "digest" && det == "full so-far corpus"
        ));

        // Absent field (older CLI) ΓåÆ None; engine detects on spoken text alone. A stale
        // CLI still sending the retired `message_key` decodes the same way (serde ignores
        // unknown fields), so hooks and engine may update out of step.
        let legacy =
            r#"{"cmd":"speak_narration","text":"hi","message_key":"m1","source":"claude_code"}"#;
        let old: Request = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            old,
            Request::SpeakNarration {
                detection_text: None,
                ..
            }
        ));

        // Serialize without the field omits it (skip_serializing_if).
        let bare = Request::SpeakNarration {
            text: "hi".into(),
            detection_text: None,
            session: None,
            narration_id: None,
            source: ClientSource::ClaudeCode,
        };
        let bare_line = serde_json::to_string(&bare).unwrap();
        assert!(!bare_line.contains("detection_text"));
    }

    /// Unknown `ok` tag ΓåÆ terminal `Response::Unknown` (see variant docs).
    /// Cross-repo wire contract: the Zed frontend mirrors these EXACT byte
    /// shapes as fixture tests (see docs/ZED-FRONTEND.md), so key names, key
    /// ORDER, and value spelling are all load-bearing here — a serialization
    /// change that reshuffles or renames anything must fail loudly.
    #[test]
    fn frontend_wire_shapes_match_the_documented_contract() {
        // Client → daemon, on the persistent subscription connection.
        assert_eq!(
            serde_json::to_string(&Request::SubscribeFrontend { app: "zed".into() }).unwrap(),
            r#"{"cmd":"subscribe_frontend","app":"zed"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::AckDeliver { seq: 4, ok: true }).unwrap(),
            r#"{"cmd":"ack_deliver","seq":4,"ok":true}"#
        );
        // Client → daemon, one-shot (the panel-agent narration bridge).
        assert_eq!(
            serde_json::to_string(&Request::NarrateBatch {
                session: "sess-1".into(),
                key: "sess-1#0#2".into(),
                text: "> Hi.".into(),
                is_final: false,
            })
            .unwrap(),
            r#"{"cmd":"narrate_batch","session":"sess-1","key":"sess-1#0#2","text":"> Hi.","is_final":false}"#
        );

        // Daemon → client, streamed on that connection.
        let cases: [(Response, &str); 6] = [
            (
                Response::FrontendEvent {
                    event: DictationEvent::RecordingStarted,
                    seq: 1,
                },
                r#"{"ok":"frontend_event","event":"recording_started","seq":1}"#,
            ),
            (
                Response::FrontendEvent {
                    event: DictationEvent::Partial {
                        text: "hello wor".into(),
                    },
                    seq: 2,
                },
                r#"{"ok":"frontend_event","event":"partial","text":"hello wor","seq":2}"#,
            ),
            (
                Response::FrontendEvent {
                    event: DictationEvent::AwaitingConfirm {
                        text: "hello world".into(),
                    },
                    seq: 3,
                },
                r#"{"ok":"frontend_event","event":"awaiting_confirm","text":"hello world","seq":3}"#,
            ),
            (
                Response::FrontendEvent {
                    event: DictationEvent::Deliver {
                        text: "hello world".into(),
                        submit: true,
                    },
                    seq: 4,
                },
                r#"{"ok":"frontend_event","event":"deliver","text":"hello world","submit":true,"seq":4}"#,
            ),
            (
                Response::FrontendEvent {
                    event: DictationEvent::Cancelled,
                    seq: 5,
                },
                r#"{"ok":"frontend_event","event":"cancelled","seq":5}"#,
            ),
            (
                Response::FrontendEvent {
                    event: DictationEvent::Refused,
                    seq: 6,
                },
                r#"{"ok":"frontend_event","event":"refused","seq":6}"#,
            ),
        ];
        for (resp, wire) in cases {
            assert_eq!(serde_json::to_string(&resp).unwrap(), wire);
            // And the decode side round-trips back to the same bytes.
            let back: Response = serde_json::from_str(wire).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), wire);
        }
    }

    #[test]
    fn unknown_frontend_event_does_not_break_the_subscription_decoder() {
        let response: Response = serde_json::from_str(
            r#"{"ok":"frontend_event","event":"future_event","text":"ignored","seq":9}"#,
        )
        .unwrap();
        assert!(matches!(
            response,
            Response::FrontendEvent {
                event: DictationEvent::Unknown,
                seq: 9
            }
        ));
    }

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
