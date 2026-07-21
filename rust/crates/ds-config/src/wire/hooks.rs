//! Claude-contract JSON hooks — Claude Code (`ArgsArray`) and Qwen (`InlineShell`;
//! [`HookCommandStyle`]). Pure merge/strip; disk owned by the wire orchestrator.
//! Additive + idempotent + replace-ours.

use super::cmdline::{ShellOverride, command_is_ours, host_inline_flavor, inline_command};
use super::registry::HookCommandStyle;
use ds_client::ClientSource;
use serde_json::{Map, Value, json};

/// Inputs for [`merge_hooks`]. Caller owns path formatting (incl. `.exe`).
pub struct HookSpec<'a> {
    /// Absolute `dontspeak[.exe]`. Verbs via `args` ([`HookCommandStyle::ArgsArray`]) or
    /// inlined ([`HookCommandStyle::InlineShell`]): `notify` (async sink) / `provide` (sync query).
    pub bin: &'a str,
    /// Optional `preferredNotifChannel` (e.g. `"iterm2_with_bell"`).
    pub notif_channel: Option<&'a str>,
    /// `true` → wire `MessageDisplay` (per-batch). `false` → voice whole reply from `Stop`.
    pub streaming: bool,
    /// ArgsArray: spawn `command`+`args`, timeout SECONDS. InlineShell: shell `command` only,
    /// timeout MILLISECONDS (Qwen).
    pub command_style: HookCommandStyle,
    /// Trailing `--client <token>` on every entry — shapers stay client-agnostic.
    pub client: ClientSource,
}

/// Group is ours if any command is our `dontspeak` binary (idempotent merge + strip).
fn hook_group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(command_is_ours)
            })
        })
}

// Command-string dialect: shared `wire::cmdline` (Qwen/Codex must not drift).

/// One entry for `spec.command_style`: ArgsArray → `args` + seconds; InlineShell → inlined
/// verbs + ms (`timeout_secs * 1000`; unscaled `5` would be 5 ms). Zero omits `timeout`.
/// Always appends `--client <token>` (snake_case → Windows quote-free by construction).
fn hook_entry(spec: &HookSpec, verbs: &[&str], timeout_secs: u64, is_async: bool) -> Value {
    let mut h = match spec.command_style {
        HookCommandStyle::ArgsArray => {
            let mut entry = json!({ "type": "command", "command": spec.bin });
            entry["args"] = Value::Array(
                verbs
                    .iter()
                    .copied()
                    .chain(["--client", spec.client.as_str()])
                    .map(Value::from)
                    .collect(),
            );
            entry
        }
        HookCommandStyle::InlineShell => {
            // Qwen has `shell` → pin spaced path to PowerShell (vs Codex 8.3 short name).
            let (cmd, shell) = inline_command(
                host_inline_flavor(),
                spec.bin,
                verbs
                    .iter()
                    .copied()
                    .chain(["--client", spec.client.as_str()]),
                ShellOverride::Supported,
            );
            let mut v = json!({ "type": "command", "command": cmd });
            if let Some(sh) = shell {
                v["shell"] = json!(sh);
            }
            v
        }
    };
    if is_async {
        h["async"] = json!(true);
    }
    if timeout_secs > 0 {
        h["timeout"] = match spec.command_style {
            HookCommandStyle::ArgsArray => json!(timeout_secs),
            HookCommandStyle::InlineShell => json!(timeout_secs * 1000),
        };
    }
    h
}

/// Canonical `(event, group)` set — one definition every installer uses.
fn canonical_hook_groups(spec: &HookSpec) -> Vec<(&'static str, Value)> {
    // Same binary, two verbs: `notify` async sink (routes on `hook_event_name`);
    // `provide` sync query (`hookSpecificOutput` — async would drop stdout).
    let notify = |timeout: u64| hook_entry(spec, &["notify"], timeout, true);
    // One group/event. MessageDisplay = streaming only; else whole reply from Stop.
    // SessionStart greet (+ stream-witness seed when streaming); SessionEnd barge;
    // UserPromptSubmit mark-active + sync provide; Stop/Notification earcons.
    let mut groups: Vec<(&'static str, Value)> = Vec::new();
    if spec.streaming {
        groups.push(("MessageDisplay", json!({ "hooks": [ notify(10) ] })));
    }
    // SessionStart: async notify only (voice greet; CC 2.1+ drops SessionStart stdout).
    // Streaming → plain `notify` (greet + witness seed); non-streaming → `--greet-only`
    // (seed would mark session "already narrated" and silence Stop).
    let session_start = if spec.streaming {
        notify(0)
    } else {
        hook_entry(spec, &["notify", "--greet-only"], 0, true)
    };
    groups.push(("SessionStart", json!({ "hooks": [ session_start ] })));
    groups.push(("SessionEnd", json!({ "hooks": [ notify(0) ] })));
    groups.push(("Stop", json!({ "hooks": [ notify(0) ] })));
    groups.push(("Notification", json!({ "hooks": [ notify(0) ] })));
    groups.push((
        "UserPromptSubmit",
        json!({ "hooks": [
            notify(5),
            hook_entry(spec, &["provide"], 5, false) ] }),
    ));
    groups
}

/// [`merge_hooks`] failure. Mirrors `CodexMergeError`: report, leave document untouched (pure).
#[derive(Debug)]
pub enum HooksMergeError {
    /// `hooks.<Event>` exists but is not an array. `String` = path (`hooks.<Event>`).
    UnmergeableShape(String),
}

impl std::fmt::Display for HooksMergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HooksMergeError::UnmergeableShape(s) => write!(
                f,
                "settings.json has an unexpected `{s}` shape; left unchanged (DontSpeak hooks NOT wired)"
            ),
        }
    }
}

impl std::error::Error for HooksMergeError {}

/// Merge canonical hooks into parsed `settings.json`. Pure; preserves foreign keys.
/// Replace-ours + idempotent (self-heals stale bin path / verb shape on re-wire).
/// Non-array `hooks.<Event>` → [`HooksMergeError::UnmergeableShape`].
/// Drops stale top-level `dontspeak` block; leaves CC `voice` read-only.
/// `preferredNotifChannel` is updated (not get-or-create) on re-wire.
pub fn merge_hooks(mut root: Value, spec: &HookSpec) -> Result<Value, HooksMergeError> {
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    let obj = root.as_object_mut().expect("coerced to object above");
    {
        let hooks = obj
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()));
        if !hooks.is_object() {
            *hooks = Value::Object(Map::new());
        }
        let hooks = hooks.as_object_mut().expect("coerced to object above");
        for (evt, group) in canonical_hook_groups(spec) {
            match hooks.get_mut(evt) {
                None => {
                    hooks.insert(evt.to_string(), Value::Array(vec![group]));
                }
                Some(slot) => {
                    let Some(arr) = slot.as_array_mut() else {
                        return Err(HooksMergeError::UnmergeableShape(format!("hooks.{evt}")));
                    };
                    // Basename match may hit a stale absolute path — replace, don't keep.
                    arr.retain(|g| !hook_group_is_ours(g));
                    arr.push(group);
                }
            }
        }
    }
    obj.remove("dontspeak");
    if let Some(ch) = spec.notif_channel {
        obj.insert("preferredNotifChannel".to_string(), json!(ch));
    }
    Ok(root)
}

/// Strip our groups; prune empty events / empty `hooks` / `preferredNotifChannel`.
/// Leaves CC `voice` and foreign keys.
pub fn strip_hooks(mut root: Value) -> Value {
    if let Some(obj) = root.as_object_mut() {
        let mut hooks_now_empty = false;
        if let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            let events: Vec<String> = hooks.keys().cloned().collect();
            for evt in events {
                if let Some(arr) = hooks.get_mut(&evt).and_then(|v| v.as_array_mut()) {
                    arr.retain(|g| !hook_group_is_ours(g));
                    if arr.is_empty() {
                        hooks.remove(&evt);
                    }
                }
            }
            hooks_now_empty = hooks.is_empty();
        }
        if hooks_now_empty {
            obj.remove("hooks");
        }
        obj.remove("preferredNotifChannel");
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code: ArgsArray + MessageDisplay.
    fn spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::ClaudeCode,
        }
    }

    /// Qwen: InlineShell + MessageDisplay.
    fn inline_spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
            command_style: HookCommandStyle::InlineShell,
            client: ClientSource::QwenCode,
        }
    }

    fn merged(root: Value) -> Value {
        merge_hooks(root, &spec()).expect("merge ok")
    }

    /// ArgsArray `args` including the uniform `--client` tail (forces assertions to cover it).
    fn args(client: ClientSource, verbs: &[&str]) -> Value {
        let mut v = verbs.to_vec();
        v.extend_from_slice(&["--client", client.as_str()]);
        json!(v)
    }

    /// InlineShell command suffix for `verbs` + `--client`.
    fn tail(client: ClientSource, verbs: &str) -> String {
        format!(" {verbs} --client {}", client.as_str())
    }

    #[test]
    fn merge_hooks_is_additive_and_idempotent() {
        let root = json!({
            "model": "opus",
            "hooks": { "MessageDisplay": [ { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ] }
        });
        let once = merged(root);
        assert_eq!(once["model"], json!("opus"), "unrelated key preserved");
        assert_eq!(
            once["hooks"]["MessageDisplay"].as_array().unwrap().len(),
            2,
            "user + ours"
        );
        let twice = merged(once.clone());
        assert_eq!(
            twice["hooks"]["MessageDisplay"].as_array().unwrap().len(),
            2,
            "idempotent"
        );
        assert_eq!(twice, once, "second merge is a no-op");
    }

    #[test]
    fn merge_hooks_replaces_our_group_with_a_stale_bin_path() {
        // Self-heal: re-wire replaces basename-matched group with stale path / pre-token args.
        let stale = json!({
            "hooks": { "MessageDisplay": [
                { "hooks": [ { "type": "command", "command": "/home/u/.local/bin/dontspeak",
                               "args": ["notify"], "async": true, "timeout": 10 } ] },
                { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] }
            ] }
        });
        let out = merged(stale);
        let md = out["hooks"]["MessageDisplay"].as_array().unwrap();
        assert_eq!(md.len(), 2, "user group kept, ours replaced not duplicated");
        let ours: Vec<&Value> = md.iter().filter(|g| hook_group_is_ours(g)).collect();
        assert_eq!(ours.len(), 1, "exactly one group of ours after re-wire");
        assert_eq!(
            ours[0]["hooks"][0]["command"],
            json!("/bin/dontspeak"),
            "stale bin path healed to the resolved one"
        );
        assert_eq!(
            ours[0]["hooks"][0]["args"],
            args(ClientSource::ClaudeCode, &["notify"]),
            "client-less verbs healed to the token-carrying shape"
        );
    }

    #[test]
    fn merge_hooks_strips_stale_ds_block() {
        let root = json!({ "dontspeak": { "voice": "am_adam", "custom": 1 }, "model": "opus" });
        let out = merged(root);
        assert!(
            out.get("dontspeak").is_none(),
            "stale dontspeak block removed"
        );
        assert_eq!(out["model"], json!("opus"), "unrelated key preserved");
    }

    #[test]
    fn merge_hooks_omits_ds_and_never_writes_cc_voice() {
        let out = merged(json!({}));
        assert!(out.get("dontspeak").is_none());
        assert!(
            out.get("voice").is_none(),
            "CC voice block is not written by wiring"
        );
    }

    #[test]
    fn merge_hooks_leaves_user_voice_block_untouched() {
        // CC `voice` is read-only for wiring.
        let voice = json!({ "enabled": false, "mode": "hold", "autoSubmit": true });
        let root = json!({ "voice": voice.clone() });
        let out = merged(root);
        assert_eq!(
            out["voice"], voice,
            "CC voice block preserved verbatim (never written)"
        );
    }

    #[test]
    fn strip_hooks_removes_only_ours() {
        let wired = merged(
            json!({ "hooks": { "MessageDisplay": [ { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ] } }),
        );
        let stripped = strip_hooks(wired);
        let md = stripped["hooks"]["MessageDisplay"].as_array().unwrap();
        assert_eq!(md.len(), 1, "user hook kept");
        assert_eq!(md[0]["hooks"][0]["command"], json!("/usr/bin/true"));
        assert!(
            stripped["hooks"].get("SessionStart").is_none(),
            "ours-only event removed"
        );
    }

    #[test]
    fn merge_hooks_wires_sessionstart_notify_cross_platform() {
        let out = merged(json!({}));
        let ss = out["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart wired");
        assert_eq!(ss.len(), 1);
        assert!(
            ss[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak")
        );
        assert_eq!(
            ss[0]["hooks"][0]["args"],
            args(ClientSource::ClaudeCode, &["notify"])
        );
        let twice = merged(out.clone());
        assert_eq!(
            twice["hooks"]["SessionStart"].as_array().unwrap().len(),
            1,
            "idempotent"
        );
        let stripped = strip_hooks(out);
        assert!(
            stripped["hooks"].get("SessionStart").is_none(),
            "notify hook stripped on uninstall"
        );
    }

    #[test]
    fn provide_query_is_sync_on_userpromptsubmit() {
        // `provide` must stay sync — async drops stdout and kills narration context.
        let out = merged(json!({}));
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .clone();
        let notify = ups
            .iter()
            .find(|h| h["args"] == args(ClientSource::ClaudeCode, &["notify"]))
            .expect("notify sink wired on UserPromptSubmit");
        assert_eq!(notify["async"], json!(true), "notify is fire-and-forget");
        let provide = ups
            .iter()
            .find(|h| h["args"] == args(ClientSource::ClaudeCode, &["provide"]))
            .expect("provide query wired on UserPromptSubmit");
        assert!(
            provide.get("async").is_none(),
            "provide must not be async (its stdout is read)"
        );
        assert!(provide["command"].as_str().unwrap().contains("dontspeak"));
        assert!(strip_hooks(out)["hooks"].get("UserPromptSubmit").is_none());
    }

    #[test]
    fn non_streaming_client_omits_messagedisplay_keeps_stop_provide() {
        let spec_ns = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: false,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::QwenCode,
        };
        let out = merge_hooks(json!({}), &spec_ns).expect("merge ok");
        assert!(
            out["hooks"].get("MessageDisplay").is_none(),
            "MessageDisplay must NOT be wired for a non-streaming client"
        );
        for evt in [
            "SessionStart",
            "SessionEnd",
            "Stop",
            "Notification",
            "UserPromptSubmit",
        ] {
            assert!(
                out["hooks"].get(evt).is_some(),
                "{evt} wired for non-streaming client"
            );
        }
        assert_eq!(
            out["hooks"]["Stop"][0]["hooks"][0]["args"],
            args(ClientSource::QwenCode, &["notify"]),
            "Stop notify sink present"
        );
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        assert!(
            ups.iter()
                .any(|h| h["args"] == args(ClientSource::QwenCode, &["provide"]))
        );
    }

    #[test]
    fn merge_hooks_wires_stop_and_notification_earcon_events() {
        let out = merged(json!({}));
        for evt in ["Stop", "Notification"] {
            let g = out["hooks"][evt]
                .as_array()
                .unwrap_or_else(|| panic!("{evt} wired"));
            assert_eq!(g.len(), 1, "{evt} is a single notify group");
            assert_eq!(
                g[0]["hooks"][0]["args"],
                args(ClientSource::ClaudeCode, &["notify"]),
                "{evt} is notify-only"
            );
            assert_eq!(
                g[0]["hooks"][0]["async"],
                json!(true),
                "{evt} never blocks Claude"
            );
        }
        let twice = merged(out.clone());
        assert_eq!(twice, out, "second merge is a no-op");
        let stripped = strip_hooks(out);
        assert!(
            stripped["hooks"].get("Stop").is_none(),
            "Stop stripped on uninstall"
        );
        assert!(
            stripped["hooks"].get("Notification").is_none(),
            "Notification stripped on uninstall"
        );
    }

    #[test]
    fn merge_hooks_updates_preferred_notif_channel_on_rewire() {
        // Update (not get-or-create) so channel changes reach existing installs.
        let spec_a = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("iterm2_with_bell"),
            streaming: true,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::ClaudeCode,
        };
        let once = merge_hooks(json!({}), &spec_a).expect("merge ok");
        assert_eq!(once["preferredNotifChannel"], json!("iterm2_with_bell"));

        let spec_b = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("other_channel"),
            streaming: true,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::ClaudeCode,
        };
        let twice = merge_hooks(once, &spec_b).expect("merge ok");
        assert_eq!(
            twice["preferredNotifChannel"],
            json!("other_channel"),
            "re-wire updates a changed channel, not stuck on the first value"
        );
    }

    #[test]
    fn strip_hooks_removes_preferred_notif_channel_and_empty_hooks_scaffold() {
        // Strip undoes merge's scaffold + preferredNotifChannel exactly.
        let spec_with_channel = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("iterm2_with_bell"),
            streaming: true,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::ClaudeCode,
        };
        let wired = merge_hooks(json!({ "model": "opus" }), &spec_with_channel).expect("merge ok");
        assert!(wired.get("preferredNotifChannel").is_some());

        let stripped = strip_hooks(wired);
        assert!(
            stripped.get("preferredNotifChannel").is_none(),
            "preferredNotifChannel removed on strip"
        );
        assert!(
            stripped.get("hooks").is_none(),
            "emptied hooks scaffold pruned, not left behind as `hooks: {{}}`"
        );
        assert_eq!(stripped["model"], json!("opus"), "unrelated key untouched");
    }

    #[test]
    fn merge_hooks_rejects_non_array_event_slot_without_clobbering() {
        let root = json!({ "hooks": { "MessageDisplay": { "not": "an array" } } });
        let err = merge_hooks(root, &spec()).expect_err("non-array hooks.<Event> must error");
        assert!(
            matches!(&err, HooksMergeError::UnmergeableShape(s) if s.contains("MessageDisplay")),
            "error names the offending path: {err}"
        );
    }

    // InlineShell (Qwen): shell-only `command`, no `args`, timeout ms. Command strings /
    // `command_is_ours` pinned in `wire::cmdline`.

    #[test]
    fn inline_shell_entries_have_no_args_key_and_millisecond_timeouts() {
        // Qwen drops `args` silently; pin inlined verbs + s→ms timeouts end-to-end.
        let out = merge_hooks(json!({}), &inline_spec()).expect("merge ok");
        let hooks = out["hooks"].as_object().expect("hooks object");
        assert!(
            hooks.get("MessageDisplay").is_some(),
            "streaming hook present"
        );
        for (evt, groups) in hooks {
            for g in groups.as_array().expect("array of groups") {
                for h in g["hooks"].as_array().expect("hooks array") {
                    assert!(
                        h.get("args").is_none(),
                        "{evt}: Qwen has no `args` field — verbs must be inlined, got {h}"
                    );
                    let cmd = h["command"].as_str().expect("command string");
                    assert!(cmd.contains("dontspeak"), "{evt}: our binary, got {cmd}");
                    assert!(
                        cmd.contains(" notify") || cmd.contains(" provide"),
                        "{evt}: verb inlined into the command string, got {cmd}"
                    );
                }
            }
        }
        let ss = out["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            ss.ends_with(&tail(ClientSource::QwenCode, "notify")),
            "streaming SessionStart carries the client token without greet-only, got {ss}"
        );
        for evt in ["SessionStart", "SessionEnd", "Stop", "Notification"] {
            let h = &out["hooks"][evt][0]["hooks"][0];
            assert!(h.get("timeout").is_none(), "{evt}: no timeout field");
            assert_eq!(h["async"], json!(true), "{evt}: async notify sink");
        }
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        let notify = ups
            .iter()
            .find(|h| {
                h["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(&tail(ClientSource::QwenCode, "notify"))
            })
            .expect("notify entry");
        assert_eq!(notify["timeout"], json!(5000), "seconds scaled to ms");
        assert_eq!(notify["async"], json!(true));
        let provide = ups
            .iter()
            .find(|h| {
                h["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(&tail(ClientSource::QwenCode, "provide"))
            })
            .expect("provide entry");
        assert_eq!(provide["timeout"], json!(5000), "seconds scaled to ms");
        assert!(
            provide.get("async").is_none(),
            "provide must not be async (its stdout is read)"
        );
    }

    #[test]
    fn args_array_entries_keep_second_timeouts() {
        // ArgsArray timeout stays seconds; ms scale is InlineShell-only.
        let out = merged(json!({}));
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        for verb in ["notify", "provide"] {
            let h = ups
                .iter()
                .find(|h| h["args"] == args(ClientSource::ClaudeCode, &[verb]))
                .expect("entry for verb");
            assert_eq!(
                h["timeout"],
                json!(5),
                "{verb}: args-array timeout stays in seconds"
            );
        }
    }

    #[test]
    fn args_array_sessionstart_is_greet_only_iff_non_streaming() {
        let streaming = merged(json!({}));
        assert_eq!(
            streaming["hooks"]["SessionStart"][0]["hooks"][0]["args"],
            args(ClientSource::ClaudeCode, &["notify"]),
            "streaming SessionStart seeds the witness with plain notify"
        );
        let spec_ns = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: false,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::QwenCode,
        };
        let non_streaming = merge_hooks(json!({}), &spec_ns).expect("merge ok");
        assert_eq!(
            non_streaming["hooks"]["SessionStart"][0]["hooks"][0]["args"],
            args(ClientSource::QwenCode, &["notify", "--greet-only"]),
            "non-streaming SessionStart must skip the witness seed"
        );
    }

    #[test]
    fn inline_streaming_wires_messagedisplay_with_ms_timeout_and_plain_sessionstart() {
        // Streaming InlineShell: MessageDisplay inlined + ms timeout; SessionStart plain notify.
        let spec_streaming = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
            command_style: HookCommandStyle::InlineShell,
            client: ClientSource::QwenCode,
        };
        let out = merge_hooks(json!({}), &spec_streaming).expect("merge ok");
        let md = &out["hooks"]["MessageDisplay"][0]["hooks"][0];
        assert!(
            md["command"]
                .as_str()
                .unwrap()
                .ends_with(&tail(ClientSource::QwenCode, "notify")),
            "MessageDisplay carries the inlined notify verb + client token, got {md}"
        );
        assert!(
            md.get("args").is_none(),
            "no `args` key in the inline dialect"
        );
        assert_eq!(md["timeout"], json!(10_000), "10 s scaled to 10000 ms");
        assert_eq!(md["async"], json!(true), "fire-and-forget notify sink");
        let ss = out["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            ss.ends_with(&tail(ClientSource::QwenCode, "notify")) && !ss.contains("--greet-only"),
            "streaming SessionStart seeds the witness with plain notify, got {ss}"
        );
    }

    #[test]
    fn inline_merge_is_idempotent_and_strips_clean() {
        let once = merge_hooks(json!({}), &inline_spec()).expect("merge ok");
        let twice = merge_hooks(once.clone(), &inline_spec()).expect("merge ok");
        assert_eq!(twice, once, "second inline merge is a no-op");
        let stripped = strip_hooks(once);
        assert!(
            stripped.get("hooks").is_none(),
            "inlined groups recognized as ours and stripped clean"
        );
    }

    #[test]
    fn rewire_self_heals_a_stale_args_array_group_to_the_inlined_shape() {
        // Broken Qwen wire left bare+args groups; re-wire replaces with inlined shape.
        let stale = json!({
            "hooks": { "Stop": [
                { "hooks": [ { "type": "command", "command": "/bin/dontspeak",
                               "args": ["notify"], "async": true } ] },
                { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] }
            ] }
        });
        let out = merge_hooks(stale, &inline_spec()).expect("merge ok");
        let stop = out["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            stop.len(),
            2,
            "user group kept, ours replaced not duplicated"
        );
        let ours: Vec<&Value> = stop.iter().filter(|g| hook_group_is_ours(g)).collect();
        assert_eq!(ours.len(), 1, "exactly one group of ours after re-wire");
        let healed = &ours[0]["hooks"][0];
        assert!(
            healed.get("args").is_none(),
            "healed to the inlined shape (no args), got {healed}"
        );
        assert!(
            healed["command"]
                .as_str()
                .unwrap()
                .ends_with(&tail(ClientSource::QwenCode, "notify")),
            "verb now inlined in the command string, carrying the client token"
        );
    }
}
