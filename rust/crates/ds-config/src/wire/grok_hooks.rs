//! Grok CLI native voice hooks (`~/.grok/hooks/dontspeak.json`).
//!
//! Grok reads per-file hook definitions out of `~/.grok/hooks/*.json` (and project
//! `.grok/hooks/*.json`) using a Claude-COMPATIBLE event contract — events routed by
//! `hookEventName`, one JSON object on stdin, `Stop` carrying the final assistant message —
//! so the SAME `dontspeak` binary serves them. Unlike the
//! Codex/Qwen hook surfaces, we do NOT merge into a file the client also owns: DontSpeak
//! writes its OWN dedicated file (`dontspeak.json`) that it owns outright, so wiring is a
//! whole-file overwrite (a backup is taken first) and unwiring simply DELETES the file. No
//! `toml_edit`/`merge` machinery is needed — there is nothing of the user's to preserve in
//! a file that is exclusively ours.
//!
//! Grok also imports `~/.claude/settings.json` by default, but its compatibility adapter
//! ignores Claude Code's `args` array. A Claude DontSpeak entry therefore collapses to the
//! bare binary path. Native Grok entries deliberately use that exact same bare command:
//! Grok deduplicates identical command targets across sources, so native + compatibility
//! wiring becomes one process per event instead of two registrations. The hook runner's
//! reserved `GROK_HOOK_EVENT` environment variable lets the no-argument binary distinguish
//! this launch from its normal no-argument MCP-server role and dispatch both the command
//! side effect and (for `UserPromptSubmit`) the query response itself.
//!
//! The per-event set is the full non-streaming shape (Grok has no `MessageDisplay` stream, so
//! the reply is voiced whole from `Stop`):
//!
//!   * `SessionStart` → notify, greet-only — the spoken greeting without seeding the
//!     streaming witness, which on a hook-only client would mark every session "already
//!     narrated" and silence the `Stop` reply.
//!   * `SessionEnd` → `notify` — barge this session's playback on close.
//!   * `UserPromptSubmit` → notify + provide in one process — mark-active routing, and inject the
//!     narration spec so Grok WRITES the spoken-line blockquotes.
//!   * `Stop` → `notify` — speak the final reply (end-of-turn narration).
//!   * `Notification` → `notify` — the needs-input earcon.
//!
//! Rendered as one group and one handler per event:
//!   { "hooks": { "Stop": [ { "hooks": [ { "type": "command",
//!                                         "command": "…/dontspeak",
//!                                         "timeout": 1800 } ] } ] } }

use serde_json::{Map, Value, json};

/// The full non-streaming event set and seconds timeouts. Every event gets one bare-binary
/// handler; the runtime performs the event-specific notify/provide combination.
const GROK_HOOKS: &[(&str, i64)] = &[
    ("SessionStart", 30),
    ("SessionEnd", 30),
    ("UserPromptSubmit", 5),
    ("Stop", 1800),
    ("Notification", 30),
];

/// Render the dedicated Grok hooks file body — the WHOLE `dontspeak.json` (DontSpeak owns the
/// file outright, so this is not merged into anything). `bin` is the absolute path to the
/// `dontspeak` binary. The exact, unmodified path is load-bearing: it matches the `command`
/// in Claude Code's args-array entries byte-for-byte, allowing Grok to deduplicate the native
/// and imported handlers. There are no `args`, `async`, `shell`, or `matcher` keys.
pub fn grok_hooks_value(bin: &str) -> Value {
    let mut events = Map::new();
    for (event, timeout) in GROK_HOOKS {
        events.insert(
            (*event).to_string(),
            json!([{ "hooks": [{
                "type": "command",
                "command": bin,
                "timeout": timeout,
            }] }]),
        );
    }
    json!({ "hooks": events })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "/home/u/.local/bin/dontspeak";

    /// Every inner `(command, timeout)` wired on `event`, in order — asserting the event holds
    /// exactly one group along the way (so callers also pin "not duplicated").
    fn event_entries(v: &Value, event: &str) -> Vec<(String, i64)> {
        let groups = v["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("{event}: array of groups"));
        assert_eq!(groups.len(), 1, "{event}: exactly one group");
        groups[0]["hooks"]
            .as_array()
            .unwrap_or_else(|| panic!("{event}: inner hooks array"))
            .iter()
            .map(|h| {
                (
                    h["command"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{event}: command is a string"))
                        .to_string(),
                    h["timeout"]
                        .as_i64()
                        .unwrap_or_else(|| panic!("{event}: timeout is an integer")),
                )
            })
            .collect()
    }

    fn value() -> Value {
        grok_hooks_value(BIN)
    }

    #[test]
    fn value_round_trips_through_serde_json() {
        let v = value();
        let s = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn exactly_the_five_expected_events_present() {
        let v = value();
        let hooks = v["hooks"].as_object().unwrap();
        let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "Notification",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "UserPromptSubmit",
            ]
        );
    }

    #[test]
    fn every_event_uses_one_identical_bare_command_and_seconds_timeout() {
        let v = value();
        for (event, timeout) in GROK_HOOKS {
            assert_eq!(
                event_entries(&v, event),
                vec![(BIN.to_string(), *timeout)],
                "{event}: one bare command so Grok deduplicates Claude compatibility"
            );
        }
    }

    #[test]
    fn every_entry_carries_a_timeout_and_no_async_key() {
        let v = value();
        let hooks = v["hooks"].as_object().unwrap();
        for (event, groups) in hooks {
            for group in groups.as_array().unwrap() {
                for h in group["hooks"].as_array().unwrap() {
                    assert!(
                        h.get("timeout").and_then(Value::as_i64).is_some(),
                        "{event}: every entry carries a numeric (seconds) timeout"
                    );
                    assert!(
                        h.get("async").is_none(),
                        "{event}: Grok hooks run synchronously — never emit an async key"
                    );
                    assert!(
                        h.get("matcher").is_none(),
                        "{event}: no matcher key on our entries"
                    );
                    assert_eq!(h["type"].as_str(), Some("command"), "{event}: type=command");
                }
            }
        }
    }
}
