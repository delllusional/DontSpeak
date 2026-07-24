//! Engine-ping side effects for `notify` (from [`crate::hook_core`]). Best-effort IPC;
//! neither blocks the client nor synthesizes (engine owns playback).
//!
//! Greet — SessionStart → `GreetSession` (engine self-gates on `greet`).
//! MarkActive — UserPromptSubmit → TTS follows this terminal, EXCEPT
//! [`is_synthetic_continuation`] (harness-injected, e.g. `<task-notification>`) — then only
//! liveness bookkeeping; skip claim-active / cancel-on-submit (issue #11).
//!
//! Spoken replies not here: MessageDisplay → `hook_narrate`; Stop → `hook_core`.

use ds_config::{Paths, WiredAgent};
use serde::Deserialize;

pub enum Ping {
    Greet,
    MarkActive,
}

/// Prefix markers (after leading whitespace) for harness continuations — never substring,
/// so a human prompt that merely mentions the tag mid-text isn't misclassified.
///
/// Only `<task-notification>` confirmed today (issue #11). Add entries only when
/// observed or documented (Anthropic doesn't publish other wrapper text).
const SYNTHETIC_PROMPT_MARKERS: &[&str] = &["<task-notification>"];

fn is_synthetic_continuation(prompt: &str) -> bool {
    let trimmed = prompt.trim_start();
    SYNTHETIC_PROMPT_MARKERS
        .iter()
        .any(|m| trimmed.starts_with(m))
}

/// UserPromptSubmit `prompt` (fail-open: absent/malformed → `""`).
#[derive(Deserialize, Default)]
struct PromptEnvelope {
    #[serde(default)]
    prompt: String,
}

fn prompt_from_payload(payload: &str) -> String {
    serde_json::from_str::<PromptEnvelope>(payload.trim())
        .map(|e| e.prompt)
        .unwrap_or_default()
}

/// Split from [`engine_ping`] so shape is unit-testable.
fn mark_active_request(payload: &str, client: WiredAgent) -> ds_ipc::Request {
    ds_ipc::Request::MarkActive {
        session: crate::hook_core::session_id_from_payload(payload),
        queue_session: crate::session_scope::for_hook(payload),
        synthetic: is_synthetic_continuation(&prompt_from_payload(payload)),
        source: Some(client),
    }
}

/// Best-effort ping from already-read payload. Engine down ⇒ no-op.
pub fn engine_ping(paths: &Paths, ping: Ping, payload: &str, client: WiredAgent) {
    let req = match ping {
        Ping::Greet => ds_ipc::Request::GreetSession {
            session: crate::hook_core::session_id_from_payload(payload),
            queue_session: crate::session_scope::for_hook(payload),
            source: Some(client),
        },
        Ping::MarkActive => mark_active_request(payload, client),
    };
    if let Ok(mut c) = ds_ipc::connect(&paths.engine_sock)
        && c.send(&req).is_ok()
    {
        let _ = c.recv_terminal();
    }
}

fn earcon_request(
    payload: &str,
    event: ds_earcon::EarconEvent,
    client: WiredAgent,
) -> ds_ipc::Request {
    earcon_request_with(payload, event, client, |name| std::env::var(name).ok())
}

fn earcon_request_with(
    payload: &str,
    event: ds_earcon::EarconEvent,
    client: WiredAgent,
    get: impl Fn(&str) -> Option<String>,
) -> ds_ipc::Request {
    ds_ipc::Request::Earcon {
        event,
        session: crate::session_scope::for_hook_with(payload, get),
        source: Some(client),
    }
}

/// Enqueue earcon behind this session's speech. Engine down ⇒ no-op.
pub fn engine_earcon(
    paths: &Paths,
    event: ds_earcon::EarconEvent,
    payload: &str,
    client: WiredAgent,
) {
    let _ = ds_ipc::request(&paths.engine_sock, &earcon_request(payload, event, client));
}

/// Explicit session (Grok Stop sticky `grok-stop:<session>` so reply_done queues behind digests;
/// MarkActive current-clear cannot prune the tag).
pub fn engine_earcon_for_session(
    paths: &Paths,
    event: ds_earcon::EarconEvent,
    session: Option<String>,
    client: WiredAgent,
) {
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::Earcon {
            event,
            session,
            source: Some(client),
        },
    );
}

#[derive(Debug, Deserialize, Default)]
struct NotificationHook {
    #[serde(default, alias = "notificationType")]
    notification_type: String,
}

/// Only `permission_prompt` / `idle_prompt` warrant needs-input (cue stays meaningful).
fn wants_needs_input_earcon(payload: &str) -> bool {
    let kind = serde_json::from_str::<NotificationHook>(payload.trim())
        .map(|h| h.notification_type)
        .unwrap_or_default();
    matches!(kind.as_str(), "permission_prompt" | "idle_prompt")
}

/// Needs-input earcon only for "waiting on you" notification types.
pub fn notification_earcon(paths: &Paths, payload: &str, client: WiredAgent) {
    if wants_needs_input_earcon(payload) {
        engine_earcon(paths, ds_earcon::EarconEvent::NeedsInput, payload, client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_input_earcon_is_gated_on_notification_type() {
        for payload in [
            r#"{"notification_type":"permission_prompt"}"#,
            r#"{"notification_type":"idle_prompt"}"#,
        ] {
            assert!(wants_needs_input_earcon(payload), "{payload}");
        }
        // Unknown types, missing field, malformed JSON, empty — fail closed, no panic.
        for payload in [
            r#"{"notification_type":"auth_success"}"#,
            r#"{"notification_type":"mcp_elicitation"}"#,
            r#"{}"#,
            r#"{"other_key":"value"}"#,
            "not json at all",
            "{unterminated",
            "",
        ] {
            assert!(!wants_needs_input_earcon(payload), "{payload:?}");
        }
    }

    /// No socket: connect fails fast; no panic / hang.
    #[test]
    fn engine_ping_with_no_socket_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let c = WiredAgent::ClaudeCode;
        engine_ping(&paths, Ping::Greet, r#"{"session_id":"s1"}"#, c);
        engine_ping(&paths, Ping::MarkActive, r#"{"session_id":"s1"}"#, c);
    }

    #[test]
    fn engine_earcon_with_no_socket_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        engine_earcon(
            &paths,
            ds_earcon::EarconEvent::NeedsInput,
            r#"{"sessionId":"grok-session"}"#,
            WiredAgent::Grok,
        );
    }

    #[test]
    fn earcon_request_derives_session_from_each_hook_dialect() {
        for (payload, expected) in [
            (r#"{"session_id":"claude-session"}"#, "claude-session"),
            (r#"{"sessionId":"grok-session"}"#, "grok-session"),
        ] {
            assert!(matches!(
                earcon_request_with(
                    payload,
                    ds_earcon::EarconEvent::ReplyDone,
                    WiredAgent::ClaudeCode,
                    |_| None
                ),
                ds_ipc::Request::Earcon {
                    session: Some(ref session),
                    ..
                } if session == expected
            ));
        }
    }

    #[test]
    fn task_notification_prefix_is_classified_synthetic() {
        assert!(is_synthetic_continuation(
            "<task-notification>\nBackground task \"watch tests\" finished.\n</task-notification>"
        ));
    }

    #[test]
    fn leading_whitespace_before_marker_is_still_detected() {
        assert!(is_synthetic_continuation(
            "\n  <task-notification>\nBackground task finished.\n</task-notification>"
        ));
    }

    #[test]
    fn human_prompt_mentioning_the_tag_is_not_misclassified() {
        assert!(!is_synthetic_continuation(
            "why did narration cut off — I see <task-notification> in the transcript?"
        ));
    }

    #[test]
    fn ordinary_empty_and_malformed_prompts_are_not_synthetic() {
        assert!(!is_synthetic_continuation("fix the bug in foo.rs"));
        assert!(!is_synthetic_continuation(""));
        for payload in [r#"{"session_id":"s1"}"#, "not json at all", "{unterminated"] {
            assert_eq!(prompt_from_payload(payload), "");
            assert!(!is_synthetic_continuation(&prompt_from_payload(payload)));
        }
    }

    #[test]
    fn mark_active_request_carries_synthetic_flag_from_payload_shape() {
        let synthetic_payload =
            r#"{"session_id":"s1","prompt":"<task-notification>\nfoo\n</task-notification>"}"#;
        match mark_active_request(synthetic_payload, WiredAgent::ClaudeCode) {
            ds_ipc::Request::MarkActive {
                session,
                queue_session,
                synthetic,
                source,
            } => {
                assert_eq!(session, Some("s1".to_string()));
                assert_eq!(
                    queue_session,
                    crate::session_scope::for_hook(synthetic_payload)
                );
                assert!(synthetic);
                assert_eq!(source, Some(WiredAgent::ClaudeCode));
            }
            other => panic!("expected MarkActive, got {other:?}"),
        }

        let ordinary_payload = r#"{"session_id":"s2","prompt":"fix the bug in foo.rs"}"#;
        match mark_active_request(ordinary_payload, WiredAgent::Codex) {
            ds_ipc::Request::MarkActive {
                session,
                queue_session,
                synthetic,
                source,
            } => {
                assert_eq!(session, Some("s2".to_string()));
                assert_eq!(
                    queue_session,
                    crate::session_scope::for_hook(ordinary_payload)
                );
                assert!(!synthetic);
                assert_eq!(
                    source,
                    Some(WiredAgent::Codex),
                    "the request carries the CLIENT that invoked the hook, verbatim"
                );
            }
            other => panic!("expected MarkActive, got {other:?}"),
        }
    }
}
