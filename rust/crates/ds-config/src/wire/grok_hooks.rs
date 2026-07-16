//! Grok native hooks (`~/.grok/hooks/dontspeak.json`) — dedicated file we own (overwrite /
//! delete; no merge). Bare binary command so Grok dedupes with imported Claude wiring
//! (compat adapter drops `args`). `GROK_HOOK_EVENT` distinguishes hook vs MCP launch.
//!
//! No MessageDisplay; Stop reads final text from `transcriptPath` JSONL.
//! Events: SessionStart greet-only; SessionEnd barge; UserPromptSubmit notify (+ AGENTS.md
//! digests — stdout ignored, issue #95); Stop narrate+earcon; Notification needs-input.

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
