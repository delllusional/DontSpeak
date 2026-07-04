//! Engine-ping side effects for two `notify` events (dispatched from [`crate::hook_core`]).
//! Both are tiny best-effort pings to the warm engine over the IPC socket; neither blocks
//! Claude and neither synthesizes here (the engine owns playback).
//!
//! [`Ping`] variants:
//!   Greet       — SessionStart. A new terminal opened → `GreetSession`, so the engine greets
//!                 in this session's pool voice IF `greet_on_open` is set (engine self-gates).
//!   MarkActive  — UserPromptSubmit. You just prompted HERE → `MarkActive`, so the TTS queue
//!                 speaks only this terminal's items and HOLDS the rest until they become
//!                 active (narration follows the terminal you're working in).
//!
//! Spoken REPLIES and tool-step narration are NOT here: for streaming clients (Claude Code)
//! every assistant message rides the ONE `MessageDisplay` → `hook_narrate::message_display`
//! pipeline — the final reply is just another streamed message. Non-streaming clients (Codex)
//! get their final reply voiced from the Stop handler in `hook_core`.

use ds_config::Paths;
use serde::Deserialize;

/// Which best-effort engine ping a notify event maps to.
pub enum Ping {
    /// SessionStart → greet in this session's pool voice (engine self-gates on `greet_on_open`).
    Greet,
    /// UserPromptSubmit → mark THIS terminal active so narration follows it.
    MarkActive,
}

/// Fire ONE best-effort ping to the warm engine from a hook `payload` (the Claude Code hook
/// JSON, already read from stdin by the `notify` dispatch — NOT re-read here). Pulls the
/// ambient `session_id` so the engine scopes the greet / active-mark to the right session.
/// Engine down ⇒ no-op; never blocks or fails the hook.
pub fn engine_ping(paths: &Paths, ping: Ping, payload: &str) {
    let session = crate::hook_core::session_id_from_payload(payload);
    let req = match ping {
        Ping::Greet => ds_ipc::Request::GreetSession { session },
        Ping::MarkActive => ds_ipc::Request::MarkActive { session },
    };
    if let Ok(mut c) = ds_ipc::connect(&paths.engine_sock)
        && c.send(&req).is_ok()
    {
        let _ = c.recv_terminal();
    }
}

/// Ask the warm engine to play an audible earcon (`event` = `"reply_done"` / `"needs_input"`).
/// Best-effort fire-and-forget: the engine self-gates on `earcon_enabled` + mute and resolves
/// the sound, so this just forwards the event. Engine down ⇒ no-op; never blocks the hook.
pub fn engine_earcon(paths: &Paths, event: &str) {
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::Earcon {
            event: event.to_string(),
        },
    );
}

/// The `Notification` hook payload (subset): which kind of notification Claude Code surfaced.
#[derive(Debug, Deserialize, Default)]
struct NotificationHook {
    #[serde(default)]
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
pub fn notification_earcon(paths: &Paths, payload: &str) {
    if wants_needs_input_earcon(payload) {
        engine_earcon(paths, "needs_input");
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
        engine_ping(&paths, Ping::Greet, r#"{"session_id":"s1"}"#);
        engine_ping(&paths, Ping::MarkActive, r#"{"session_id":"s1"}"#);
    }

    #[test]
    fn engine_earcon_with_no_socket_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        engine_earcon(&paths, "needs_input");
    }
}
