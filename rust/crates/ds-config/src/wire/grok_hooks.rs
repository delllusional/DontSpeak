//! Grok CLI native voice hooks (`~/.grok/hooks/dontspeak.json`).
//!
//! Grok reads per-file hook definitions out of `~/.grok/hooks/*.json` (and project
//! `.grok/hooks/*.json`) using a Claude-COMPATIBLE event contract — events routed by
//! `hookEventName`, one JSON object on stdin, `Stop` carrying the final assistant message —
//! so the SAME `dontspeak notify` / `dontspeak provide` binary serves them. Unlike the
//! Codex/Qwen hook surfaces, we do NOT merge into a file the client also owns: DontSpeak
//! writes its OWN dedicated file (`dontspeak.json`) that it owns outright, so wiring is a
//! whole-file overwrite (a backup is taken first) and unwiring simply DELETES the file. No
//! `toml_edit`/`merge` machinery is needed — there is nothing of the user's to preserve in
//! a file that is exclusively ours.
//!
//! Two things differ from the Claude Code JSON wiring the [`hooks`](super::hooks) module
//! emits:
//!   * the verb is INLINED into `command` (a single shell string, NO `args` array), rendered
//!     by the shared [`cmdline`](super::cmdline) — quoted on POSIX, but QUOTE-FREE on Windows,
//!     where an embedded `"` cannot survive cmd.exe (see that module);
//!   * every entry runs SYNCHRONOUSLY: there is NO `async` key and NO `matcher` key, and the
//!     `timeout` is in SECONDS.
//!
//! The per-event set is the full non-streaming shape (Grok has no `MessageDisplay` stream, so
//! the reply is voiced whole from `Stop`):
//!
//!   * `SessionStart` → `notify --greet-only` — the spoken greeting. `--greet-only` skips the
//!     streaming-witness seed, which on a hook-only client would mark every session "already
//!     narrated" and silence the `Stop` reply.
//!   * `SessionEnd` → `notify` — barge this session's playback on close.
//!   * `UserPromptSubmit` → `notify` + `provide` — mark-active routing, and inject the
//!     narration spec so Grok WRITES the spoken-line blockquotes.
//!   * `Stop` → `notify` — speak the final reply (end-of-turn narration).
//!   * `Notification` → `notify` — the needs-input earcon.
//!
//! Rendered as (per event, one group holding that event's inner hooks) — POSIX `command`
//! shown; on Windows it is the quote-free `C:/…/dontspeak.exe notify`:
//!   { "hooks": { "Stop": [ { "hooks": [ { "type": "command",
//!                                         "command": "\"…/dontspeak\" notify",
//!                                         "timeout": 1800 } ] } ] } }

use super::cmdline::{ShellOverride, host_inline_flavor, inline_command};
use ds_client::ClientSource;
use serde_json::{Map, Value, json};

/// The `(event, [(verb, timeout_secs)])` hooks DontSpeak owns for Grok — ONE group per event,
/// holding that event's inner hooks. The full non-streaming set: Grok has no MessageDisplay
/// stream, so the reply is voiced from `Stop`; the narration spec is injected at
/// `UserPromptSubmit` via `provide` (alongside a `notify` sibling for mark-active routing);
/// and `SessionStart` greets with `--greet-only` — the flag skips the streaming-witness seed,
/// which on a hook-only client would silence every `Stop` reply. Timeouts are SECONDS, and
/// every entry runs SYNCHRONOUSLY (no `async` key is ever emitted).
const GROK_HOOKS: &[(&str, &[(&str, i64)])] = &[
    ("SessionStart", &[("notify --greet-only", 30)]),
    ("SessionEnd", &[("notify", 30)]),
    ("UserPromptSubmit", &[("notify", 5), ("provide", 5)]),
    ("Stop", &[("notify", 1800)]),
    ("Notification", &[("notify", 30)]),
];

/// Render the dedicated Grok hooks file body — the WHOLE `dontspeak.json` (DontSpeak owns the
/// file outright, so this is not merged into anything). `bin` is the absolute path to the
/// `dontspeak` binary; each command is rendered by the SHARED `super::cmdline::inline_command`
/// (with a seconds `timeout`, and no `async`/`matcher`/`args` keys) — the same string Codex
/// gets, because Grok's hook entry likewise carries a bare command string with NO `shell`
/// field to pin it to a quote-tolerant shell. Each command carries the uniform trailing
/// `--client <token>` ([`ClientSource`]) every hook mechanism stamps, so the spawned binary
/// knows who invoked it; `client` is passed in (always `Grok` today) rather than hardcoded, so
/// this shaper stays client-agnostic.
pub fn grok_hooks_value(bin: &str, client: ClientSource) -> Value {
    let mut events = Map::new();
    for (event, verbs) in GROK_HOOKS {
        let inner: Vec<Value> = verbs
            .iter()
            .map(|(verb, timeout)| {
                let (command, _shell) = inline_command(
                    host_inline_flavor(),
                    bin,
                    &[verb, "--client", client.as_str()],
                    ShellOverride::Unsupported,
                );
                json!({
                    "type": "command",
                    "command": command,
                    "timeout": timeout,
                })
            })
            .collect();
        events.insert((*event).to_string(), json!([{ "hooks": inner }]));
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

    /// The command string Grok should carry for `verb` on THIS host — INCLUDING the uniform
    /// `--client grok` tail every wired verb now carries. The DIALECT itself (the quote-free
    /// Windows form, the quoted POSIX form, the 8.3 spaced-path fallback) is pinned per-flavor
    /// by `wire::cmdline`'s own tests; these tests pin Grok's STRUCTURE — which events, which
    /// verbs, which timeouts, one group each — without hardcoding a dialect that would make
    /// them pass on Linux CI and fail on a Windows host.
    fn grok_command(verb: &str) -> String {
        inline_command(
            host_inline_flavor(),
            BIN,
            &[verb, "--client", "grok"],
            ShellOverride::Unsupported,
        )
        .0
    }

    fn value() -> Value {
        grok_hooks_value(BIN, ClientSource::Grok)
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
    fn exact_per_event_command_strings_and_seconds_timeouts() {
        let v = value();
        assert_eq!(
            event_entries(&v, "SessionStart"),
            vec![(grok_command("notify --greet-only"), 30)]
        );
        assert_eq!(
            event_entries(&v, "SessionEnd"),
            vec![(grok_command("notify"), 30)]
        );
        // UserPromptSubmit is ONE group with TWO inner hooks: notify (mark-active) then provide.
        assert_eq!(
            event_entries(&v, "UserPromptSubmit"),
            vec![(grok_command("notify"), 5), (grok_command("provide"), 5),]
        );
        // Stop's 1800 s is the tell that timeouts are SECONDS (not ms).
        assert_eq!(
            event_entries(&v, "Stop"),
            vec![(grok_command("notify"), 1800)]
        );
        assert_eq!(
            event_entries(&v, "Notification"),
            vec![(grok_command("notify"), 30)]
        );
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
