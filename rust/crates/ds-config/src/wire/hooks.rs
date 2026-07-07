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
/// directory it controls TODAY — the CURRENT set, used as the "keep" list by `dontspeak wire`'s
/// stale-binary cleanup: it prunes any `dontspeak*`/`ds-*` executable in the install dir that
/// is NOT in this set (see `prune_stale_bins`), covering binaries a rename/drop left behind.
/// The install dir is user-writable on every platform (the macOS `.app` bundle, the Windows
/// `%LOCALAPPDATA%\Programs\DontSpeak` extract, Linux `~/.local/bin`), so the prune runs
/// entirely in the user context.
pub const INSTALLED_BINS: &[&str] = &["dontspeak", "ds-helper", "ds-winui", "ds-gtk"];

/// Inputs for [`merge_hooks`]: the resolved hook command path + voice prefs. All `&str`
/// so the caller owns path formatting (incl. the platform `.exe` suffix).
pub struct HookSpec<'a> {
    /// Absolute path to the single `dontspeak[.exe]` multi-call binary. Every hook is this
    /// one binary with a different `args` head (`notify` for the async sinks, `provide` for
    /// the synchronous narration-spec query).
    pub bin: &'a str,
    /// Optional `preferredNotifChannel` (e.g. macOS iTerm's `"iterm2_with_bell"`).
    pub notif_channel: Option<&'a str>,
    /// Whether the client streams assistant messages via a `MessageDisplay` hook event
    /// (Claude Code). `true` wires `MessageDisplay` for per-batch narration; `false`
    /// (Qwen Code, Codex) omits it — the full reply is voiced from `Stop`'s
    /// `last_assistant_message` via the non-streaming `speak_reply` path.
    pub streaming: bool,
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
    // every fire-and-forget event; `MessageDisplay` is the streaming narration pipeline
    // (Claude Code ≥ 2.1.x streams it per batch) — omitted for non-streaming clients
    // (Qwen Code, Codex) where the reply is voiced whole from `Stop`. SessionStart greets +
    // seeds the streaming witness; SessionEnd barges this window's playback;
    // UserPromptSubmit marks THIS terminal active so narration follows it AND carries the
    // synchronous `provide` (the narration spec as `additionalContext`). Stop fires once
    // when the turn ends → the reply "ding" earcon (and, for non-streaming clients, the
    // `speak_reply` fallback voices the whole reply). Notification fires on a permission
    // prompt / idle → the needs-input earcon.
    let mut groups: Vec<(&'static str, Value)> = Vec::new();
    if spec.streaming {
        groups.push(("MessageDisplay", json!({ "hooks": [ notify(10) ] })));
    }
    // SessionStart is async-notify ONLY: the engine voice greet + streaming-witness seed,
    // off the critical path. The greeting is voice-only — there is no visible banner, so no
    // synchronous `provide` twin (CC 2.1+ drops a SessionStart hook's stdout anyway).
    groups.push(("SessionStart", json!({ "hooks": [ notify(0) ] })));
    groups.push(("SessionEnd", json!({ "hooks": [ notify(0) ] })));
    // Stop fires once when the turn finishes → the reply "ding" earcon. Notification
    // fires on a permission prompt / idle → the needs-input earcon. Both are async notify
    // sinks (never block the client); the binary routes them in `hook_core` and self-gates on
    // `earcon_enabled` / notification_type.
    groups.push(("Stop", json!({ "hooks": [ notify(0) ] })));
    groups.push(("Notification", json!({ "hooks": [ notify(0) ] })));
    groups.push((
        "UserPromptSubmit",
        json!({ "hooks": [
            notify(5),
            { "type": "command", "command": spec.bin, "args": ["provide"], "timeout": 5 } ] }),
    ));
    groups
}

/// Why a [`merge_hooks`] call could not apply. Mirrors Codex TOML's `CodexMergeError`: an
/// unmergeable shape is reported, not silently coerced away, and the caller must treat it
/// as a non-success (the original document is untouched — `merge_hooks` is pure, so nothing
/// was ever written to disk).
#[derive(Debug)]
pub enum HooksMergeError {
    /// `hooks.<Event>` exists in the parsed JSON but is not an array (a hand-edited or
    /// foreign shape, e.g. an object or string). We do NOT silently replace it with an
    /// empty array — that would discard whatever the user had there. `String` names the
    /// offending path (`hooks.<Event>`).
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

/// Merge the canonical DontSpeak hooks into a parsed `settings.json`, PRESERVING every
/// other key. REPLACE-OURS + idempotent: per event, our existing groups are swapped for
/// the fresh canonical one (never duplicated); other hooks on that event survive. The
/// replace (not keep-if-present) is what makes a re-wire SELF-HEALING: an old group
/// recognized as ours only by the `dontspeak` basename may point at a binary that moved
/// or was deleted (e.g. the pre-app-bundle `~/.local/bin/dontspeak`), and keeping it
/// verbatim would leave every hook dead after an upgrade. Shape changes (events, flags,
/// verbs) reach existing installs the same way, on any plain re-wire. The `voice` block
/// is only CREATED when absent (never overrides an existing mode). `preferredNotifChannel`
/// is UPDATED (not just get-or-created) so a changed channel reaches existing installs on
/// re-wire too, mirroring the group-replace self-heal above. A non-array `hooks.<Event>`
/// (hand-edited/foreign shape) is reported as [`HooksMergeError::UnmergeableShape`] instead
/// of being silently discarded, matching the Codex TOML path's `UnmergeableShape` — the
/// file is left exactly as it was. PURE — no disk.
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
                    // A non-array `hooks.<Event>` is a hand-edited/foreign shape we must not
                    // clobber by silently replacing it with an empty array — report instead,
                    // same as the Codex TOML path's `UnmergeableShape`.
                    let Some(arr) = slot.as_array_mut() else {
                        return Err(HooksMergeError::UnmergeableShape(format!("hooks.{evt}")));
                    };
                    // Replace ours, don't keep-if-present: an existing group matches on the
                    // `dontspeak` basename alone, so its command may be a STALE absolute path
                    // from a previous install layout. Keeping it would wire nothing.
                    arr.retain(|g| !hook_group_is_ours(g));
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
        // UPDATE, not get-or-create: a re-wire with a different resolved channel (e.g. a
        // terminal-detection change) must reach an existing install, mirroring the hook
        // group self-heal above — get-or-create alone would leave a stale channel forever.
        obj.insert("preferredNotifChannel".to_string(), json!(ch));
    }
    Ok(root)
}

/// Remove every DontSpeak hook group from `settings.json`, dropping an event that becomes
/// empty — and the whole `hooks` object too if it becomes empty (undoing the get-or-create
/// scaffold `merge_hooks` adds when `hooks` is absent). Also removes the top-level
/// `preferredNotifChannel` key `merge_hooks` may have set: strip must undo exactly what
/// merge added, no more and no less, so a full `dontspeak unwire claude_code` doesn't leave
/// an emptied `hooks: {}` scaffold or a stray `preferredNotifChannel` behind. Leaves Claude
/// Code's `voice` block and all other unrelated keys untouched.
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

    fn spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
        }
    }

    /// `merge_hooks` with the default `spec()`, unwrapped — the happy path every test but
    /// the dedicated error tests exercises.
    fn merged(root: Value) -> Value {
        merge_hooks(root, &spec()).expect("merge ok")
    }

    #[test]
    fn merge_hooks_is_additive_and_idempotent() {
        // A user hook on a SHARED event (MessageDisplay) + an unrelated key must survive;
        // ours is added once.
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
        // Re-running must NOT duplicate our group.
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
        // Upgrade self-heal: hooks wired by a previous install layout point at a binary
        // that no longer exists (e.g. ~/.local/bin/dontspeak before the app-bundle move).
        // A re-wire must UPDATE the command to the freshly resolved path, not keep the
        // dead group as "already wired" — that exact keep left every hook silently
        // broken after the v0.1.0 → Helpers-layout upgrade.
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
    }

    #[test]
    fn merge_hooks_strips_stale_ds_block() {
        // Our config moved to our config.toml; a stale `dontspeak` block left in
        // settings.json by an older version is removed (the file stays purely CC's).
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
        assert_eq!(ss[0]["hooks"][0]["args"], json!(["notify"]));
        // Re-running is idempotent (no duplicate group).
        let twice = merged(out.clone());
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
        let out = merged(json!({}));

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
    fn non_streaming_client_omits_messagedisplay_keeps_stop_provide() {
        // Qwen Code (and Codex) have no MessageDisplay stream — the reply is voiced whole
        // from Stop's last_assistant_message. A non-streaming wire must omit MessageDisplay
        // (a dead hook for an event the client never fires) while keeping the events that
        // DO fire: SessionStart, SessionEnd, Stop, Notification, UserPromptSubmit.
        let spec_ns = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: false,
        };
        let out = merge_hooks(json!({}), &spec_ns).expect("merge ok");
        assert!(
            out["hooks"].get("MessageDisplay").is_none(),
            "MessageDisplay must NOT be wired for a non-streaming client"
        );
        // The events that DO fire are all present.
        for evt in ["SessionStart", "SessionEnd", "Stop", "Notification", "UserPromptSubmit"] {
            assert!(
                out["hooks"].get(evt).is_some(),
                "{evt} wired for non-streaming client"
            );
        }
        // Stop is where the reply gets voiced for a non-streaming client.
        assert_eq!(
            out["hooks"]["Stop"][0]["hooks"][0]["args"],
            json!(["notify"]),
            "Stop notify sink present"
        );
        // UserPromptSubmit still carries the synchronous `provide` query.
        let ups = out["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        assert!(ups.iter().any(|h| h["args"] == json!(["provide"])));
    }

    #[test]
    fn merge_hooks_wires_stop_and_notification_earcon_events() {
        // The earcon events: Stop (reply ding) + Notification (needs-input cue) are wired as
        // async notify-only sinks, recognized as ours (idempotent), and stripped on uninstall.
        let out = merged(json!({}));
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
        // get-or-create alone would leave a channel set by an earlier wire stuck forever;
        // a later re-wire (e.g. after a channel-resolution change) must UPDATE it, mirroring
        // the self-healing hook-group replace above.
        let spec_a = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("iterm2_with_bell"),
            streaming: true,
        };
        let once = merge_hooks(json!({}), &spec_a).expect("merge ok");
        assert_eq!(once["preferredNotifChannel"], json!("iterm2_with_bell"));

        let spec_b = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("other_channel"),
            streaming: true,
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
        // `merge_hooks`' get-or-create `hooks` scaffold and `preferredNotifChannel` are both
        // things WIRE adds; a full unwire must remove both, not just the hook groups —
        // otherwise `preferredNotifChannel` (and an emptied `hooks: {}`) survive a full
        // `dontspeak unwire claude_code`.
        let spec_with_channel = HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: Some("iterm2_with_bell"),
            streaming: true,
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
        // A hand-edited/foreign shape at hooks.<Event> (e.g. an object, not our array-of-
        // groups convention) must not be silently replaced with an empty array — discarding
        // whatever the user had there. Mirrors Codex TOML's `UnmergeableShape`: report,
        // don't clobber.
        let root = json!({ "hooks": { "MessageDisplay": { "not": "an array" } } });
        let err = merge_hooks(root, &spec()).expect_err("non-array hooks.<Event> must error");
        assert!(
            matches!(&err, HooksMergeError::UnmergeableShape(s) if s.contains("MessageDisplay")),
            "error names the offending path: {err}"
        );
    }
}
