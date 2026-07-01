//! Claude Code hook wiring — the SINGLE cross-platform source of truth for the
//! DontSpeak voice hooks in ~/.claude/settings.json. Replaces the old per-platform
//! copies (macOS claude/settings.snippet.json, Windows settings.snippet.json,
//! linux/settings.snippet.json), which had drifted. PURE merge/strip here (no disk); the
//! `dontspeak wire claude_code` orchestrator (via `wire_hooks::claude_code_hooks`) owns path
//! resolution, backup, and the atomic write. Mirrors `merge_settings`'
//! coerce-to-object / get-or-create / additive discipline so unrelated keys (Claude
//! Code's own hooks, permissions, model) are never clobbered.

use serde_json::{Map, Value, json};

/// The base names (no extension) of every executable DontSpeak installs into a binary
/// directory it controls TODAY — the CURRENT set, used by the installers' stale-binary
/// cleanup as the "keep" list. `dontspeak wire`'s housekeeping prunes any `dontspeak*` executable in
/// the install dir that is NOT in this set (see its `prune_stale_bins`); the Windows Inno
/// `[InstallDelete]` mirrors it (it must enumerate names declaratively, pre-copy, in the
/// ELEVATED context — it can't call into this user-context binary to clear `{app}` under
/// Program Files). Keep the two in sync.
///
/// Any `dontspeak*` executable in the install dir that is NOT in this set is pruned as an
/// orphan, so this is the single keep-list both the prune and the Inno `[InstallDelete]`
/// honor.
pub const INSTALLED_BINS: &[&str] = &["dontspeak", "dontspeakd", "ds-helper", "ds-winui"];

/// Inputs for [`merge_hooks`]: the resolved hook command path + voice prefs. All `&str`
/// so the caller owns path formatting (incl. the platform `.exe` suffix).
pub struct HookSpec<'a> {
    /// Absolute path to the single `dontspeak[.exe]` multi-call binary. Every hook is this
    /// one binary with a different `args` head (`notify` for the async sinks, `provide` for
    /// the synchronous narration-spec query).
    pub bin: &'a str,
    /// Optional `preferredNotifChannel` (e.g. macOS iTerm's `"iterm2_with_bell"`).
    pub notif_channel: Option<&'a str>,
}

/// The basename (no extension) of the command we install — the single `dontspeak` binary.
fn command_is_ours(cmd: &str) -> bool {
    std::path::Path::new(cmd)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem == "dontspeak")
}

/// A hook group is "ours" if any of its commands is our `dontspeak` binary — used for
/// idempotent merge + clean removal.
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

/// The canonical `(event, group)` hook set in settings.json shape — the ONE
/// definition every platform installs.
fn canonical_hook_groups(spec: &HookSpec) -> Vec<(&'static str, Value)> {
    // EVERY hook is the SAME binary with ONE of two verbs, split by contract (see hook_core):
    //   `notify`  — COMMAND sink, ASYNC fire-and-forget, replies with nothing. The binary
    //               routes on the payload's `hook_event_name`, so the wiring is uniform — only
    //               the event list + per-entry flags differ, never the command.
    //   `provide` — QUERY, SYNCHRONOUS, returns the `hookSpecificOutput` JSON. The ONE verb
    //               Claude Code waits on (an async run would drop the output → no context).
    let notify = |timeout: u64| {
        let mut h =
            json!({ "type": "command", "command": spec.bin, "args": ["notify"], "async": true });
        if timeout > 0 {
            h["timeout"] = json!(timeout);
        }
        h
    };
    // One group per event (ours, so merge stays idempotent + strip stays clean). `notify` on
    // every fire-and-forget event: MessageDisplay is the SINGLE narration pipeline (Claude Code
    // ≥ 2.1.x streams it per batch); SessionStart greets; SessionEnd barges this window's
    // playback; UserPromptSubmit marks THIS terminal active so narration follows it. The
    // UserPromptSubmit group ALSO carries the synchronous `provide` (the narration spec as
    // `additionalContext`) — two interaction kinds on one event, in one group.
    vec![
        ("MessageDisplay", json!({ "hooks": [ notify(10) ] })),
        // SessionStart is async-notify ONLY: the engine voice greet + streaming-witness seed,
        // off the critical path. The greeting is voice-only — there is no visible banner, so no
        // synchronous `provide` twin (CC 2.1+ drops a SessionStart hook's stdout anyway).
        ("SessionStart", json!({ "hooks": [ notify(0) ] })),
        ("SessionEnd", json!({ "hooks": [ notify(0) ] })),
        // Stop fires once when Claude finishes a turn → the reply "ding" earcon. Notification
        // fires on a permission prompt / idle → the needs-input earcon. Both are async notify
        // sinks (never block Claude); the binary routes them in `hook_core` and self-gates on
        // `earcon_enabled` / notification_type.
        ("Stop", json!({ "hooks": [ notify(0) ] })),
        ("Notification", json!({ "hooks": [ notify(0) ] })),
        (
            "UserPromptSubmit",
            json!({ "hooks": [
                notify(5),
                { "type": "command", "command": spec.bin, "args": ["provide"], "timeout": 5 } ] }),
        ),
    ]
}

/// Merge the canonical DontSpeak hooks into a parsed `settings.json`, PRESERVING every
/// other key. ADDITIVE + idempotent: per event, if one of our groups is already
/// present we leave it (re-running never duplicates); other hooks on that event
/// survive. The `voice` block is only CREATED when absent (never overrides an
/// existing mode). PURE — no disk. (Not self-healing: a hook-shape change like
/// dropping SessionStart's `provide` banner reaches existing installs via a clean
/// re-wire — `wire claude_code --remove` then `wire claude_code` — not an in-place merge.)
pub fn merge_hooks(mut root: Value, spec: &HookSpec) -> Value {
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
                    if !slot.is_array() {
                        *slot = Value::Array(Vec::new());
                    }
                    let arr = slot.as_array_mut().expect("coerced to array above");
                    if arr.iter().any(hook_group_is_ours) {
                        continue; // already wired — idempotent
                    }
                    arr.push(group);
                }
            }
        }
    }
    // Our own config now lives in `our config.toml`, NOT here. Drop any stale
    // `dontspeak` block a previous version seeded into settings.json so the file stays
    // purely Claude Code's (hooks + its `voice` block). `set_config` no longer writes here.
    obj.remove("dontspeak");
    // We do NOT touch Claude Code's own `voice` block. Read-don't-write: DontSpeak can't
    // (and shouldn't) force CC dictation on — symmetric with system STT, which we can't
    // grant ourselves either. The `claude_code` STT engine READS whether CC voice is
    // enabled + which key is bound and REPORTS it (telling the user to run `/voice` if
    // it's off), rather than silently flipping CC's settings.
    if let Some(ch) = spec.notif_channel {
        obj.entry("preferredNotifChannel")
            .or_insert_with(|| json!(ch));
    }
    root
}

/// Remove every DontSpeak hook group from `settings.json`, dropping an event that
/// becomes empty. Leaves our `dontspeak` block, Claude Code's `voice` block, and all
/// unrelated keys untouched.
pub fn strip_hooks(mut root: Value) -> Value {
    if let Some(hooks) = root
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
    {
        let events: Vec<String> = hooks.keys().cloned().collect();
        for evt in events {
            if let Some(arr) = hooks.get_mut(&evt).and_then(|v| v.as_array_mut()) {
                arr.retain(|g| !hook_group_is_ours(g));
                if arr.is_empty() {
                    hooks.remove(&evt);
                }
            }
        }
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
        }
    }

    #[test]
    fn merge_hooks_is_additive_and_idempotent() {
        // A user hook on a SHARED event (MessageDisplay) + an unrelated key must survive;
        // ours is added once.
        let root = json!({
            "model": "opus",
            "hooks": { "MessageDisplay": [ { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ] }
        });
        let once = merge_hooks(root, &spec());
        assert_eq!(once["model"], json!("opus"), "unrelated key preserved");
        assert_eq!(
            once["hooks"]["MessageDisplay"].as_array().unwrap().len(),
            2,
            "user + ours"
        );
        assert!(
            once.get("voice").is_none(),
            "CC voice block never written by wiring"
        );
        assert!(
            once.get("dontspeak").is_none(),
            "our config lives in our data dir, not settings.json"
        );
        // Re-running must NOT duplicate our group.
        let twice = merge_hooks(once.clone(), &spec());
        assert_eq!(
            twice["hooks"]["MessageDisplay"].as_array().unwrap().len(),
            2,
            "idempotent"
        );
        assert_eq!(twice, once, "second merge is a no-op");
    }

    #[test]
    fn merge_hooks_strips_stale_ds_block() {
        // Our config moved to our config.toml; a stale `dontspeak` block left in
        // settings.json by an older version is removed (the file stays purely CC's).
        let root = json!({ "dontspeak": { "voice": "am_adam", "custom": 1 }, "model": "opus" });
        let out = merge_hooks(root, &spec());
        assert!(
            out.get("dontspeak").is_none(),
            "stale dontspeak block removed"
        );
        assert_eq!(out["model"], json!("opus"), "unrelated key preserved");
    }

    #[test]
    fn merge_hooks_omits_ds_and_never_writes_cc_voice() {
        let out = merge_hooks(json!({}), &spec());
        // No `dontspeak` block is written into settings.json…
        assert!(out.get("dontspeak").is_none());
        // …and read-don't-write: wiring NEVER adds Claude Code's `voice` block. If CC
        // voice is off, the engine reports it (claude_code mode) instead of forcing it on.
        assert!(
            out.get("voice").is_none(),
            "CC voice block is not written by wiring"
        );
    }

    #[test]
    fn merge_hooks_leaves_user_voice_block_untouched() {
        // read-don't-write: wiring hooks never modifies Claude Code's `voice` block — the
        // user's enabled/mode/sibling keys all survive verbatim (we only add OUR hooks).
        let voice = json!({ "enabled": false, "mode": "hold", "autoSubmit": true });
        let root = json!({ "voice": voice.clone() });
        let out = merge_hooks(root, &spec());
        assert_eq!(
            out["voice"], voice,
            "CC voice block preserved verbatim (never written)"
        );
    }

    #[test]
    fn strip_hooks_removes_only_ours() {
        let merged = merge_hooks(
            json!({ "hooks": { "MessageDisplay": [ { "hooks": [ { "type": "command", "command": "/usr/bin/true" } ] } ] } }),
            &spec(),
        );
        let stripped = strip_hooks(merged);
        let md = stripped["hooks"]["MessageDisplay"].as_array().unwrap();
        assert_eq!(md.len(), 1, "user hook kept");
        assert_eq!(md[0]["hooks"][0]["command"], json!("/usr/bin/true"));
        // Events that were ONLY ours are dropped entirely.
        assert!(
            stripped["hooks"].get("SessionStart").is_none(),
            "ours-only event removed"
        );
    }

    #[test]
    fn merge_hooks_wires_sessionstart_notify_cross_platform() {
        // SessionStart carries the uniform `notify` command (which greets internally),
        // wired via the ONE canonical set every installer uses — all platforms identical
        // (no drift), recognized as ours (idempotent merge), removed cleanly on uninstall.
        let out = merge_hooks(json!({}), &spec());
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
        assert_eq!(ss[0]["hooks"][0]["args"], json!(["notify"]));
        // Re-running is idempotent (no duplicate group).
        let twice = merge_hooks(out.clone(), &spec());
        assert_eq!(
            twice["hooks"]["SessionStart"].as_array().unwrap().len(),
            1,
            "idempotent"
        );
        // strip_hooks removes it (recognized as ours).
        let stripped = strip_hooks(out);
        assert!(
            stripped["hooks"].get("SessionStart").is_none(),
            "notify hook stripped on uninstall"
        );
    }

    #[test]
    fn provide_query_is_sync_on_userpromptsubmit() {
        // Split by contract: every event gets the async `notify` command sink; UserPromptSubmit
        // ALSO gets the SYNCHRONOUS `provide` query (the narration spec as additionalContext).
        // Pin that `provide` is not async — its stdout JSON is read for the context; an async
        // hook is fire-and-forget and its output would be dropped (silently killing narration).
        let out = merge_hooks(json!({}), &spec());

        // SessionStart is notify-only now (voice greet, no visible banner) — that shape is
        // pinned by `merge_hooks_wires_sessionstart_notify_cross_platform`. Here we pin the
        // notify+provide split on UserPromptSubmit.
        // UserPromptSubmit carries notify (async) + provide (sync).
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .clone();
        let notify = ups
            .iter()
            .find(|h| h["args"] == json!(["notify"]))
            .expect("notify sink wired on UserPromptSubmit");
        assert_eq!(notify["async"], json!(true), "notify is fire-and-forget");
        let provide = ups
            .iter()
            .find(|h| h["args"] == json!(["provide"]))
            .expect("provide query wired on UserPromptSubmit");
        assert!(
            provide.get("async").is_none(),
            "provide must not be async (its stdout is read)"
        );
        assert!(provide["command"].as_str().unwrap().contains("dontspeak"));

        // Whole group is still ours → stripped cleanly on uninstall.
        assert!(strip_hooks(out)["hooks"].get("UserPromptSubmit").is_none());
    }

    #[test]
    fn merge_hooks_wires_stop_and_notification_earcon_events() {
        // The earcon events: Stop (reply ding) + Notification (needs-input cue) are wired as
        // async notify-only sinks, recognized as ours (idempotent), and stripped on uninstall.
        let out = merge_hooks(json!({}), &spec());
        for evt in ["Stop", "Notification"] {
            let g = out["hooks"][evt]
                .as_array()
                .unwrap_or_else(|| panic!("{evt} wired"));
            assert_eq!(g.len(), 1, "{evt} is a single notify group");
            assert_eq!(
                g[0]["hooks"][0]["args"],
                json!(["notify"]),
                "{evt} is notify-only"
            );
            assert_eq!(
                g[0]["hooks"][0]["async"],
                json!(true),
                "{evt} never blocks Claude"
            );
        }
        // Idempotent re-merge, and clean strip on uninstall.
        let twice = merge_hooks(out.clone(), &spec());
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
}
