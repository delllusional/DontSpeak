//! Claude Code hook dispatch behind the two stdio entries (`dontspeak notify` /
//! `dontspeak provide`). The whole interaction is just "event name + payload JSON in →
//! optional JSON out".
//!
//! The split is by CONTRACT (command vs query), not by event:
//!   • [`notify`]  — COMMAND: the client notifies us of an event; we run the side effect
//!                   and reply with NOTHING. Fire-and-forget, never blocks, errors ignored.
//!                   (MessageDisplay, SessionStart, SessionEnd, UserPromptSubmit→mark-active,
//!                   and Stop→speak-the-final-reply for non-streaming clients like Codex.)
//!                   These are wired `async`, so Claude Code discards their stdout — fine, they
//!                   reply with nothing.
//!   • [`provide`] — QUERY: Claude Code asks us for input and WAITS; we return JSON it renders.
//!                   (UserPromptSubmit → the narration spec.)
//!
//! A single CC event can ride BOTH (UserPromptSubmit marks the terminal active AND provides
//! the spec) — they're two different interaction kinds that happen to share the event.
//!
//! The SessionStart GREETING is voice-only. A visible banner used to ride a synchronous
//! `provide` twin, but CC 2.1+ drops a SessionStart hook's `systemMessage` and the
//! `terminalSequence` OSC notification only fires on terminals that implement it — so it
//! never reliably surfaced and was removed. The greeting is just the engine voice greet.

use serde::Deserialize;
use serde_json::Value;

use crate::{hook_narrate, hook_prompt, hook_speak};

/// The one field every Claude Code hook payload carries that we route on.
#[derive(Deserialize, Default)]
struct EventEnvelope {
    #[serde(default)]
    hook_event_name: String,
}

/// Pull the `hook_event_name` out of a raw hook payload (empty string if absent/unparseable).
pub fn event_name(payload: &str) -> String {
    serde_json::from_str::<EventEnvelope>(payload.trim())
        .map(|e| e.hook_event_name)
        .unwrap_or_default()
}

/// COMMAND: run the side effect for `event` from its `payload`; no reply. Unknown events are
/// ignored (forward-compatible — a newly-wired event we don't handle yet is a no-op).
pub fn notify(event: &str, payload: &str) {
    let Some(paths) = ds_config::Paths::resolve() else {
        return;
    };
    match event {
        "SessionStart" => {
            hook_speak::engine_ping(&paths, hook_speak::Ping::Greet, payload);
            // Seed this session's streaming witness so the Stop handler reliably knows Claude
            // Code narrates via MessageDisplay (closing the only timing gap in the double-
            // narration guard). Codex wires no SessionStart, so it never seeds — and its Stop
            // still voices the reply.
            hook_narrate::mark_streaming_session(&paths, payload);
            // Greeting is voice-only (the engine greet above); no visible banner — see module docs.
        }
        "UserPromptSubmit" => {
            hook_speak::engine_ping(&paths, hook_speak::Ping::MarkActive, payload)
        }
        "SessionEnd" => hook_narrate::barge_session(&paths, payload),
        "MessageDisplay" => hook_narrate::message_display(&paths, payload),
        // Two clients send Stop, handled by ONE arm:
        //  • Codex (no MessageDisplay stream) → speak_reply voices `last_assistant_message`.
        //  • Claude Code streams via MessageDisplay but ALSO delivers `last_assistant_message`
        //    on Stop, so speak_reply self-gates on this session's MessageDisplay state file
        //    (present ⇒ already narrated ⇒ silent); CC wires Stop for the turn-done ding.
        // The reply-done earcon then rings for both (engine self-gates on `earcon_enabled` +
        // mute), so a finished turn is signalled whether or not the reply was just voiced.
        "Stop" => {
            hook_narrate::speak_reply(&paths, payload);
            hook_speak::engine_earcon(&paths, "reply_done");
        }
        // A permission prompt / idle notification → the needs-input earcon (the handler filters
        // to just the "waiting on you" notification types).
        "Notification" => hook_speak::notification_earcon(&paths, payload),
        _ => {}
    }
}

/// QUERY: return the `hookSpecificOutput` JSON Claude Code should inject for `event`, or
/// `None` when this event owes no reply (or narration is off). `payload` is reserved for
/// future per-event queries that need it.
pub fn provide(event: &str, _payload: &str) -> Option<Value> {
    match event {
        "UserPromptSubmit" => hook_prompt::narration_context(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_start_owes_no_provide_reply() {
        // The greeting is voice-only — SessionStart no longer returns a visible banner from the
        // sync `provide` path (CC 2.1+ drops a SessionStart hook's stdout; see module docs).
        assert!(provide("SessionStart", "{}").is_none());
    }
}
