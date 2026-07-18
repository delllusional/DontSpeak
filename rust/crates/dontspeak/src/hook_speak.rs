//! Engine-ping side effects for `notify` events (dispatched from [`crate::hook_core`]).
//! Best-effort IPC pings; neither blocks the client nor synthesizes here (engine owns playback).
//!
//! [`Ping`]:
//!   Greet       — SessionStart → `GreetSession` (engine self-gates on `greet_on_open`).
//!   MarkActive  — UserPromptSubmit → `MarkActive` so TTS follows this terminal. EXCEPT when
//!                 the prompt is [`is_synthetic_continuation`] (harness-injected, e.g. Claude
//!                 Code `<task-notification>` after a background task) — then skip
//!                 claim-active / cancel-on-submit side effects engine-side; only liveness
//!                 bookkeeping. See issue #11.
//!
//! Spoken replies are not here: streaming clients use MessageDisplay →
//! `hook_narrate::message_display`; non-streaming final replies use Stop in `hook_core`.

use ds_config::{ClientSource, Paths};
use serde::Deserialize;

/// Which best-effort engine ping a notify event maps to.
pub enum Ping {
    /// SessionStart → greet in this agent's assigned voice (engine self-gates).
    Greet,
    /// UserPromptSubmit → mark THIS terminal active so narration follows it.
    MarkActive,
}

/// Prefix markers (after leading whitespace) for harness-injected continuations — never
/// substring, so a human prompt that merely mentions the tag mid-text isn't misclassified.
///
/// Only `<task-notification>` confirmed today (issue #11). Other harness shapes exist
/// (Agent Teams, `/loop`) but Anthropic doesn't publish wrapper text — add entries only
/// when observed or documented.
const SYNTHETIC_PROMPT_MARKERS: &[&str] = &["<task-notification>"];

/// Pure: is this UserPromptSubmit `prompt` a harness continuation? See markers above.
fn is_synthetic_continuation(prompt: &str) -> bool {
    let trimmed = prompt.trim_start();
    SYNTHETIC_PROMPT_MARKERS
        .iter()
        .any(|m| trimmed.starts_with(m))
}

/// UserPromptSubmit `prompt` field (fail-open: absent/malformed → `""`).
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

/// Build `MarkActive` from payload — split from [`engine_ping`] so shape is unit-testable.
fn mark_active_request(payload: &str, client: ClientSource) -> ds_ipc::Request {
    ds_ipc::Request::MarkActive {
        session: crate::hook_core::session_id_from_payload(payload),
        synthetic: is_synthetic_continuation(&prompt_from_payload(payload)),
        source: client,
    }
}

/// One best-effort ping from hook `payload` (already read by notify — not re-read). Scopes
/// by ambient `session_id` and stamps `client`. Engine down ⇒ no-op.
pub fn engine_ping(paths: &Paths, ping: Ping, payload: &str, client: ClientSource) {
    let req = match ping {
        Ping::Greet => ds_ipc::Request::GreetSession {
            session: crate::hook_core::session_id_from_payload(payload),
            source: client,
        },
        Ping::MarkActive => mark_active_request(payload, client),
    };
    if let Ok(mut c) = ds_ipc::connect(&paths.engine_sock)
        && c.send(&req).is_ok()
    {
        let _ = c.recv_terminal();
    }
}

fn earcon_request(payload: &str, event: &str, client: ClientSource) -> ds_ipc::Request {
    ds_ipc::Request::Earcon {
        event: event.to_string(),
        session: crate::hook_core::session_id_from_payload(payload),
        source: client,
    }
}

/// Enqueue earcon (`reply_done` / `needs_input`) behind this session's speech. Engine down ⇒ no-op.
pub fn engine_earcon(paths: &Paths, event: &str, payload: &str, client: ClientSource) {
    let _ = ds_ipc::request(&paths.engine_sock, &earcon_request(payload, event, client));
}

/// Like [`engine_earcon`] with an explicit session (Grok Stop uses sticky `grok-stop:<session>`
/// so reply_done queues behind sticky digests; MarkActive current-clear cannot prune the tag).
pub fn engine_earcon_for_session(
    paths: &Paths,
    event: &str,
    session: Option<String>,
    client: ClientSource,
) {
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::Earcon {
            event: event.to_string(),
            session,
            source: client,
        },
    );
}

/// Notification hook subset: which kind Claude Code surfaced.
#[derive(Debug, Deserialize, Default)]
struct NotificationHook {
    // Grok camelCase alias.
    #[serde(default, alias = "notificationType")]
    notification_type: String,
}

/// Pure gate: only `permission_prompt` / `idle_prompt` warrant needs-input. Other types,
/// missing field, or bad JSON → no (cue stays meaningful).
fn wants_needs_input_earcon(payload: &str) -> bool {
    let kind = serde_json::from_str::<NotificationHook>(payload.trim())
        .map(|h| h.notification_type)
        .unwrap_or_default();
    matches!(kind.as_str(), "permission_prompt" | "idle_prompt")
}

/// Notification notify: needs-input earcon only for "waiting on you" types.
pub fn notification_earcon(paths: &Paths, payload: &str, client: ClientSource) {
    if wants_needs_input_earcon(payload) {
        engine_earcon(paths, "needs_input", payload, client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_prompt_wants_the_earcon() {
        assert!(wants_needs_input_earcon(
            r#"{"notification_type":"permission_prompt"}"#
        ));
    }

    #[test]
    fn idle_prompt_wants_the_earcon() {
        assert!(wants_needs_input_earcon(
            r#"{"notification_type":"idle_prompt"}"#
        ));
    }

    #[test]
    fn other_notification_types_are_silent() {
        assert!(!wants_needs_input_earcon(
            r#"{"notification_type":"auth_success"}"#
        ));
        assert!(!wants_needs_input_earcon(
            r#"{"notification_type":"mcp_elicitation"}"#
        ));
    }

    #[test]
    fn missing_field_is_silent_not_a_panic() {
        assert!(!wants_needs_input_earcon(r#"{}"#));
        assert!(!wants_needs_input_earcon(r#"{"other_key":"value"}"#));
    }

    #[test]
    fn malformed_json_is_silent_not_a_panic() {
        assert!(!wants_needs_input_earcon("not json at all"));
        assert!(!wants_needs_input_earcon("{unterminated"));
    }

    #[test]
    fn empty_string_is_silent_not_a_panic() {
        assert!(!wants_needs_input_earcon(""));
    }

    /// No socket at tempdir engine_sock: connect fails fast; no panic / hang.
    #[test]
    fn engine_ping_with_no_socket_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let c = ClientSource::ClaudeCode;
        engine_ping(&paths, Ping::Greet, r#"{"session_id":"s1"}"#, c);
        engine_ping(&paths, Ping::MarkActive, r#"{"session_id":"s1"}"#, c);
    }

    #[test]
    fn engine_earcon_with_no_socket_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        engine_earcon(
            &paths,
            "needs_input",
            r#"{"sessionId":"grok-session"}"#,
            ClientSource::Grok,
        );
    }

    #[test]
    fn earcon_request_derives_session_from_each_hook_dialect() {
        for payload in [
            r#"{"session_id":"claude-session"}"#,
            r#"{"sessionId":"grok-session"}"#,
        ] {
            let expected = if payload.contains("claude") {
                "claude-session"
            } else {
                "grok-session"
            };
            assert!(matches!(
                earcon_request(payload, "reply_done", ClientSource::ClaudeCode),
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
    fn ordinary_human_prompt_is_not_synthetic() {
        assert!(!is_synthetic_continuation("fix the bug in foo.rs"));
    }

    #[test]
    fn empty_and_missing_prompt_are_not_synthetic() {
        assert!(!is_synthetic_continuation(""));
        assert_eq!(prompt_from_payload(r#"{"session_id":"s1"}"#), "");
        assert!(!is_synthetic_continuation(&prompt_from_payload(
            r#"{"session_id":"s1"}"#
        )));
    }

    #[test]
    fn malformed_json_prompt_payload_is_not_synthetic() {
        assert_eq!(prompt_from_payload("not json at all"), "");
        assert_eq!(prompt_from_payload("{unterminated"), "");
        assert!(!is_synthetic_continuation(&prompt_from_payload(
            "not json at all"
        )));
    }

    #[test]
    fn mark_active_request_carries_synthetic_flag_from_payload_shape() {
        let synthetic_payload =
            r#"{"session_id":"s1","prompt":"<task-notification>\nfoo\n</task-notification>"}"#;
        match mark_active_request(synthetic_payload, ClientSource::ClaudeCode) {
            ds_ipc::Request::MarkActive {
                session,
                synthetic,
                source,
            } => {
                assert_eq!(session, Some("s1".to_string()));
                assert!(synthetic);
                assert_eq!(source, ClientSource::ClaudeCode);
            }
            other => panic!("expected MarkActive, got {other:?}"),
        }

        let ordinary_payload = r#"{"session_id":"s2","prompt":"fix the bug in foo.rs"}"#;
        match mark_active_request(ordinary_payload, ClientSource::Codex) {
            ds_ipc::Request::MarkActive {
                session,
                synthetic,
                source,
            } => {
                assert_eq!(session, Some("s2".to_string()));
                assert!(!synthetic);
                assert_eq!(
                    source,
                    ClientSource::Codex,
                    "the request carries the CLIENT that invoked the hook, verbatim"
                );
            }
            other => panic!("expected MarkActive, got {other:?}"),
        }
    }
}
