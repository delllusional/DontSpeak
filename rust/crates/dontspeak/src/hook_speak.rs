//! Engine-ping side effects for two `notify` events (dispatched from [`crate::hook_core`]).
//! Both are tiny best-effort pings to the warm engine over the IPC socket; neither blocks
//! Claude and neither synthesizes here (the engine owns playback).
//!
//! [`Ping`] variants:
//!   Greet       — SessionStart. A new terminal opened → `GreetSession`, so the engine greets
//!                 in this session's pool voice IF `greet_on_open` is set (engine self-gates).
//!   MarkActive  — UserPromptSubmit. You just prompted HERE → `MarkActive`, so the TTS queue
//!                 speaks only this terminal's items and HOLDS the rest until they become
//!                 active (narration follows the terminal you're working in). EXCEPT: when
//!                 the prompt body is classified [`is_synthetic_continuation`] — a
//!                 harness-injected continuation (e.g. Claude Code auto-re-invoking the agent
//!                 with a `<task-notification>` block after a background task finishes), not
//!                 something a human typed and submitted — the "you just moved your attention
//!                 here" side effects (claiming active-terminal status, cancelling stale
//!                 narration on submit) are skipped engine-side; only session-liveness
//!                 bookkeeping still happens. See issue #11.
//!
//! Spoken REPLIES and tool-step narration are NOT here: for streaming clients (Claude Code)
//! every assistant message rides the ONE `MessageDisplay` → `hook_narrate::message_display`
//! pipeline — the final reply is just another streamed message. Non-streaming clients (Codex)
//! get their final reply voiced from the Stop handler in `hook_core`.

use ds_config::{ClientSource, Paths};
use serde::Deserialize;

/// Which best-effort engine ping a notify event maps to.
pub enum Ping {
    /// SessionStart → greet in this session's pool voice (engine self-gates on `greet_on_open`).
    Greet,
    /// UserPromptSubmit → mark THIS terminal active so narration follows it.
    MarkActive,
}

/// Prompt-body markers that identify a harness-injected CONTINUATION — Claude Code
/// auto-re-invoking the agent with a synthetic user-turn message — rather than
/// something a human actually typed and submitted. Checked as a PREFIX (after
/// trimming leading whitespace), never a substring: a harness continuation IS the
/// entire synthetic prompt (nothing human precedes it), so `starts_with` can't
/// misfire on a human prompt that merely mentions or pastes the tag partway through
/// (e.g. "why did narration cut off — I see `<task-notification>` in the log?").
///
/// Only ONE entry is confirmed today: `<task-notification>`, captured live from a
/// background Bash task's completion re-invoking Claude Code (issue #11, DontSpeak
/// v0.2.2, macOS). Docs research for this fix (Agent Teams' teammate/idle-message
/// delivery, `/loop`/cron scheduled-task wakeups) confirms OTHER harness
/// continuations exist — they also inject between turns with no human present — but
/// Anthropic does not publish their literal wrapper text, so guessing one here would
/// risk either missing real occurrences or matching human text by accident. Add an
/// entry the moment a sibling shape is actually observed (a captured payload, or a
/// documented format) — this table exists precisely so that's a one-line change.
const SYNTHETIC_PROMPT_MARKERS: &[&str] = &["<task-notification>"];

/// PURE: does `prompt` (the `UserPromptSubmit` hook's `prompt` field) look like a
/// harness-injected continuation rather than a genuine human submit? See
/// [`SYNTHETIC_PROMPT_MARKERS`] for the marker table and the prefix-vs-contains
/// rationale.
fn is_synthetic_continuation(prompt: &str) -> bool {
    let trimmed = prompt.trim_start();
    SYNTHETIC_PROMPT_MARKERS
        .iter()
        .any(|m| trimmed.starts_with(m))
}

/// The `UserPromptSubmit` hook's `prompt` field (fail-open: absent/malformed JSON or
/// a non-string value reads as `""`, same idiom as [`crate::hook_core::event_name`] /
/// [`crate::hook_core::session_id_from_payload`]).
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

/// Build the `MarkActive` request for a `UserPromptSubmit` `payload`: the ambient
/// session id, the synthetic classification of the prompt body, and the CLIENT that invoked
/// the hook. Split out from [`engine_ping`] so payload → wire-request shape is unit-testable
/// without a socket.
fn mark_active_request(payload: &str, client: ClientSource) -> ds_ipc::Request {
    ds_ipc::Request::MarkActive {
        session: crate::hook_core::session_id_from_payload(payload),
        synthetic: is_synthetic_continuation(&prompt_from_payload(payload)),
        source: client,
    }
}

/// Fire ONE best-effort ping to the warm engine from a hook `payload` (the Claude Code hook
/// JSON, already read from stdin by the `notify` dispatch — NOT re-read here). Pulls the
/// ambient `session_id` so the engine scopes the greet / active-mark to the right session, and
/// stamps `client` (the `--client` token the wiring embedded) so the engine + its activity log
/// know WHICH client is talking. Engine down ⇒ no-op; never blocks or fails the hook.
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

/// Ask the warm engine to enqueue an audible earcon (`event` = `"reply_done"` /
/// `"needs_input"`) behind this session's earlier speech. Engine down ⇒ no-op.
pub fn engine_earcon(paths: &Paths, event: &str, payload: &str, client: ClientSource) {
    let _ = ds_ipc::request(&paths.engine_sock, &earcon_request(payload, event, client));
}

/// Like [`engine_earcon`], but the session is supplied by the caller (e.g. Grok Stop uses the
/// sticky `grok-stop:<session>` tag so reply_done queues behind sticky digests under the same
/// session tag — ordered relative to each other; the tag is not the real session id so
/// MarkActive current-clear cannot prune them, while `select_pos` still prefers
/// `grok-stop:<active>` with the active terminal).
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

/// The `Notification` hook payload (subset): which kind of notification Claude Code surfaced.
#[derive(Debug, Deserialize, Default)]
struct NotificationHook {
    // Grok sends camelCase (`notificationType`); the alias accepts it alongside Claude's snake_case.
    #[serde(default, alias = "notificationType")]
    notification_type: String,
}

/// PURE: does this `Notification` hook `payload` warrant the needs-input earcon? Only the
/// "waiting on you" notifications (a permission prompt or an idle prompt) do. Other types
/// (auth success, MCP elicitation chatter), a missing field, or malformed JSON all read as
/// "no" so the cue stays meaningful. Split from [`notification_earcon`] so the parse+gate
/// logic is unit-testable without an engine socket.
fn wants_needs_input_earcon(payload: &str) -> bool {
    let kind = serde_json::from_str::<NotificationHook>(payload.trim())
        .map(|h| h.notification_type)
        .unwrap_or_default();
    matches!(kind.as_str(), "permission_prompt" | "idle_prompt")
}

/// `Notification` notify: ring the needs-input earcon — but ONLY for the "waiting on you"
/// notifications (a permission prompt or an idle prompt). Other types (auth success, MCP
/// elicitation chatter) are ignored so the cue stays meaningful. `payload` is the hook JSON.
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

    /// Smoke tests: pointed at a `Paths::rooted_at` tempdir with no socket present at the
    /// resulting `engine_sock`, `engine_ping`/`engine_earcon` must not panic and must return
    /// promptly (the connect-fails-fast branch). The connect-succeeds branch needs a live
    /// engine socket and is out of scope here.
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
