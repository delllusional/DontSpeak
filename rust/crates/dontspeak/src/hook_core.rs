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
    // Grok sends the camelCase key; Claude-compatible clients send the snake_case key.
    #[serde(default, alias = "hookEventName")]
    hook_event_name: String,
}

/// Convert Grok's lowercase-snake event values to the Claude-compatible PascalCase dialect.
/// Applying it to an already-PascalCase value is an identity operation, and doing this
/// mechanically keeps new upstream event names from requiring a hand-maintained match table.
fn normalize_event_name(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len());
    let mut capitalize = true;
    for ch in raw.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            normalized.extend(ch.to_uppercase());
            capitalize = false;
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

/// Pull the event name out of a raw hook payload, returning an empty string when absent or
/// malformed. Grok's live-verified lowercase-snake values are normalized to the PascalCase
/// contract used by the dispatch arms; Claude-compatible values pass through unchanged.
pub fn event_name(payload: &str) -> String {
    let raw = serde_json::from_str::<EventEnvelope>(payload.trim())
        .map(|e| e.hook_event_name)
        .unwrap_or_default();
    normalize_event_name(&raw)
}

/// The `session_id` every Claude Code hook payload carries. Parsed ambiently so callers
/// ([`hook_speak::engine_ping`], [`hook_narrate::barge_session`],
/// [`hook_narrate::mark_streaming_session`]) can scope the greet / active-mark / barge /
/// streaming-witness to the right Claude session.
#[derive(Deserialize, Default)]
struct SessionEnvelope {
    // Grok sends camelCase `sessionId` (live-verified); Claude-compatible clients use snake_case.
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
}

/// Pull the Claude `session_id` out of any hook JSON, ignoring an empty/whitespace-only one —
/// every caller treats that as "unscoped".
pub fn session_id_from_payload(payload: &str) -> Option<String> {
    serde_json::from_str::<SessionEnvelope>(payload.trim())
        .ok()
        .and_then(|e| e.session_id)
        .filter(|s| !s.trim().is_empty())
}

/// COMMAND: run the side effect for `event` from its `payload`; no reply. Unknown events are
/// ignored (forward-compatible — a newly-wired event we don't handle yet is a no-op).
/// `greet_only` is the `dontspeak notify --greet-only` flag, wired on SessionStart for
/// NON-streaming clients (Qwen Code, OpenAI Codex): greet, but skip the streaming-witness seed — see
/// [`notify_at`]. `client` is the `--client <token>` the wiring stamped (see
/// `client_from_argv`): it rides onto every `ds-ipc` request this dispatch sends, so the engine
/// and its activity log know WHICH client caused the event. Resolves the real `Paths` and
/// delegates to the injectable core.
pub fn notify(event: &str, payload: &str, greet_only: bool, client: ds_config::ClientSource) {
    let Some(paths) = ds_config::Paths::resolve() else {
        return;
    };
    notify_at(&paths, event, payload, greet_only, client);
}

/// Injectable core of [`notify`] — takes `paths` so tests drive it against a
/// tempdir-rooted `Paths` (never the real `$HOME`; the engine pings are best-effort
/// no-ops against the tempdir's nonexistent socket).
pub(crate) fn notify_at(
    paths: &ds_config::Paths,
    event: &str,
    payload: &str,
    greet_only: bool,
    client: ds_config::ClientSource,
) {
    match event {
        "SessionStart" => {
            hook_speak::engine_ping(paths, hook_speak::Ping::Greet, payload, client);
            // Seed this session's streaming witness so the Stop handler reliably knows Claude
            // Code narrates via MessageDisplay (closing the only timing gap in the double-
            // narration guard). ONLY for streaming clients: `--greet-only` (Qwen Code and
            // OpenAI Codex, which wire SessionStart but have NO MessageDisplay stream) skips
            // the seed — seeding would mark every session "already narrated" and silence
            // each Stop reply.
            if !greet_only {
                hook_narrate::mark_streaming_session(paths, payload);
            }
            // Greeting is voice-only (the engine greet above); no visible banner — see module docs.
        }
        "UserPromptSubmit" => {
            hook_speak::engine_ping(paths, hook_speak::Ping::MarkActive, payload, client)
        }
        "SessionEnd" => hook_narrate::barge_session(paths, payload, client),
        "MessageDisplay" => hook_narrate::message_display(paths, payload, client),
        // Multiple clients send Stop, handled by ONE arm:
        //  • Codex / Qwen Code (no MessageDisplay stream) → speak_reply voices
        //    `last_assistant_message`.
        //  • Claude Code streams via MessageDisplay but ALSO delivers `last_assistant_message`
        //    on Stop, so speak_reply self-gates on this session's MessageDisplay state file
        //    (present ⇒ already narrated ⇒ silent); CC wires Stop for the turn-done ding.
        //  • Grok Stop is metadata-only; speak_reply now falls back to the `transcriptPath`
        //    file (chat_history.jsonl etc.) to obtain the final assistant text (#49).
        // The reply-done earcon then rings for every client (engine self-gates on `earcon_enabled` +
        // mute), so a finished turn is signalled whether or not the reply was just voiced.
        "Stop" => {
            hook_narrate::speak_reply(paths, payload, client);
            hook_speak::engine_earcon(paths, "reply_done", client);
        }
        // A permission prompt / idle notification → the needs-input earcon (the handler filters
        // to just the "waiting on you" notification types).
        "Notification" => hook_speak::notification_earcon(paths, payload, client),
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
    fn event_name_normalizes_the_grok_dialect_without_an_event_table() {
        for (wire, canonical) in [
            ("stop", "Stop"),
            ("session_start", "SessionStart"),
            ("user_prompt_submit", "UserPromptSubmit"),
            ("post_tool_use_failure", "PostToolUseFailure"),
            ("UserPromptSubmit", "UserPromptSubmit"),
        ] {
            let payload = format!(r#"{{"hookEventName":"{wire}"}}"#);
            assert_eq!(event_name(&payload), canonical, "{wire}");
        }
        assert_eq!(event_name("not json"), "");
    }

    #[test]
    fn session_start_owes_no_provide_reply() {
        // The greeting is voice-only — SessionStart no longer returns a visible banner from the
        // sync `provide` path (CC 2.1+ drops a SessionStart hook's stdout; see module docs).
        assert!(provide("SessionStart", "{}").is_none());
    }

    /// A reply shaped like a spoken digest — what Stop would voice (or wrongly suppress).
    const DIGEST_REPLY: &str = "> First point.\n\nDetail.";

    #[test]
    fn greet_only_session_start_skips_witness_so_stop_still_voices() {
        // The non-streaming-client fix: `notify --greet-only` on SessionStart (Qwen Code)
        // greets but must NOT seed the streaming witness — Stop is that client's ONLY
        // narration path, and a seeded witness silences every reply. Driven against a
        // tempdir-rooted `Paths`: the SessionStart engine ping is a best-effort no-op on the
        // tempdir's nonexistent socket (see hook_speak's engine_ping_with_no_socket test).
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "qwen-session-aaaa";
        let payload = format!(r#"{{"hook_event_name":"SessionStart","session_id":"{session}"}}"#);

        notify_at(
            &paths,
            "SessionStart",
            &payload,
            /*greet_only*/ true,
            ds_config::ClientSource::QwenCode,
        );
        let streamed = hook_narrate::streamed_via_message_display(&paths, session);
        assert!(
            !streamed,
            "greet-only SessionStart must not seed the streaming witness"
        );
        assert_eq!(
            hook_narrate::stop_utterances(Some(DIGEST_REPLY), true, false, false, streamed),
            vec!["First point.".to_string()],
            "unseeded session ⇒ Stop voices the whole reply"
        );
    }

    #[test]
    fn plain_session_start_seeds_witness_so_stop_stays_silent() {
        // Counterpart: the streaming wiring (plain `notify`, Claude Code) DOES seed the
        // witness, so Stop never double-speaks what MessageDisplay already narrated.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-bbbb";
        let payload = format!(r#"{{"hook_event_name":"SessionStart","session_id":"{session}"}}"#);

        notify_at(
            &paths,
            "SessionStart",
            &payload,
            /*greet_only*/ false,
            ds_config::ClientSource::ClaudeCode,
        );
        let streamed = hook_narrate::streamed_via_message_display(&paths, session);
        assert!(streamed, "streaming SessionStart seeds the witness");
        assert!(
            hook_narrate::stop_utterances(Some(DIGEST_REPLY), true, false, false, streamed)
                .is_empty(),
            "seeded session ⇒ Stop stays silent"
        );
    }

    #[test]
    fn grok_session_start_live_shape_parses_both_dialects() {
        let session = "grok-session-zzzz";
        let payload = format!(r#"{{"hookEventName":"session_start","sessionId":"{session}"}}"#);
        assert_eq!(event_name(&payload), "SessionStart");
        assert_eq!(session_id_from_payload(&payload).as_deref(), Some(session));
    }
}
