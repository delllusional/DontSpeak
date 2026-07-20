//! Hermes shell-hooks allowlist (`~/.hermes/shell-hooks-allowlist.json`).
//!
//! Consent keys on exact `(event, command)` pairs. Wire pre-approves every
//! DontSpeak shell-hook pair so non-TTY / gateway sessions register without a
//! first-use prompt. "Ours" = basename `dontspeak` on `command`.

use super::cmdline::command_is_ours;
use serde_json::{Map, Value, json};

/// `(event, command)` pairs Hermes consent needs for our wired shell hooks.
/// Delegates to `hermes_hooks::desired_hook_commands` so allowlist and
/// `hooks:` cannot drift (exact match is Hermes' non-TTY registration key).
pub fn desired_approvals(bin: &str, client: ds_client::ClientSource) -> Vec<(String, String)> {
    super::hermes_hooks::desired_hook_commands(bin, client)
}

fn approval_is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(command_is_ours)
}

fn approval_matches(entry: &Value, event: &str, command: &str) -> bool {
    entry.get("event").and_then(Value::as_str) == Some(event)
        && entry.get("command").and_then(Value::as_str) == Some(command)
}

/// Additive + idempotent + REPLACE-OURS for DontSpeak approvals.
pub fn merge_hermes_allowlist(
    existing: &Value,
    bin: &str,
    client: ds_client::ClientSource,
) -> Value {
    let mut root = if existing.is_object() {
        existing.clone()
    } else {
        Value::Object(Map::new())
    };
    let obj = root.as_object_mut().expect("coerced to object");
    let approvals = obj
        .entry("approvals".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !approvals.is_array() {
        *approvals = Value::Array(Vec::new());
    }
    let arr = approvals.as_array_mut().expect("coerced to array");
    let desired = desired_approvals(bin, client);

    let current_ours: Vec<(String, String)> = arr
        .iter()
        .filter(|e| approval_is_ours(e))
        .filter_map(|e| {
            Some((
                e.get("event")?.as_str()?.to_string(),
                e.get("command")?.as_str()?.to_string(),
            ))
        })
        .collect();
    if current_ours == desired {
        return root;
    }

    arr.retain(|e| !approval_is_ours(e));
    for (event, command) in desired {
        // Avoid duplicating an identical user-approved entry we already left.
        if arr.iter().any(|e| approval_matches(e, &event, &command)) {
            continue;
        }
        arr.push(json!({ "event": event, "command": command }));
    }
    root
}

/// Drop every DontSpeak approval; prune empty `approvals`.
pub fn strip_hermes_allowlist(existing: &Value) -> Value {
    let mut root = existing.clone();
    let Some(obj) = root.as_object_mut() else {
        return root;
    };
    let mut now_empty = false;
    if let Some(arr) = obj.get_mut("approvals").and_then(|a| a.as_array_mut()) {
        arr.retain(|e| !approval_is_ours(e));
        now_empty = arr.is_empty();
    }
    if now_empty {
        obj.remove("approvals");
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_client::ClientSource;

    const BIN: &str = "/home/u/.local/bin/dontspeak";

    #[test]
    fn merge_into_empty_wires_all_event_command_pairs() {
        let out = merge_hermes_allowlist(&Value::Null, BIN, ClientSource::Hermes);
        let arr = out["approvals"].as_array().unwrap();
        assert_eq!(arr.len(), 5);
        let desired = desired_approvals(BIN, ClientSource::Hermes);
        for (i, (event, command)) in desired.iter().enumerate() {
            assert_eq!(arr[i]["event"], *event);
            assert_eq!(arr[i]["command"], *command);
            assert!(command_is_ours(command));
        }
    }

    #[test]
    fn merge_preserves_user_approvals_and_is_idempotent() {
        let existing = json!({
            "approvals": [
                { "event": "pre_tool_call", "command": "/usr/bin/true" }
            ]
        });
        let once = merge_hermes_allowlist(&existing, BIN, ClientSource::Hermes);
        assert_eq!(once["approvals"].as_array().unwrap().len(), 6);
        assert!(
            once["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["command"] == "/usr/bin/true")
        );
        let twice = merge_hermes_allowlist(&once, BIN, ClientSource::Hermes);
        assert_eq!(once, twice);
    }

    #[test]
    fn user_approval_containing_dontspeak_substring_is_not_misidentified() {
        let existing = json!({
            "approvals": [
                { "event": "post_llm_call", "command": "/home/u/bin/my-dontspeak-checker" }
            ]
        });
        let out = merge_hermes_allowlist(&existing, BIN, ClientSource::Hermes);
        assert!(
            out["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["command"] == "/home/u/bin/my-dontspeak-checker")
        );
        let stripped = strip_hermes_allowlist(&out);
        assert!(
            stripped["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["command"] == "/home/u/bin/my-dontspeak-checker")
        );
        assert!(
            stripped["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| !approval_is_ours(e))
        );
    }

    #[test]
    fn rewire_heals_stale_bin_path() {
        let first = merge_hermes_allowlist(&Value::Null, BIN, ClientSource::Hermes);
        let new_bin = "/opt/dontspeak/bin/dontspeak";
        let second = merge_hermes_allowlist(&first, new_bin, ClientSource::Hermes);
        let text = second.to_string();
        assert!(!text.contains(BIN), "stale bin healed");
        assert!(text.contains(new_bin));
        assert_eq!(
            second["approvals"].as_array().unwrap().len(),
            desired_approvals(new_bin, ClientSource::Hermes).len()
        );
    }

    #[test]
    fn strip_removes_only_ours_and_prunes_empty() {
        let existing = json!({
            "approvals": [
                { "event": "pre_tool_call", "command": "/usr/bin/true" }
            ]
        });
        let wired = merge_hermes_allowlist(&existing, BIN, ClientSource::Hermes);
        let stripped = strip_hermes_allowlist(&wired);
        assert_eq!(stripped["approvals"].as_array().unwrap().len(), 1);
        assert_eq!(stripped["approvals"][0]["command"], "/usr/bin/true");

        let only_ours = merge_hermes_allowlist(&Value::Null, BIN, ClientSource::Hermes);
        let stripped = strip_hermes_allowlist(&only_ours);
        assert!(stripped.get("approvals").is_none());
    }

    #[test]
    fn allowlist_pairs_lockstep_with_hook_commands() {
        assert_eq!(
            desired_approvals(BIN, ClientSource::Hermes),
            super::super::hermes_hooks::desired_hook_commands(BIN, ClientSource::Hermes)
        );
    }
}
