//! Claude-contract JSON hooks — single source for Claude Code (args array) and Qwen
//! (inline shell; [`HookCommandStyle`]). Pure merge/strip; wire orchestrator owns disk.
//! Additive: never clobbers unrelated keys.

use super::cmdline::{ShellOverride, command_is_ours, host_inline_flavor, inline_command};
use super::registry::HookCommandStyle;
use ds_client::ClientSource;
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
    /// one binary with a different verb head (`notify` for the async sinks, `provide` for
    /// the synchronous narration-spec query) — carried in the `args` array
    /// ([`HookCommandStyle::ArgsArray`]) or inlined into the command string
    /// ([`HookCommandStyle::InlineShell`]).
    pub bin: &'a str,
    /// Optional `preferredNotifChannel` (e.g. macOS iTerm's `"iterm2_with_bell"`).
    pub notif_channel: Option<&'a str>,
    /// Whether the client streams assistant messages via a `MessageDisplay` hook event
    /// (Claude Code). `true` wires `MessageDisplay` for per-batch narration; `false`
    /// (Qwen Code, Codex) omits it — the full reply is voiced from `Stop`'s
    /// `last_assistant_message` via the non-streaming `speak_reply` path.
    pub streaming: bool,
    /// How the client's hook runner executes a wired entry — Claude Code spawns
    /// `command` + `args` directly (timeout in seconds); Qwen Code hands ONLY the
    /// `command` string to a shell (no `args` field; timeout in milliseconds).
    pub command_style: HookCommandStyle,
    /// WHICH client these hooks are being wired for. Stamped onto EVERY verb slice as a
    /// trailing `--client <token>` (`hook_entry`), so the `dontspeak` binary the hook
    /// spawns knows who invoked it and can put that client on its `ds-ipc` requests and its
    /// activity-log lines. Uniform across every client and every event — no shaper hardcodes
    /// its own client, so a future client reusing this mechanism stays "a registry entry, not
    /// a new writer".
    pub client: ClientSource,
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

// The command-string dialect (`InlineFlavor`), the renderer (`inline_command`) and the
// "is this entry ours?" parse (`command_is_ours`) all live in the shared `wire::cmdline`
// module — Qwen and Codex share that renderer and must not drift from it.
//
// Qwen's `expandCommand` rewrites literal `$GEMINI_PROJECT_DIR`/`$CLAUDE_PROJECT_DIR` in the
// command string before spawning; our inlined commands contain neither, so that pass is a
// no-op.

/// Build ONE hook command entry in the dialect `spec.command_style` selects:
/// [`HookCommandStyle::ArgsArray`] (Claude Code) → `command` = bin, verbs in `args`,
/// `timeout` in SECONDS; [`HookCommandStyle::InlineShell`] (Qwen Code) → NO `args` key,
/// verbs inlined into `command` (see [`inline_command`]), `timeout` scaled to MILLISECONDS
/// (Qwen SIGTERMs the hook at `timeout` ms — an unscaled `5` would be 5 ms). A zero
/// `timeout_secs` omits the field (Claude Code default; Qwen falls to its 60 s default).
///
/// EVERY entry, in either dialect, carries a trailing `--client <token>` ([`HookSpec::client`])
/// — two more `args` elements for `ArgsArray`, two more space-joined tokens for `InlineShell`.
/// The token is snake_case with no spaces and no quotes, so the Windows quote-free command
/// invariant holds by construction (`inline_command` just space-joins the slice).
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
            // Qwen's CommandHookConfig HAS a `shell` field, so a spaced bin path can be pinned
            // to PowerShell rather than needing the 8.3 short name Codex falls back to.
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

/// The canonical `(event, group)` hook set in settings.json shape — the ONE
/// definition every platform installs.
fn canonical_hook_groups(spec: &HookSpec) -> Vec<(&'static str, Value)> {
    // EVERY hook is the SAME binary with ONE of two verbs, split by contract (see hook_core):
    //   `notify`  — COMMAND sink, ASYNC fire-and-forget, replies with nothing. The binary
    //               routes on the payload's `hook_event_name`, so the wiring is uniform — only
    //               the event list + per-entry flags differ, never the command.
    //   `provide` — QUERY, SYNCHRONOUS, returns the `hookSpecificOutput` JSON. The ONE verb
    //               the client waits on (an async run would drop the output → no context).
    let notify = |timeout: u64| hook_entry(spec, &["notify"], timeout, true);
    // One group per event (ours, so merge stays idempotent + strip stays clean). `notify` on
    // every fire-and-forget event; `MessageDisplay` is the streaming narration pipeline
    // (Claude Code and Qwen Code stream it per batch) — omitted for non-streaming clients,
    // where the reply is voiced whole from `Stop`. SessionStart greets
    // (and, for streaming clients only, seeds the streaming witness); SessionEnd barges this
    // window's playback; UserPromptSubmit marks THIS terminal active so narration follows it
    // AND carries the synchronous `provide` (the narration spec as `additionalContext`).
    // Stop fires once when the turn ends → the reply "ding" earcon (and, for non-streaming
    // clients, the `speak_reply` fallback voices the whole reply). Notification fires on a
    // permission prompt / idle → the needs-input earcon.
    let mut groups: Vec<(&'static str, Value)> = Vec::new();
    if spec.streaming {
        groups.push(("MessageDisplay", json!({ "hooks": [ notify(10) ] })));
    }
    // SessionStart is async-notify ONLY: the engine voice greet, off the critical path. The
    // greeting is voice-only — there is no visible banner, so no synchronous `provide` twin
    // (CC 2.1+ drops a SessionStart hook's stdout anyway). Streaming clients get the plain
    // `notify` (greet + streaming-witness seed); NON-streaming clients get
    // `notify --greet-only` — on a client with no `MessageDisplay` stream, seeding the
    // witness would mark every session "already narrated" and silence each Stop reply.
    let session_start = if spec.streaming {
        notify(0)
    } else {
        hook_entry(spec, &["notify", "--greet-only"], 0, true)
    };
    groups.push(("SessionStart", json!({ "hooks": [ session_start ] })));
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
            hook_entry(spec, &["provide"], 5, false) ] }),
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

    /// The Claude Code combination: args-array commands, `MessageDisplay` stream.
    fn spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
            command_style: HookCommandStyle::ArgsArray,
            client: ClientSource::ClaudeCode,
        }
    }

    /// The Qwen Code combination: inline-shell commands with `MessageDisplay` streaming.
    fn inline_spec() -> HookSpec<'static> {
        HookSpec {
            bin: "/bin/dontspeak",
            notif_channel: None,
            streaming: true,
            command_style: HookCommandStyle::InlineShell,
            client: ClientSource::QwenCode,
        }
    }

    /// `merge_hooks` with the default `spec()`, unwrapped — the happy path every test but
    /// the dedicated error tests exercises.
    fn merged(root: Value) -> Value {
        merge_hooks(root, &spec()).expect("merge ok")
    }

    /// The `args` array a wired ArgsArray entry carries for `verbs`: the verbs PLUS the uniform
    /// `--client <token>` tail `hook_entry` stamps on EVERY entry. Every exact-equality
    /// assertion below goes through this, so the tail can't be asserted away by accident.
    fn args(client: ClientSource, verbs: &[&str]) -> Value {
        let mut v = verbs.to_vec();
        v.extend_from_slice(&["--client", client.as_str()]);
        json!(v)
    }

    /// The tail an inlined (InlineShell) command string ends with for `verbs` — same idea as
    /// [`args`], for the dialect where the verbs are space-joined into the command.
    fn tail(client: ClientSource, verbs: &str) -> String {
        format!(" {verbs} --client {}", client.as_str())
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
        // The stale group's verbs also predate the `--client` token (`args: ["notify"]`), and
        // the SAME replace-ours heals that: exactly one group of ours, now carrying the token.
        // `command_is_ours` only inspects the LEADING path token, which is why a client-less
        // group is still recognised as ours instead of being duplicated beside a fresh one
        // (two greets, two narrations). Self-healing, not backward compatibility — the engine
        // re-wires every client at boot, so an existing install converges with no user action.
        assert_eq!(
            ours[0]["hooks"][0]["args"],
            args(ClientSource::ClaudeCode, &["notify"]),
            "client-less verbs healed to the token-carrying shape"
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
        assert_eq!(
            ss[0]["hooks"][0]["args"],
            args(ClientSource::ClaudeCode, &["notify"])
        );
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

        // Whole group is still ours → stripped cleanly on uninstall.
        assert!(strip_hooks(out)["hooks"].get("UserPromptSubmit").is_none());
    }

    #[test]
    fn non_streaming_client_omits_messagedisplay_keeps_stop_provide() {
        // A non-streaming wire omits MessageDisplay and voices the reply from Stop while
        // retaining the other lifecycle and prompt events.
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
        // The events that DO fire are all present.
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
        // Stop is where the reply gets voiced for a non-streaming client.
        assert_eq!(
            out["hooks"]["Stop"][0]["hooks"][0]["args"],
            args(ClientSource::QwenCode, &["notify"]),
            "Stop notify sink present"
        );
        // UserPromptSubmit still carries the synchronous `provide` query.
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
                args(ClientSource::ClaudeCode, &["notify"]),
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
        // `merge_hooks`' get-or-create `hooks` scaffold and `preferredNotifChannel` are both
        // things WIRE adds; a full unwire must remove both, not just the hook groups —
        // otherwise `preferredNotifChannel` (and an emptied `hooks: {}`) survive a full
        // `dontspeak unwire claude_code`.
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

    // ── InlineShell (Qwen Code) — verbs inlined into the command string ─────────────
    //
    // Qwen's hook runner passes ONLY `command` to a shell; its CommandHookConfig has NO
    // `args` field (silently dropped), and `timeout` is MILLISECONDS. These pin the dialect
    // AS QWEN SEES IT: no `args` key anywhere, inlined verbs, scaled timeouts, the
    // `shell: "powershell"` pin on a spaced path. The command STRING itself (per-flavor,
    // both driven on any OS so Linux CI covers the Windows form) is pinned by
    // `wire::cmdline`'s own tests, which is also where Codex gets it from.

    #[test]
    fn command_is_ours_accepts_every_dialect_we_write_and_rejects_the_rest() {
        // Bare paths (args-array style, and the pre-inline Qwen shape we self-heal).
        assert!(command_is_ours("/bin/dontspeak"));
        assert!(command_is_ours(
            r"C:\Users\u\AppData\Local\Programs\DontSpeak\dontspeak.exe"
        ));
        // Inlined forms, all three flavors.
        assert!(command_is_ours("\"/opt/x y/dontspeak\" notify"));
        assert!(command_is_ours(
            "C:/Users/u/AppData/Local/Programs/DontSpeak/dontspeak.exe notify --greet-only"
        ));
        assert!(command_is_ours(
            r#"& "C:\Program Files\DontSpeak\dontspeak.exe" provide"#
        ));
        // Not ours: foreign commands, prefix-sharing names, and near-misses.
        assert!(!command_is_ours("/usr/bin/true"));
        assert!(!command_is_ours("dontspeak-uninstall"));
        assert!(!command_is_ours("ds-sync"));
        assert!(!command_is_ours(""));
    }

    #[test]
    fn inline_shell_entries_have_no_args_key_and_millisecond_timeouts() {
        // The Qwen dialect end-to-end through merge: NO `args` key on ANY entry (Qwen drops
        // it silently — the root cause of the dead wiring), verbs inlined into `command`,
        // UserPromptSubmit timeouts scaled seconds→ms (5 → 5000; an unscaled 5 would be a
        // 5 ms SIGTERM), zero-timeout events omit the field (Qwen's 60 s default), `async`
        // preserved on notify entries, and `provide` never async (its stdout is read).
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
        // Streaming SessionStart is plain notify so it seeds the stream witness too.
        let ss = out["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            ss.ends_with(&tail(ClientSource::QwenCode, "notify")),
            "streaming SessionStart carries the client token without greet-only, got {ss}"
        );
        // Zero-timeout events omit the field entirely.
        for evt in ["SessionStart", "SessionEnd", "Stop", "Notification"] {
            let h = &out["hooks"][evt][0]["hooks"][0];
            assert!(h.get("timeout").is_none(), "{evt}: no timeout field");
            assert_eq!(h["async"], json!(true), "{evt}: async notify sink");
        }
        // UserPromptSubmit: notify (async, 5000 ms) + provide (sync, 5000 ms).
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
        // Claude Code (args-array) reads `timeout` in SECONDS — the seconds→ms scaling is
        // strictly an InlineShell dialect rule, so `timeout: 5` must stay 5, not 5000.
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
        // The args-array dialect carries the same greet-only split: a streaming client
        // (Claude Code) seeds the witness with plain `notify`; a non-streaming args-array
        // wire gets `--greet-only` so the seed can't silence its Stop replies.
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
        // Qwen Code 0.19.10 ships MessageDisplay. This test pins the streaming +
        // InlineShell emits the `MessageDisplay` group with the INLINED `notify` command
        // (no `args` key — Qwen drops it silently) and the MILLISECOND-scaled timeout
        // (10 s → 10000 ms; an unscaled 10 would be a 10 ms SIGTERM), and SessionStart
        // switches to the PLAIN `notify` (the streaming-witness seed, no `--greet-only`).
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
        // The live-install fix: a Qwen settings.json wired by the broken version holds the
        // bare-command + `args` groups (which Qwen silently ran as the arg-less MCP server).
        // `command_is_ours` still matches the bare path, so a plain re-wire REPLACES each
        // stale group with exactly one inlined group — no manual unwire needed.
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
