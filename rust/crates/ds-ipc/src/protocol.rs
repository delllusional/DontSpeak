//! Wire: one JSON [`Request`] per line in; one-or-more JSON [`Response`] lines out.
//! Streaming (test-recognition) ends on a terminal line.
//!
//! Config as `serde_json::Value` (`ds_config` voice_to/from_value) — no VoiceConfig mirror.

use ds_client::WiredAgent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn deserialize_nonempty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("must not be empty"));
    }
    Ok(value)
}

fn deserialize_client_source<'de, D>(deserializer: D) -> Result<Option<WiredAgent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<WiredAgent>::deserialize(deserializer)
}

/// Client → engine (`#[serde(tag = "cmd")]`, snake_case).
///
/// ## `source` field
///
/// Required on: GreetSession, MarkActive, SessionEnd, Stop, Speak, SpeakNarration, Earcon.
/// Absent field = hard decode error (stale hooks rejected). `null` means the MCP peer is not
/// a wired client; unrecognised non-null tokens fail closed.
/// Tray / engine self-talk / STT tools omit `source` (FFI unchanged).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Codex app-server ready after narration subscriber attaches (TUI race).
    EnsureCodexStream,
    /// Global mute; speech drains silently, cues suppressed.
    SetMuted {
        on: bool,
    },
    /// SessionStart greeting when `greet`.
    GreetSession {
        /// Upstream logical session used for stream discovery/state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Terminal/window identity used only by the TTS queue.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_session: Option<String>,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// UserPromptSubmit: mark active terminal; other sessions held.
    MarkActive {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_session: Option<String>,
        /// Issue #11 harness continuation — liveness only (no active claim / clear_on_input).
        /// Classifier: `dontspeak::hook_speak::is_synthetic_continuation`.
        #[serde(default)]
        synthetic: bool,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// MCP speak (reply; survives record-barge when resume policy set).
    Speak {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tts_args: Option<Value>,
        /// Required: MCP speech is always scoped to its stdio/window identity.
        #[serde(deserialize_with = "deserialize_nonempty_string")]
        session: String,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// Mid-turn narration (barge/skip drops first); sentence-split on warm child.
    SpeakNarration {
        text: String,
        /// Reconstructed turn text so far, capped by the engine. Backs this chunk's
        /// language only when the chunk itself is too short to classify; absent/empty →
        /// the chunk stands on its own text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detection_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Admission dedup id; `None` for older hooks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        narration_id: Option<String>,
        /// Required for uniform source contract (engine skips logging this variant).
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// MCP barge-in, scoped to its stdio/window identity.
    Stop {
        #[serde(deserialize_with = "deserialize_nonempty_string")]
        session: String,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// Per-window stop; agent voice assignment survives.
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_session: Option<String>,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// Stream Listening/partials → terminal Transcript.
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
    /// Coding-agent quota deck. Runs in the host app so macOS keychain ACL grants apply.
    AgentUsage {
        refresh: bool,
    },
    /// Block until `seq` ≠ `since` or timeout. `since = 0` immediate.
    WaitModelStatus {
        since: u64,
        timeout_ms: u64,
    },
    /// Session-local TTS provider; restarts Kokoro, resets stats. Not persisted.
    SetProvider {
        provider: String,
    },
    /// Same as mtime poll (shared debounce).
    Reload,
    /// Earcon (Stop / Notification). Disabled cue ⇒ no-op.
    Earcon {
        event: ds_earcon::EarconEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(deserialize_with = "deserialize_client_source")]
        source: Option<WiredAgent>,
    },
    /// macOS System STT TCC + capability before persisting `stt_engine=system`.
    AuthorizeSystemStt,
}

/// Engine → client (`#[serde(tag = "ok")]`). Streaming: Listening/Partial then terminal.
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
    /// `[{"speaker","start","end","name"?}, …]` (seconds).
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
    AgentUsage {
        deck: Value,
    },
    Error {
        message: String,
    },
    /// Unknown `ok` tag. Terminal so `Client::recv` cleans up mid-stream. Decode-only
    /// (`#[serde(other)]`); this crate does not encode it.
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
                | Response::AgentUsage { .. }
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
                tts_args: Some(serde_json::json!({
                    "kokoro": { "voice": "af_sarah", "language": "en", "rate": 1.5 }
                })),
                session: "sess-1".into(),
                source: Some(WiredAgent::ClaudeCode),
            },
            Request::SpeakNarration {
                text: "working on it".into(),
                detection_text: Some("full turn so far for language".into()),
                session: None,
                narration_id: Some("narration-1".into()),
                source: Some(WiredAgent::QwenCode),
            },
            Request::SpeakNarration {
                text: "legacy digest only".into(),
                detection_text: None,
                session: Some("sess-1".into()),
                narration_id: None,
                source: Some(WiredAgent::ClaudeCode),
            },
            Request::Stop {
                session: "sess-1".into(),
                source: Some(WiredAgent::ClaudeCode),
            },
            Request::MarkActive {
                session: Some("sess-1".into()),
                queue_session: Some("window-1".into()),
                synthetic: false,
                source: Some(WiredAgent::Codex),
            },
            Request::MarkActive {
                session: None,
                queue_session: None,
                synthetic: true,
                source: Some(WiredAgent::ClaudeCode),
            },
            Request::GreetSession {
                session: Some("sess-1".into()),
                queue_session: Some("window-1".into()),
                source: Some(WiredAgent::Grok),
            },
            Request::SessionEnd {
                session: Some("sess-1".into()),
                queue_session: Some("window-1".into()),
                source: Some(WiredAgent::QwenCode),
            },
            Request::TestRecognitionStart,
            Request::ModelStatus,
            Request::AgentUsage { refresh: true },
            Request::SetProvider {
                provider: "mlx".into(),
            },
            Request::AuthorizeSystemStt,
            Request::Earcon {
                event: ds_earcon::EarconEvent::ReplyDone,
                session: Some("sess-1".into()),
                source: Some(WiredAgent::Grok),
            },
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
    fn unused_engine_only_commands_are_not_part_of_the_wire_contract() {
        for line in [
            r#"{"cmd":"ensure_kokoro_frontend"}"#,
            r#"{"cmd":"shutdown"}"#,
        ] {
            assert!(serde_json::from_str::<Request>(line).is_err(), "{line}");
        }
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
        assert!(
            Response::AgentUsage {
                deck: serde_json::json!({ "cards": [] })
            }
            .is_terminal()
        );
        assert!(!Response::Listening.is_terminal());
        assert!(!Response::Partial { text: "x".into() }.is_terminal());
    }

    /// Issue #11: absent `synthetic` → false.
    #[test]
    fn mark_active_synthetic_defaults_to_false_when_absent_on_the_wire() {
        let line = r#"{"cmd":"mark_active","session":"sess-1","source":"claude"}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert!(matches!(
            req,
            Request::MarkActive {
                session: Some(ref s),
                synthetic: false,
                source: Some(WiredAgent::ClaudeCode),
                ..
            } if s == "sess-1"
        ));
    }

    /// Absent `source` remains a hard decode error even though `null` is a valid value.
    #[test]
    fn request_without_source_is_a_hard_decode_error() {
        let cases = [
            r#"{"cmd":"greet_session"}"#,
            r#"{"cmd":"mark_active","session":"sess-1"}"#,
            r#"{"cmd":"session_end","session":"sess-1"}"#,
            r#"{"cmd":"stop","session":"sess-1"}"#,
            r#"{"cmd":"speak","text":"hi","session":"sess-1"}"#,
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
    fn mcp_speak_and_stop_require_a_non_optional_session() {
        for line in [
            r#"{"cmd":"speak","text":"hi","source":"claude"}"#,
            r#"{"cmd":"stop","source":"claude"}"#,
        ] {
            let err = serde_json::from_str::<Request>(line)
                .expect_err("MCP queue operations must never decode without a scope");
            assert!(
                err.to_string().contains("session"),
                "missing-session error must name the field for {line}: {err}"
            );
        }
    }

    #[test]
    fn mcp_speak_and_stop_reject_an_empty_session() {
        for line in [
            r#"{"cmd":"speak","text":"hi","session":"  ","source":"claude"}"#,
            r#"{"cmd":"stop","session":"","source":"claude"}"#,
        ] {
            let err = serde_json::from_str::<Request>(line)
                .expect_err("MCP queue operations must never decode with an empty scope");
            assert!(
                err.to_string().contains("must not be empty"),
                "{line}: {err}"
            );
        }
    }

    #[test]
    fn nullable_source_accepts_unwired_mcp_and_rejects_unknown_tokens() {
        let req: Request =
            serde_json::from_str(r#"{"cmd":"greet_session","source":null}"#).unwrap();
        assert!(matches!(req, Request::GreetSession { source: None, .. }));
        assert!(
            serde_json::from_str::<Request>(r#"{"cmd":"greet_session","source":"gemini_cli"}"#)
                .is_err()
        );
        let line = format!(
            r#"{{"cmd":"earcon","event":"reply_done","source":"{}"}}"#,
            WiredAgent::Codex.as_str()
        );
        let req: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            req,
            Request::Earcon {
                source: Some(WiredAgent::Codex),
                ..
            }
        ));
    }

    #[test]
    fn earcon_session_roundtrips_and_old_lines_default_to_global() {
        let with_session = Request::Earcon {
            event: ds_earcon::EarconEvent::NeedsInput,
            session: Some("sess-1".into()),
            source: Some(WiredAgent::ClaudeCode),
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

        let line = format!(
            r#"{{"cmd":"earcon","event":"reply_done","source":"{}"}}"#,
            WiredAgent::Codex.as_str()
        );
        let old: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(old, Request::Earcon { session: None, .. }));
    }

    #[test]
    fn speak_narration_optional_detection_fields_roundtrip_and_legacy_decodes() {
        let with_fields = Request::SpeakNarration {
            text: "digest".into(),
            detection_text: Some("full so-far corpus".into()),
            session: Some("sess-1".into()),
            narration_id: Some("n1".into()),
            source: Some(WiredAgent::Codex),
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

        // Absent field (older CLI) → None; engine detects on spoken text alone. A stale
        // CLI still sending the retired `message_key` decodes the same way (serde ignores
        // unknown fields), so hooks and engine may update out of step.
        let legacy =
            r#"{"cmd":"speak_narration","text":"hi","message_key":"m1","source":"claude"}"#;
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
            source: Some(WiredAgent::ClaudeCode),
        };
        let bare_line = serde_json::to_string(&bare).unwrap();
        assert!(!bare_line.contains("detection_text"));
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
