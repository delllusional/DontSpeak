//! Hermes Agent shell hooks (`~/.hermes/config.yaml` `hooks:` block).
//!
//! Nested `hooks.<event>: [{command, timeout}, …]` — events from Hermes
//! `VALID_HOOKS`. Commands via [`cmdline`](super::cmdline) (shlex-split,
//! `shell=False`). Timeouts SECONDS, capped at Hermes 300.
//!
//! Non-streaming: SessionStart greet-only; pre_llm_call notify+provide;
//! post_llm_call Stop; on_session_finalize SessionEnd. "Ours" = basename
//! `dontspeak` ([`command_is_ours`](super::cmdline::command_is_ours)).
//!
//! Re-emit loses comments (accepted; no format-preserving YAML editor).

use super::cmdline::{ShellOverride, command_is_ours, shell_client_command};
use super::yaml_doc;
use ds_client::ClientSource;
use serde_json::{Map, Value, json};

/// No `shell`/`args` → inlined verbs + `--client`; spaced bin → 8.3.
fn hermes_command(bin: &str, verb: &str, client: ClientSource) -> String {
    shell_client_command(bin, verb, client, ShellOverride::Unsupported)
}

/// `(event, verb-fragment, timeout_secs)` — timeouts within Hermes 300 max.
const HERMES_HOOKS: &[(&str, &[(&str, i64)])] = &[
    ("on_session_start", &[("notify --greet-only", 30)]),
    ("pre_llm_call", &[("notify", 5), ("provide", 5)]),
    (
        "post_llm_call",
        &[("notify", super::SYNC_STOP_TIMEOUT_SECS)],
    ),
    ("on_session_finalize", &[("notify", 30)]),
];

fn parse_yaml(existing: &str) -> Result<Value, String> {
    yaml_doc::parse(existing).map_err(|e| format!("config.yaml is not valid YAML: {e}"))
}

fn emit_yaml(root: &Value) -> Result<String, String> {
    yaml_doc::emit(root).map_err(|e| format!("config.yaml serialize failed: {e}"))
}

fn unmergeable_shape(path: &str) -> String {
    format!("config.yaml has an unexpected `{path}` shape; left unchanged (Hermes hooks NOT wired)")
}

fn entry_command(entry: &Value) -> Option<&str> {
    entry.get("command").and_then(Value::as_str)
}

fn entry_is_ours(entry: &Value) -> bool {
    entry_command(entry).is_some_and(command_is_ours)
}

fn entry_timeout(entry: &Value) -> Option<i64> {
    entry.get("timeout").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
            .or_else(|| v.as_f64().map(|f| f as i64))
    })
}

fn hermes_entry(command: &str, timeout: i64) -> Value {
    json!({ "command": command, "timeout": timeout })
}

fn desired_entries(bin: &str, client: ClientSource) -> Vec<(String, String, i64)> {
    HERMES_HOOKS
        .iter()
        .flat_map(|(event, verbs)| {
            verbs.iter().map(move |(verb, timeout)| {
                (
                    (*event).to_string(),
                    hermes_command(bin, verb, client),
                    *timeout,
                )
            })
        })
        .collect()
}

/// Exact `(event, command)` pairs wired into `hooks:` — single SoT for allowlist too.
pub fn desired_hook_commands(bin: &str, client: ClientSource) -> Vec<(String, String)> {
    desired_entries(bin, client)
        .into_iter()
        .map(|(event, command, _)| (event, command))
        .collect()
}

/// Additive + idempotent + REPLACE-OURS. User entries never touched. Comment loss on re-emit.
pub fn merge_hermes_hooks(
    existing: &str,
    bin: &str,
    client: ClientSource,
) -> Result<String, String> {
    let mut root = parse_yaml(existing)?;
    if !root.is_object() {
        return Err(unmergeable_shape("root"));
    }
    let desired = desired_entries(bin, client);

    let obj = root.as_object_mut().expect("object checked above");
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        return Err(unmergeable_shape("hooks"));
    }
    let hooks_obj = hooks.as_object_mut().expect("object checked above");

    // Per-event ours lists (YAML map key order is not stable across re-emit).
    if ours_match_desired(hooks_obj, &desired) {
        return Ok(existing.to_string());
    }

    // Strip ours from every event list; drop empty event keys we emptied.
    let events: Vec<String> = hooks_obj.keys().cloned().collect();
    for event in events {
        if event == "output_spill" {
            continue;
        }
        let Some(entries) = hooks_obj.get_mut(&event) else {
            continue;
        };
        let Some(arr) = entries.as_array_mut() else {
            // Non-list event key we didn't write — leave alone.
            continue;
        };
        let before = arr.len();
        arr.retain(|e| !entry_is_ours(e));
        if arr.len() != before && arr.is_empty() {
            hooks_obj.remove(&event);
        }
    }

    // Append desired under their events (create lists as needed).
    for (event, command, timeout) in &desired {
        let list = hooks_obj
            .entry(event.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !list.is_array() {
            return Err(unmergeable_shape(&format!("hooks.{event}")));
        }
        list.as_array_mut()
            .expect("array checked above")
            .push(hermes_entry(command, *timeout));
    }

    emit_yaml(&root)
}

/// Compare ours per-event (map iteration order is not load-bearing).
fn ours_match_desired(hooks_obj: &Map<String, Value>, desired: &[(String, String, i64)]) -> bool {
    use std::collections::BTreeMap;
    let mut current: BTreeMap<&str, Vec<(&str, i64)>> = BTreeMap::new();
    for (event, entries) in hooks_obj {
        if event == "output_spill" {
            continue;
        }
        let Some(arr) = entries.as_array() else {
            continue;
        };
        for entry in arr {
            if entry_is_ours(entry) {
                current.entry(event.as_str()).or_default().push((
                    entry_command(entry).unwrap_or(""),
                    entry_timeout(entry).unwrap_or(-1),
                ));
            }
        }
    }
    let mut want: BTreeMap<&str, Vec<(&str, i64)>> = BTreeMap::new();
    for (event, command, timeout) in desired {
        want.entry(event.as_str())
            .or_default()
            .push((command.as_str(), *timeout));
    }
    current == want
}

/// Drop every DontSpeak hook entry; remove empty event keys and empty `hooks`.
pub fn strip_hermes_hooks(existing: &str) -> Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let mut root = parse_yaml(existing)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(existing.to_string());
    };
    let Some(hooks) = obj.get_mut("hooks") else {
        return Ok(existing.to_string());
    };
    let Some(hooks_obj) = hooks.as_object_mut() else {
        // Scalar/list `hooks` can't hold entries we wrote → leave alone.
        return Ok(existing.to_string());
    };
    let mut changed = false;
    let events: Vec<String> = hooks_obj.keys().cloned().collect();
    for event in events {
        if event == "output_spill" {
            continue;
        }
        let Some(entries) = hooks_obj.get_mut(&event) else {
            continue;
        };
        let Some(arr) = entries.as_array_mut() else {
            continue;
        };
        let before = arr.len();
        arr.retain(|e| !entry_is_ours(e));
        changed |= arr.len() != before;
        if arr.len() != before && arr.is_empty() {
            hooks_obj.remove(&event);
        }
    }
    if !changed {
        return Ok(existing.to_string());
    }
    if hooks_obj.is_empty() {
        obj.remove("hooks");
    }
    emit_yaml(&root)
}

#[cfg(test)]
mod tests {
    use super::super::cmdline::{InlineFlavor, ShellOverride, inline_command};
    use super::*;

    const BIN: &str = "/home/u/.local/bin/dontspeak";

    fn merged(existing: &str) -> String {
        merge_hermes_hooks(existing, BIN, ClientSource::Hermes).expect("merge ok")
    }

    fn cmd(verb: &str) -> String {
        hermes_command(BIN, verb, ClientSource::Hermes)
    }

    fn parse(s: &str) -> Value {
        parse_yaml(s).expect("yaml")
    }

    /// Ours entries as `(event, command, timeout)` in `HERMES_HOOKS` event order.
    fn our_entries(root: &Value) -> Vec<(String, String, i64)> {
        let hooks = root["hooks"].as_object().expect("hooks object");
        let mut out = Vec::new();
        for (event, _) in HERMES_HOOKS {
            let Some(arr) = hooks.get(*event).and_then(Value::as_array) else {
                continue;
            };
            for entry in arr {
                if entry_is_ours(entry) {
                    let keys: Vec<&str> = entry
                        .as_object()
                        .expect("entry object")
                        .keys()
                        .map(|k| k.as_str())
                        .collect();
                    assert!(
                        keys.contains(&"command") && keys.contains(&"timeout"),
                        "entry needs command+timeout: {entry}"
                    );
                    out.push((
                        (*event).to_string(),
                        entry_command(entry).unwrap().to_string(),
                        entry_timeout(entry).expect("timeout"),
                    ));
                }
            }
        }
        out
    }

    fn expected() -> Vec<(String, String, i64)> {
        desired_entries(BIN, ClientSource::Hermes)
    }

    #[test]
    fn merge_into_empty_wires_all_events_with_exact_timeouts() {
        let out = merged("");
        let root = parse(&out);
        assert_eq!(
            our_entries(&root),
            vec![
                ("on_session_start".into(), cmd("notify --greet-only"), 30),
                ("pre_llm_call".into(), cmd("notify"), 5),
                ("pre_llm_call".into(), cmd("provide"), 5),
                ("post_llm_call".into(), cmd("notify"), 60),
                ("on_session_finalize".into(), cmd("notify"), 30),
            ]
        );
        let ss = &our_entries(&root)[0];
        assert!(
            ss.1.contains("notify --greet-only --client hermes"),
            "greet-only SessionStart: {}",
            ss.1
        );
        let stop = &our_entries(&root)[3];
        assert!(
            stop.1.contains("notify --client hermes"),
            "Stop notify: {}",
            stop.1
        );
        assert!(!stop.1.contains("--greet-only"));
        // Hermes max is 300; our Stop is 60.
        assert!(our_entries(&root).iter().all(|(_, _, t)| *t <= 300));
    }

    #[test]
    fn merge_preserves_unrelated_hooks_and_config_and_is_idempotent() {
        let existing = r#"
theme: dark
hooks:
  post_tool_call:
    - command: /usr/bin/true
      timeout: 10
"#;
        let once = merged(existing);
        assert!(once.contains("theme"), "unrelated key preserved: {once}");
        assert!(
            once.contains("/usr/bin/true"),
            "user's hook preserved: {once}"
        );
        let twice = merged(&once);
        assert_eq!(once, twice, "idempotent");
        assert_eq!(our_entries(&parse(&twice)), expected(), "no duplicates");

        let commented = format!("# keep this comment\n{once}");
        assert_eq!(
            merged(&commented),
            commented,
            "semantic no-op preserves bytes"
        );
    }

    #[test]
    fn user_hook_containing_dontspeak_substring_is_not_misidentified() {
        let existing = r#"
hooks:
  post_llm_call:
    - command: /home/u/bin/my-dontspeak-checker
      timeout: 5
"#;
        let out = merged(existing);
        assert!(
            out.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives merge: {out}"
        );
        assert_eq!(our_entries(&parse(&out)), expected());

        let stripped = strip_hermes_hooks(&out).unwrap();
        assert!(
            stripped.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives strip: {stripped}"
        );
        assert!(our_entries(&parse(&stripped)).is_empty(), "ours removed");
    }

    #[test]
    fn rewire_heals_a_changed_binary_path_by_replacing_stale_entries() {
        let first = merged("");
        let new_bin = "/opt/dontspeak/bin/dontspeak";
        let second = merge_hermes_hooks(&first, new_bin, ClientSource::Hermes).expect("merge ok");
        assert!(!second.contains(BIN), "stale bin path healed away");
        assert!(second.contains(new_bin), "re-wire re-points the command");
        assert_eq!(
            our_entries(&parse(&second)),
            desired_entries(new_bin, ClientSource::Hermes),
            "stale entries replaced, not duplicated"
        );
    }

    #[test]
    fn legacy_quoted_entry_is_still_ours_so_strip_removes_it_and_merge_heals_it() {
        let legacy = format!(
            "hooks:\n  post_llm_call:\n    - command: \"\\\"{BIN}\\\" notify\"\n      timeout: 60\n"
        );
        let stripped = strip_hermes_hooks(&legacy).expect("strip ok");
        assert!(
            !stripped.contains("dontspeak"),
            "legacy quoted entry removed on unwire, got: {stripped}"
        );
        assert_eq!(our_entries(&parse(&merged(&legacy))), expected());
    }

    #[test]
    fn strip_removes_only_ours_and_drops_empty_hooks() {
        let stripped = strip_hermes_hooks(&merged(
            "hooks:\n  post_tool_call:\n    - command: /usr/bin/true\n      timeout: 10\n",
        ))
        .unwrap();
        assert!(stripped.contains("/usr/bin/true"), "user hook kept");
        assert!(!stripped.contains("dontspeak"), "all ours removed");
        let root = parse(&stripped);
        assert!(root["hooks"]["post_tool_call"].as_array().unwrap().len() == 1);

        let stripped = strip_hermes_hooks(&merged("")).unwrap();
        assert!(
            root_missing_hooks(&stripped),
            "empty hooks dropped: {stripped}"
        );
    }

    fn root_missing_hooks(s: &str) -> bool {
        let root = parse(s);
        root.get("hooks").is_none()
    }

    #[test]
    fn strip_on_empty_input_is_a_noop() {
        assert_eq!(strip_hermes_hooks("").unwrap(), "");
    }

    #[test]
    fn strip_without_our_hooks_preserves_bytes() {
        let existing = "# keep\nhooks:\n  post_tool_call: [{command: /usr/bin/true}]\n";
        assert_eq!(strip_hermes_hooks(existing).unwrap(), existing);
    }

    #[test]
    fn unmergeable_scalar_hooks_errors() {
        let bad = "hooks: oops\n";
        let err = merge_hermes_hooks(bad, BIN, ClientSource::Hermes).unwrap_err();
        assert!(err.contains("unexpected `hooks` shape"), "{err}");
    }

    #[test]
    fn parse_error_surfaces() {
        let bad = "hooks: [\n  - :\n";
        let err = merge_hermes_hooks(bad, BIN, ClientSource::Hermes).unwrap_err();
        assert!(err.contains("not valid YAML"), "{err}");
    }

    #[test]
    fn windows_inline_flavor_is_unquoted_for_spaceless_bin() {
        // Hermes shlex.splits the command string; Windows spaceless form must stay quote-free.
        let bin = r"C:\Users\usr\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        let (cmd, shell) = inline_command(
            InlineFlavor::Windows,
            bin,
            ["notify", "--client", ClientSource::Hermes.as_str()],
            ShellOverride::Unsupported,
        );
        assert_eq!(
            cmd,
            "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe notify --client hermes"
        );
        assert!(!cmd.contains('"'), "no quote may reach Hermes shlex: {cmd}");
        assert_eq!(shell, None);
    }
}
