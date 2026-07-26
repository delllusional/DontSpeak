//! Claude Code hook dispatch behind `dontspeak notify` / `dontspeak provide`.
//! Interaction is "event name + payload JSON in → optional JSON out".
//!
//! Split by **contract** (command vs query), not by event:
//!   • [`notify`]  — COMMAND: side effect, reply nothing. Fire-and-forget; wired `async`
//!                   so Claude Code discards stdout. (MessageDisplay, SessionStart/End,
//!                   UserPromptSubmit→mark-active, Stop→final reply for non-streaming clients.)
//!   • [`provide`] — QUERY: Claude waits; we return JSON it renders.
//!                   (UserPromptSubmit → narration spec.)
//!
//! One CC event can ride both (UserPromptSubmit marks active AND provides the spec).
//!
//! SessionStart greeting is voice-only. A visible banner used to ride a synchronous
//! `provide` twin, but CC 2.1+ drops SessionStart `systemMessage` and the OSC path only
//! fires on terminals that implement it — removed as unreliable.

use serde::Deserialize;
use serde_json::Value;

use crate::{hook_narrate, hook_prompt, hook_speak};

/// The one field every Claude Code hook payload carries that we route on.
#[derive(Deserialize, Default)]
struct EventEnvelope {
    // Grok: camelCase; Claude-compatible: snake_case.
    #[serde(default, alias = "hookEventName")]
    hook_event_name: String,
}

/// Hermes dialect → Claude PascalCase (before TitleCase, which would yield
/// `OnSessionStart` / `PreLlmCall` / … and miss our handlers).
fn remap_hermes_event(raw: &str) -> Option<&'static str> {
    match raw {
        "on_session_start" => Some("SessionStart"),
        "pre_llm_call" => Some("UserPromptSubmit"),
        "post_llm_call" => Some("Stop"),
        "on_session_finalize" => Some("SessionEnd"),
        _ => None,
    }
}

/// Grok lowercase-snake → Claude PascalCase. Identity on already-PascalCase; mechanical so
/// new upstream names need no hand-maintained match table.
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

/// Event name from raw hook payload; empty when absent/malformed.
/// Hermes remapped first; then Grok TitleCase normalize.
pub fn event_name(payload: &str) -> String {
    let raw = serde_json::from_str::<EventEnvelope>(payload.trim())
        .map(|e| e.hook_event_name)
        .unwrap_or_default();
    if let Some(mapped) = remap_hermes_event(&raw) {
        return mapped.to_string();
    }
    normalize_event_name(&raw)
}

/// `session_id` for scoping greet / active-mark / barge / streaming-witness.
#[derive(Deserialize, Default)]
struct SessionEnvelope {
    // Grok: camelCase `sessionId` (live-verified); Claude-compatible: snake_case.
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
}

/// Claude `session_id` from any hook JSON; empty/whitespace ⇒ "unscoped".
pub fn session_id_from_payload(payload: &str) -> Option<String> {
    serde_json::from_str::<SessionEnvelope>(payload.trim())
        .ok()
        .and_then(|e| e.session_id)
        .filter(|s| !s.trim().is_empty())
}

/// COMMAND: side effect for `event`; no reply. Unknown events ignored (forward-compatible).
/// `greet_only` = `notify --greet-only` (non-streaming SessionStart — see [`notify_at`]).
/// `client` from wiring stamps every ds-ipc request so the activity log knows who caused it.
pub fn notify(event: &str, payload: &str, greet_only: bool, client: ds_config::WiredAgent) {
    let context = ds_config::ClientContext::for_hook(client);
    if !hook_surface_allowed(context) {
        return;
    }
    let Some(paths) = ds_config::Paths::resolve() else {
        return;
    };
    notify_at(&paths, event, payload, greet_only, client);
}

/// Injectable [`notify`] core — tests use tempdir-rooted `Paths` (engine pings are no-ops).
pub(crate) fn notify_at(
    paths: &ds_config::Paths,
    event: &str,
    payload: &str,
    greet_only: bool,
    client: ds_config::WiredAgent,
) {
    match event {
        "SessionStart" => {
            hook_speak::engine_ping(paths, hook_speak::Ping::Greet, payload, client);
            // Seed witness before first streaming batch; `--greet-only` skips for Stop clients.
            if !greet_only {
                hook_narrate::mark_streaming_session(paths, payload);
            }
        }
        "UserPromptSubmit" => {
            hook_speak::engine_ping(paths, hook_speak::Ping::MarkActive, payload, client)
        }
        "SessionEnd" => hook_narrate::barge_session(paths, payload, client),
        "MessageDisplay" => hook_narrate::message_display(paths, payload, client),
        // Stop: one arm, multi-client —
        //  • Plain-TUI Codex: no MessageDisplay → speak_reply voices `last_assistant_message`.
        //  • Claude/Qwen: MessageDisplay stream → speak_reply self-gates on witness.
        //  • Grok: metadata-only Stop; falls back to transcriptPath file (#49).
        // reply_done earcon queues behind admitted narration. Grok may return a sticky admit
        // session so digests and ding share one queue key (see `grok_stop_session_tag`).
        "Stop" => {
            let earcon_session = hook_narrate::speak_reply(paths, payload, client);
            match earcon_session {
                Some(session) => {
                    hook_speak::engine_earcon_for_session(
                        paths,
                        ds_earcon::EarconEvent::ReplyDone,
                        session,
                        client,
                    );
                }
                None => hook_speak::engine_earcon(
                    paths,
                    ds_earcon::EarconEvent::ReplyDone,
                    payload,
                    client,
                ),
            }
        }
        // Permission / idle only — handler filters "waiting on you" types.
        "Notification" => hook_speak::notification_earcon(paths, payload, client),
        _ => {}
    }
}

/// QUERY: context JSON for `event`, or `None` when no reply / narration off.
/// Hermes uses flat `{"context":…}`; other clients keep Claude `hookSpecificOutput`.
pub fn provide(event: &str, _payload: &str, client: ds_config::WiredAgent) -> Option<Value> {
    let context = ds_config::ClientContext::for_hook(client);
    if !hook_surface_allowed(context) {
        return None;
    }
    match event {
        "UserPromptSubmit" => hook_prompt::narration_context(client),
        _ => None,
    }
}

fn hook_surface_allowed(context: ds_config::ClientContext) -> bool {
    context.allows_narration()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_and_provide_share_the_cli_vs_desktop_surface_policy() {
        for (context, allowed) in [
            (
                ds_config::ClientContext::for_wired_with_markers(
                    ds_config::WiredAgent::Codex,
                    None,
                    None,
                ),
                true,
            ),
            (
                ds_config::ClientContext::for_wired_with_markers(
                    ds_config::WiredAgent::ClaudeCode,
                    None,
                    None,
                ),
                true,
            ),
            (
                ds_config::ClientContext::for_wired_with_markers(
                    ds_config::WiredAgent::Codex,
                    Some("Codex Desktop"),
                    None,
                ),
                false,
            ),
            (
                ds_config::ClientContext::for_wired_with_markers(
                    ds_config::WiredAgent::ClaudeCode,
                    None,
                    Some("claude-desktop"),
                ),
                false,
            ),
            (
                ds_config::ClientContext::for_wired_with_markers(
                    ds_config::WiredAgent::ClaudeCode,
                    None,
                    Some("claude-desktop-3p"),
                ),
                false,
            ),
        ] {
            assert_eq!(hook_surface_allowed(context), allowed, "{context:?}");
        }
    }

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
    fn event_name_remaps_hermes_events_before_titlecase() {
        for (wire, canonical) in [
            ("on_session_start", "SessionStart"),
            ("pre_llm_call", "UserPromptSubmit"),
            ("post_llm_call", "Stop"),
            ("on_session_finalize", "SessionEnd"),
        ] {
            let payload = format!(r#"{{"hook_event_name":"{wire}"}}"#);
            assert_eq!(event_name(&payload), canonical, "{wire}");
        }
        // Without the remap table, TitleCase would yield OnSessionStart.
        assert_ne!(
            event_name(r#"{"hook_event_name":"on_session_start"}"#),
            "OnSessionStart"
        );
    }

    #[test]
    fn session_start_owes_no_provide_reply() {
        // Greeting is voice-only; SessionStart no longer returns a banner (CC 2.1+).
        assert!(provide("SessionStart", "{}", ds_config::WiredAgent::ClaudeCode).is_none());
    }

    /// Digest-shaped reply — what Stop would voice (or wrongly suppress).
    const DIGEST_REPLY: &str = "> First point.\n\nDetail.";

    #[test]
    fn greet_only_session_start_skips_witness_so_stop_still_voices() {
        // Non-streaming `notify --greet-only` must not seed the witness or Stop is silenced.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "qwen-session-aaaa";
        let payload = format!(r#"{{"hook_event_name":"SessionStart","session_id":"{session}"}}"#);

        notify_at(
            &paths,
            "SessionStart",
            &payload,
            /*greet_only*/ true,
            ds_config::WiredAgent::QwenCode,
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
        // Streaming wiring (plain notify) seeds so Stop never double-speaks MessageDisplay.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-bbbb";
        let payload = format!(r#"{{"hook_event_name":"SessionStart","session_id":"{session}"}}"#);

        notify_at(
            &paths,
            "SessionStart",
            &payload,
            /*greet_only*/ false,
            ds_config::WiredAgent::ClaudeCode,
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
