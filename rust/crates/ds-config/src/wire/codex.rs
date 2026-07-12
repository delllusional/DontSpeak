//! OpenAI Codex CLI hooks (`~/.codex/config.toml`).
//!
//! Codex grew a hooks system with the SAME contract as Claude Code's — events routed by
//! `hook_event_name`, one JSON object on stdin, `Stop` carrying `last_assistant_message`,
//! `UserPromptSubmit` honouring `hookSpecificOutput.additionalContext` — so the SAME
//! `dontspeak notify` / `dontspeak provide` binary handles them. Only two things differ from
//! the Claude Code wiring: the file is TOML (so we edit it with `toml_edit` to preserve the
//! user's tables + comments), and the per-event set is Codex-shaped:
//!
//!   * `SessionStart` → `dontspeak notify --greet-only` — the spoken greeting when a session
//!     opens. `--greet-only` because Codex has no `MessageDisplay` hook stream: the plain
//!     `notify` would seed the streaming witness, which on a hook-only client marks every
//!     session "already narrated" and suppresses the `Stop` narration entirely. (Mid-turn
//!     Codex narration exists, but rides the engine's app-server subscriber —
//!     `dontspeakd::codex_stream` — which seeds the witness itself on a successful
//!     `thread/resume`, so only actually-streamed sessions silence their `Stop`.)
//!   * `UserPromptSubmit` → ONE group with TWO inner hooks (Codex launches all matching
//!     hooks for an event concurrently):
//!       - `dontspeak notify` — `MarkActive{session}`: per-terminal active routing +
//!         `input_clears` parity for mid-turn narration, AND the engine's session
//!         re-discovery after a restart (the codex_stream supervisor otherwise only learns
//!         session ids at SessionStart's greet).
//!       - `dontspeak provide` — inject the narration spec so Codex WRITES the spoken-line
//!         blockquotes (without this it has nothing to speak).
//!   * `Stop` → `dontspeak notify` — speak the final reply for sessions NOT streamed via the
//!     app-server (plain-TUI); a streamed session's witness keeps this silent.
//!
//! Written as (POSIX shown; on Windows `command` is the QUOTE-FREE form — Codex hands the
//! whole string to `cmd.exe /C` as one argv element, where an embedded `"` cannot survive.
//! See [`cmdline`](super::cmdline)):
//!   [[hooks.Stop]]
//!   [[hooks.Stop.hooks]]
//!   type = "command"
//!   command = "\"…/dontspeak\" notify"
//!   timeout = 1800
//!
//! Additive + idempotent, mirroring `merge_hooks`: a group is "ours" if one of its inner
//! hook commands invokes the `dontspeak` binary — identified precisely by the command's
//! binary basename (see `command_is_ours`), the same discipline the Claude Code JSON path
//! uses (`command_is_ours` there), NOT a substring match on the rendered command string.

use super::cmdline::{ShellOverride, command_is_ours, host_inline_flavor, inline_command};
use ds_client::ClientSource;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item as TomlItem, Table as TomlTable, value};

/// Render one Codex hook command. Codex's command-hook schema is `command` /
/// `command_windows` / `timeout` / `async` / `statusMessage` — there is NO `shell` field, so
/// a spaced bin path cannot be pinned to a shell that tolerates quotes and must instead be
/// made space-free (the 8.3 short name); see [`super::cmdline`].
///
/// We write the host's dialect into plain `command` rather than splitting it across
/// `command`/`command_windows`: `dontspeak wire` runs ON the machine it is wiring, the bin
/// path it embeds is that machine's absolute install path, and Codex resolves
/// `command_windows.unwrap_or(command)` — so a second, non-host form would be dead weight
/// carrying a path that doesn't exist on the other OS anyway. This matches what the Claude
/// Code / Qwen JSON writers already do.
///
/// `client` is stamped on as a trailing `--client <token>` — the same uniform tail every other
/// hook mechanism appends (see [`super::hooks::HookSpec::client`]), so the binary knows who
/// invoked it. Quote-free by construction: the token is snake_case with no spaces.
fn codex_command(bin: &str, verb: &str, client: ClientSource) -> String {
    inline_command(
        host_inline_flavor(),
        bin,
        [verb, "--client", client.as_str()],
        ShellOverride::Unsupported,
    )
    .0
}

/// The `(event, [(verb, timeout)])` hooks we wire into Codex — ONE group per event, holding
/// that event's inner hooks. Codex has no MessageDisplay hook stream, so the reply is voiced
/// from `Stop` (unless the engine's app-server subscriber streamed it mid-turn); the
/// narration spec is injected at `UserPromptSubmit` via the synchronous `provide` verb (its
/// stdout `additionalContext` is read by Codex), alongside a `notify` sibling in the SAME
/// group — mark-active routing + engine session re-discovery (see the module docs); and
/// `SessionStart` greets with `--greet-only` — the flag skips the streaming-witness seed,
/// which on a hook-only client would silence every `Stop` reply. Timeouts are SECONDS,
/// and every entry runs SYNCHRONOUSLY: Codex SKIPS `async = true` hooks outright, so we never
/// emit an `async` flag here (the UserPromptSubmit `notify` is a ~ms `MarkActive` ping, so a
/// synchronous spawn per prompt is fine). That's also why SessionStart carries an explicit
/// 30 s ceiling — the greet ping returns in milliseconds, and a tight bound beats inheriting
/// Codex's 600 s default on a hook it blocks on.
const CODEX_HOOKS: &[(&str, &[(&str, i64)])] = &[
    ("SessionStart", &[("notify --greet-only", 30)]),
    ("UserPromptSubmit", &[("notify", 5), ("provide", 5)]),
    ("Stop", &[("notify", 1800)]),
];

/// Why a [`merge_codex_hooks`]/[`strip_codex_hooks`] call could not apply. The caller must
/// treat BOTH variants as a non-success: an unmergeable shape must NOT be reported as a
/// silent success (it would claim the hooks were wired while wiring nothing).
#[derive(Debug)]
pub enum CodexMergeError {
    /// The file is not valid TOML (passes through `toml_edit`'s parse error).
    Parse(toml_edit::TomlError),
    /// The file is valid TOML, but `hooks` / `hooks.<event>` has a shape we can neither
    /// append to nor safely coerce (e.g. `hooks = "x"`, or an event is a scalar). We do NOT
    /// clobber the user's file; we report so the installer can warn instead of claiming success.
    UnmergeableShape(String),
}

impl std::fmt::Display for CodexMergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodexMergeError::Parse(e) => write!(f, "config.toml is not valid TOML: {e}"),
            CodexMergeError::UnmergeableShape(s) => {
                write!(
                    f,
                    "config.toml has an unexpected `{s}` shape; left unchanged (Codex hooks NOT wired)"
                )
            }
        }
    }
}

impl std::error::Error for CodexMergeError {}

impl From<toml_edit::TomlError> for CodexMergeError {
    fn from(e: toml_edit::TomlError) -> Self {
        CodexMergeError::Parse(e)
    }
}

/// True if `cmd` (a Codex hook's `command` string) invokes OUR `dontspeak` binary — matched
/// by the leading double-quoted path's basename (file stem), mirroring the Claude Code JSON
/// path's `command_is_ours`. Deliberately NOT a substring match on the whole string: a
/// user's own unrelated hook whose command merely happens to CONTAIN "dontspeak" (e.g. a
/// personal script path component) must not be misidentified as ours — that misidentification
/// was empirically reproduced to both silently skip wiring our real hook (merge sees the
/// event as "already ours") AND make `strip_codex_hooks` delete the user's entire hook group
/// on unwire.
///
/// Recognition is the SHARED [`super::cmdline::command_is_ours`] — deliberately NOT a local
/// copy. It accepts every dialect we have ever written, which is what makes a re-wire
/// self-heal: entries left by the old always-quoted Windows form are still seen as ours, so
/// merge replaces them rather than appending a duplicate, and unwire still removes them.
/// (The previous local copy required a LEADING QUOTE and used `std::path::Path`, so it
/// recognised neither the quote-free Windows command we now emit nor a `\`-separated path on
/// a non-Windows host.)
///
/// Does this inner hooks `Item` (the `[[hooks.<event>.hooks]]` array-of-tables OR an inline
/// `hooks = [{…}]` array) hold a `command` referencing our `dontspeak` binary?
fn inner_hooks_are_ours(item: &TomlItem) -> bool {
    let cmd_is_ours = |c: Option<&str>| c.is_some_and(command_is_ours);
    if let Some(aot) = item.as_array_of_tables() {
        return aot
            .iter()
            .any(|t| cmd_is_ours(t.get("command").and_then(|c| c.as_str())));
    }
    if let Some(arr) = item.as_array() {
        return arr.iter().any(|e| {
            cmd_is_ours(
                e.as_inline_table()
                    .and_then(|t| t.get("command"))
                    .and_then(|c| c.as_str()),
            )
        });
    }
    false
}

/// True if this `[[hooks.<event>]]` group is one of ours.
fn codex_group_is_ours(group: &TomlTable) -> bool {
    group.get("hooks").is_some_and(inner_hooks_are_ours)
}

/// True if this `[[hooks.<event>]]` group is ALREADY exactly ours for this event's rendered
/// `commands` — its inner command list is IDENTICAL (same commands, same order; a group may
/// hold SEVERAL inner hooks now, e.g. UserPromptSubmit's notify + provide). Used to make a
/// re-wire with an UNCHANGED `dontspeak` path a true byte-for-byte no-op (see
/// `append_to_event`): only rebuild the group when something actually differs, rather than
/// unconditionally retain+push-ing an identical replacement every time (which would risk
/// disturbing `toml_edit`'s formatting/decor and would rewrite the file on every plain
/// re-wire even when nothing changed). Exact-list (not any-member) matching means an OLD
/// one-verb group from a previous version correctly reads "differs" and is healed to the
/// current two-verb shape on re-wire.
fn codex_group_matches(group: &TomlTable, commands: &[String]) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array_of_tables())
        .is_some_and(|aot| {
            let got: Vec<&str> = aot
                .iter()
                .filter_map(|t| t.get("command").and_then(|c| c.as_str()))
                .collect();
            got == commands.iter().map(String::as_str).collect::<Vec<_>>()
        })
}

/// `codex_group_matches` for the inline-array element case (a user's `event = [{ hooks = … }]`).
fn inner_hooks_value_matches(v: &toml_edit::Value, commands: &[String]) -> bool {
    if let Some(arr) = v.as_array() {
        let got: Vec<&str> = arr
            .iter()
            .filter_map(|e| {
                e.as_inline_table()
                    .and_then(|t| t.get("command"))
                    .and_then(|c| c.as_str())
            })
            .collect();
        return got == commands.iter().map(String::as_str).collect::<Vec<_>>();
    }
    false
}

/// Build our group (`{ hooks = [[…]] }`) for this event's `(command, timeout)` list as a
/// standalone table — one inner `[[hooks.<event>.hooks]]` table per command, in order.
fn codex_our_group(commands: &[(String, i64)]) -> TomlTable {
    let mut inner_aot = ArrayOfTables::new();
    for (command, timeout) in commands {
        let mut inner = TomlTable::new();
        inner.insert("type", value("command"));
        inner.insert("command", value(command));
        inner.insert("timeout", value(*timeout));
        inner_aot.push(inner);
    }
    let mut group = TomlTable::new();
    group.insert("hooks", TomlItem::ArrayOfTables(inner_aot));
    group
}

/// Get-or-create the `[hooks]` table, returning `None` (→ UnmergeableShape) if `hooks` exists
/// as a non-table scalar we must not clobber.
fn hooks_table(doc: &mut DocumentMut) -> Result<&mut TomlTable, CodexMergeError> {
    if doc.get("hooks").is_none() {
        let mut t = TomlTable::new();
        t.set_implicit(true);
        doc.insert("hooks", TomlItem::Table(t));
    }
    doc.get_mut("hooks")
        .and_then(|h| h.as_table_mut())
        .ok_or_else(|| CodexMergeError::UnmergeableShape("hooks".into()))
}

/// REPLACE-OURS + append our group into the `hooks.<event>` slot — handling both the
/// array-of-tables form we write and a user's inline `<event> = [{…}]` array. Returns `Err`
/// only for a scalar shape we can't coerce. Mirrors the Claude Code JSON path's
/// `merge_hooks`: an existing group of ours on this event is REPLACED (not just left as
/// "already wired") whenever its inner-command LIST differs from the freshly rendered one —
/// so a re-wire after the resolved `dontspeak` binary path changes (an install-layout
/// upgrade), OR after the wired verb set for the event changes (e.g. UserPromptSubmit
/// growing its `notify` sibling), HEALS the group instead of leaving a stale one in place.
/// When the list is UNCHANGED, the slot is left byte-for-byte untouched (a true no-op, not
/// a rebuild-to-the-same-content) — `commands` are the exact strings `group` was built from
/// (see `merge_codex_hooks`), used only for that identical-content check. A user's own
/// unrelated hook on the same event is never touched either way.
fn append_to_event(
    htbl: &mut TomlTable,
    event: &str,
    group: TomlTable,
    commands: &[String],
) -> Result<(), CodexMergeError> {
    match htbl.get_mut(event) {
        None => {
            let mut aot = ArrayOfTables::new();
            aot.push(group);
            htbl.insert(event, TomlItem::ArrayOfTables(aot));
            Ok(())
        }
        Some(item) => {
            if item.is_array_of_tables() {
                let aot = item.as_array_of_tables_mut().expect("checked");
                if !aot.iter().any(|g| codex_group_matches(g, commands)) {
                    aot.retain(|g| !codex_group_is_ours(g));
                    aot.push(group);
                }
                Ok(())
            } else if let Some(arr) = item.as_array_mut() {
                // User's inline `event = [{ hooks = … }]`: replace ours (if present and
                // different) and append the freshly rendered inline table.
                let already_current = arr.iter().any(|e| {
                    e.as_inline_table()
                        .and_then(|t| t.get("hooks"))
                        .is_some_and(|h| inner_hooks_value_matches(h, commands))
                });
                if !already_current {
                    arr.retain(|e| {
                        !e.as_inline_table()
                            .and_then(|t| t.get("hooks"))
                            .is_some_and(inner_hooks_are_ours_value)
                    });
                    arr.push(group_to_inline(&group));
                }
                Ok(())
            } else {
                Err(CodexMergeError::UnmergeableShape(format!("hooks.{event}")))
            }
        }
    }
}

/// `inner_hooks_are_ours` for a `toml_edit::Value` (the inline-array element case).
fn inner_hooks_are_ours_value(v: &toml_edit::Value) -> bool {
    if let Some(arr) = v.as_array() {
        return arr.iter().any(|e| {
            e.as_inline_table()
                .and_then(|t| t.get("command"))
                .and_then(|c| c.as_str())
                .is_some_and(command_is_ours)
        });
    }
    false
}

/// Render our standalone group `Table` as an inline `toml_edit::Value` (for the inline-array form).
fn group_to_inline(group: &TomlTable) -> toml_edit::Value {
    let mut inner_arr = Array::new();
    if let Some(aot) = group.get("hooks").and_then(|h| h.as_array_of_tables()) {
        for t in aot.iter() {
            let mut it = toml_edit::InlineTable::new();
            for (k, v) in t.iter() {
                if let Some(val) = v.as_value() {
                    it.insert(k, val.clone());
                }
            }
            inner_arr.push(toml_edit::Value::InlineTable(it));
        }
    }
    let mut outer = toml_edit::InlineTable::new();
    outer.insert("hooks", toml_edit::Value::Array(inner_arr));
    toml_edit::Value::InlineTable(outer)
}

/// Merge DontSpeak's Codex hooks (SessionStart → notify --greet-only, UserPromptSubmit →
/// notify + provide, Stop → notify) into a Codex
/// `config.toml` (its text), preserving every other key. ADDITIVE + idempotent per event,
/// and REPLACE-OURS (not keep-if-present, see `append_to_event`): a re-wire after the
/// resolved `dontspeak` path changes — or after the per-event verb set changes — updates
/// the existing group instead of leaving a stale, dead one in place. `bin` is the absolute
/// path to the `dontspeak` binary; each command Codex runs is `"<bin>" <verb> --client <client>`.
/// `client` is the [`ClientSource`] whose config this is (always `Codex` today — passed in
/// rather than hardcoded, so this shaper stays client-agnostic like every other one).
pub fn merge_codex_hooks(
    existing: &str,
    bin: &str,
    client: ClientSource,
) -> Result<String, CodexMergeError> {
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing.parse()?
    };
    {
        let htbl = hooks_table(&mut doc)?;
        for (event, verbs) in CODEX_HOOKS {
            let commands: Vec<(String, i64)> = verbs
                .iter()
                .map(|(verb, timeout)| (codex_command(bin, verb, client), *timeout))
                .collect();
            let cmd_strings: Vec<String> = commands.iter().map(|(c, _)| c.clone()).collect();
            append_to_event(htbl, event, codex_our_group(&commands), &cmd_strings)?;
        }
    }
    Ok(doc.to_string())
}

/// Remove EVERY DontSpeak hook group from a Codex `config.toml`, across all events, dropping
/// an event (and the `hooks` table) that becomes empty. Leaves all other config untouched.
pub fn strip_codex_hooks(existing: &str) -> Result<String, CodexMergeError> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let mut doc: DocumentMut = existing.parse()?;
    let Some(htbl) = doc.get_mut("hooks").and_then(|h| h.as_table_mut()) else {
        return Ok(doc.to_string()); // no `hooks` table (or a scalar) → nothing of ours
    };
    let events: Vec<String> = htbl.iter().map(|(k, _)| k.to_string()).collect();
    for event in events {
        let drop_event = match htbl.get_mut(&event) {
            Some(item) if item.is_array_of_tables() => {
                let aot = item.as_array_of_tables_mut().expect("checked");
                aot.retain(|g| !codex_group_is_ours(g));
                aot.is_empty()
            }
            Some(item) if item.as_array().is_some() => {
                let arr = item.as_array_mut().expect("checked");
                arr.retain(|e| {
                    !e.as_inline_table()
                        .and_then(|t| t.get("hooks"))
                        .is_some_and(inner_hooks_are_ours_value)
                });
                arr.is_empty()
            }
            _ => false,
        };
        if drop_event {
            htbl.remove(&event);
        }
    }
    if htbl.is_empty() {
        doc.as_table_mut().remove("hooks");
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "/home/u/.local/bin/dontspeak";

    fn merged(existing: &str) -> String {
        merge_codex_hooks(existing, BIN, ClientSource::Codex).expect("merge ok")
    }

    /// The command string Codex should carry for `verb` on THIS host — including the uniform
    /// `--client codex` tail every wired verb now carries. (The dialect itself is pinned
    /// per-flavor by `wire::cmdline`'s tests; these pin Codex's structure.)
    fn cmd(verb: &str) -> String {
        codex_command(BIN, verb, ClientSource::Codex)
    }

    /// EVERY inner command string wired on `event` in a parsed doc, in order — asserting
    /// along the way that the event holds exactly one group, so callers comparing the
    /// returned commands also pin "not duplicated". Exact-string comparison is deliberate:
    /// a substring check like `contains("\" notify")` stopped discriminating the moment
    /// SessionStart's `notify --greet-only` entered the set (it contains ` notify`).
    fn event_commands(doc: &DocumentMut, event: &str) -> Vec<String> {
        let groups = doc["hooks"][event]
            .as_array_of_tables()
            .unwrap_or_else(|| panic!("{event}: array-of-tables"));
        assert_eq!(groups.len(), 1, "{event}: exactly one group");
        let inner = groups.iter().next().unwrap()["hooks"]
            .as_array_of_tables()
            .unwrap_or_else(|| panic!("{event}: inner hooks array-of-tables"));
        inner
            .iter()
            .map(|t| {
                t["command"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{event}: command is a string"))
                    .to_string()
            })
            .collect()
    }

    /// The ONE inner command on a single-verb event (SessionStart / Stop) — additionally
    /// pins "exactly one inner hook" for those events.
    fn event_command(doc: &DocumentMut, event: &str) -> String {
        let mut cmds = event_commands(doc, event);
        assert_eq!(cmds.len(), 1, "{event}: exactly one inner hook");
        cmds.remove(0)
    }

    #[test]
    fn merge_into_empty_wires_all_events() {
        let out = merged("");
        // Round-trips to valid TOML.
        let doc: DocumentMut = out.parse().unwrap();
        // Exact per-event command strings (see `event_commands` on why not substrings).
        // UserPromptSubmit is ONE group with TWO inner hooks: the mark-active/session-
        // re-discovery `notify` plus the narration-spec `provide`.
        assert_eq!(
            event_command(&doc, "SessionStart"),
            cmd("notify --greet-only")
        );
        assert_eq!(
            event_commands(&doc, "UserPromptSubmit"),
            vec![cmd("notify"), cmd("provide")]
        );
        assert_eq!(event_command(&doc, "Stop"), cmd("notify"));
    }

    #[test]
    fn merge_preserves_existing_and_is_idempotent() {
        let existing = "model = \"o4\"\n\n[tui]\ntheme = \"dark\"\n";
        let once = merged(existing);
        assert!(once.contains("model = \"o4\""), "unrelated key preserved");
        assert!(
            once.contains("theme = \"dark\""),
            "unrelated table preserved"
        );
        // Re-merging must not duplicate our groups.
        let twice = merged(&once);
        assert_eq!(once, twice, "idempotent");
        // Exactly one group per event, each with its exact command list (`event_commands`
        // asserts the one-group shape, so this also pins "no duplicates").
        let doc: DocumentMut = twice.parse().unwrap();
        assert_eq!(
            event_command(&doc, "SessionStart"),
            cmd("notify --greet-only")
        );
        assert_eq!(
            event_commands(&doc, "UserPromptSubmit"),
            vec![cmd("notify"), cmd("provide")]
        );
        assert_eq!(event_command(&doc, "Stop"), cmd("notify"));
    }

    #[test]
    fn legacy_quoted_group_is_still_ours_so_strip_removes_it_and_merge_heals_it() {
        // THE RECOGNISER-MIGRATION REGRESSION TEST. Every Codex config in the field was
        // written by an older DontSpeak in the always-quoted dialect. The shared
        // `command_is_ours` must still see those entries as OURS — if it didn't, unwire would
        // strand them in the user's config forever, and a re-wire would append a second group
        // beside them (two SessionStart greets, two Stop narrations) instead of healing them
        // to the new quote-free Windows form.
        let legacy = format!(
            "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"\\\"{BIN}\\\" notify\"\ntimeout = 1800\n"
        );

        // Unwire: the legacy group is recognised and removed, not stranded.
        let stripped = strip_codex_hooks(&legacy).expect("strip ok");
        assert!(
            !stripped.contains("dontspeak"),
            "legacy quoted group must be removed on unwire, got: {stripped}"
        );

        // Re-wire: REPLACED in place, not duplicated — `event_commands` pins exactly one group
        // and exactly one inner hook on Stop.
        let doc: DocumentMut = merged(&legacy).parse().expect("merge round-trips");
        assert_eq!(
            event_commands(&doc, "Stop"),
            vec![cmd("notify")],
            "the legacy quoted entry is healed to this host's dialect, not duplicated"
        );
    }

    #[test]
    fn merge_keeps_a_users_own_hook_on_the_same_event() {
        let existing = "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"/usr/bin/true\"\n";
        let out = merged(existing);
        assert!(out.contains("/usr/bin/true"), "user's Stop hook survives");
        // Assert on the PARSED doc, not the raw text: the rendered command is host-dependent
        // (quote-free on Windows), so a hardcoded quoted substring would pass on Linux CI and
        // fail on a Windows host. (Two groups here — the user's and ours — so `event_commands`,
        // which pins exactly one, doesn't apply.)
        let doc: DocumentMut = out.parse().unwrap();
        assert!(
            stop_has_command(&doc, &cmd("notify")),
            "ours added alongside"
        );
    }

    /// Does ANY group on `Stop` hold an inner hook with exactly this command? Used where the
    /// event legitimately holds more than one group (the user's + ours).
    fn stop_has_command(doc: &DocumentMut, command: &str) -> bool {
        doc["hooks"]["Stop"]
            .as_array_of_tables()
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    g["hooks"].as_array_of_tables().is_some_and(|inner| {
                        inner.iter().any(|t| t["command"].as_str() == Some(command))
                    })
                })
            })
    }

    #[test]
    fn user_hook_containing_dontspeak_substring_is_not_misidentified() {
        // Empirically reproduced bug: a user's own unrelated hook whose command merely
        // CONTAINS the substring "dontspeak" (e.g. a personal script's path component) was
        // misidentified as ours by a raw `.contains("dontspeak")` check. That misidentification
        // caused `merge_codex_hooks` to silently skip wiring our real hook (the event looked
        // "already ours") — and separately made `strip_codex_hooks` delete the user's ENTIRE
        // hook group on unwire, once collapsing a user's whole config.toml to empty. Neither
        // must happen: the identity check is now a precise binary-basename match.
        let existing = "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"/home/u/bin/my-dontspeak-checker\"\n";
        let out = merged(existing);
        assert!(
            out.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives merge, untouched"
        );
        // Host-dependent command string ⇒ assert on the parsed doc, never a quoted substring.
        let doc: DocumentMut = out.parse().unwrap();
        let stop = doc["hooks"]["Stop"].as_array_of_tables().unwrap();
        assert_eq!(
            stop.len(),
            2,
            "user's look-alike group + ours, not merged/skipped into one"
        );
        assert!(
            stop_has_command(&doc, &cmd("notify")),
            "our real hook IS wired alongside — not skipped as already-present"
        );

        // Stripping must remove ONLY ours, leaving the user's look-alike hook intact — not
        // the "collapsed the whole config.toml to empty" regression.
        let stripped = strip_codex_hooks(&out).unwrap();
        assert!(
            stripped.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives strip"
        );
        assert!(!stripped.contains(&cmd("notify")), "ours removed");
    }

    #[test]
    fn rewire_heals_a_changed_binary_path_by_replacing_the_stale_group() {
        // Unlike the Claude Code JSON path (which explicitly replaces a stale "ours" group),
        // `append_to_event` used to only append when NO existing group looked like ours —
        // so a re-wire after the resolved `dontspeak` path changed (e.g. an install-layout
        // upgrade) produced byte-identical output, leaving every hook pointed at a dead path.
        let first = merged("");
        let new_bin = "/opt/dontspeak/bin/dontspeak";
        let second = merge_codex_hooks(&first, new_bin, ClientSource::Codex).expect("merge ok");
        assert!(
            !second.contains(BIN),
            "stale bin path healed away, not left stale"
        );
        assert!(
            second.contains(new_bin),
            "re-wire updates the command to the new binary path"
        );
        // Exactly one group per event — replaced, not duplicated alongside the stale one.
        let doc: DocumentMut = second.parse().unwrap();
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            let aot = doc["hooks"][event].as_array_of_tables().unwrap();
            assert_eq!(
                aot.len(),
                1,
                "{event}: stale group replaced, not duplicated"
            );
        }
    }

    #[test]
    fn sessionstart_is_greet_only_while_stop_is_plain_notify() {
        // Pins the witness-seed invariant: on a NON-streaming client (no `MessageDisplay`),
        // a plain `notify` at SessionStart would seed the streaming witness and suppress ALL
        // Stop narration — the Qwen bug. So Codex's SessionStart must carry `--greet-only`,
        // while Stop must stay a flag-free plain `notify` (it's Codex's only narration path).
        // Every command now ENDS with the uniform `--client codex` tail, so these assert on
        // the greet-only flag's presence/absence rather than on the string's tail.
        let doc: DocumentMut = merged("").parse().unwrap();
        let ss = event_command(&doc, "SessionStart");
        assert!(
            ss.ends_with(" notify --greet-only --client codex"),
            "SessionStart is greet-only, got {ss}"
        );
        let stop = event_command(&doc, "Stop");
        assert!(
            stop.ends_with(" notify --client codex") && !stop.contains("--greet-only"),
            "Stop is plain notify with NO greet-only flag, got {stop}"
        );
    }

    #[test]
    fn userpromptsubmit_group_holds_notify_then_provide_with_timeouts() {
        // The two-inner-hook group in full: notify (MarkActive — per-terminal routing +
        // the engine's codex_stream session re-discovery after a restart) FIRST, then the
        // synchronous provide (the narration spec Codex blocks on). Both explicit 5 s
        // ceilings, both type = "command", NO `async` flag anywhere (Codex skips
        // `async = true` hooks outright).
        let doc: DocumentMut = merged("").parse().unwrap();
        let groups = doc["hooks"]["UserPromptSubmit"]
            .as_array_of_tables()
            .unwrap();
        assert_eq!(groups.len(), 1, "one group of ours");
        let inner = groups.iter().next().unwrap()["hooks"]
            .as_array_of_tables()
            .unwrap();
        let hooks: Vec<_> = inner.iter().collect();
        assert_eq!(hooks.len(), 2, "notify + provide in the ONE group");
        assert_eq!(hooks[0]["command"].as_str().unwrap(), cmd("notify"));
        assert_eq!(hooks[1]["command"].as_str().unwrap(), cmd("provide"));
        for h in &hooks {
            assert_eq!(h["type"].as_str(), Some("command"));
            assert_eq!(h["timeout"].as_integer(), Some(5));
            assert!(
                h.get("async").is_none(),
                "Codex skips async hooks — never emit the flag"
            );
        }
    }

    #[test]
    fn rewire_heals_an_old_single_verb_userpromptsubmit_group() {
        // An install wired by the previous version holds ONLY `provide` on
        // UserPromptSubmit. A plain re-wire must REPLACE that group with the current
        // notify+provide one (exact-list matching reads it as "differs"), not keep it as
        // "already ours" — otherwise existing installs would never gain mark-active
        // routing / engine session re-discovery.
        let old = format!(
            "[[hooks.UserPromptSubmit]]\n[[hooks.UserPromptSubmit.hooks]]\ntype = \"command\"\ncommand = \"\\\"{BIN}\\\" provide\"\ntimeout = 5\n"
        );
        let out = merged(&old);
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(
            event_commands(&doc, "UserPromptSubmit"),
            vec![cmd("notify"), cmd("provide")],
            "stale one-verb group healed to the two-verb shape, not duplicated"
        );
    }

    #[test]
    fn rewire_heals_a_group_wired_without_the_client_token() {
        // SELF-HEALING (not backward compat): a config wired by the build immediately before
        // the `--client` token existed holds client-less commands. `codex_group_matches` is an
        // exact-command-list comparison, so that group correctly reads "differs" and is
        // REPLACED — one group, now carrying the token — rather than duplicated beside a fresh
        // one (which would greet twice and narrate twice). The engine re-wires every client at
        // boot, so this converges without the user doing anything.
        let old = format!(
            "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"\\\"{BIN}\\\" notify\"\ntimeout = 1800\n"
        );
        let doc: DocumentMut = merged(&old).parse().expect("merge round-trips");
        assert_eq!(
            event_commands(&doc, "Stop"),
            vec![cmd("notify")],
            "the client-less group is healed to the token-carrying command, not duplicated"
        );
        assert!(
            cmd("notify").ends_with(" notify --client codex"),
            "…and the healed command really carries the token"
        );
    }

    #[test]
    fn strip_removes_the_two_verb_group_cleanly() {
        let stripped = strip_codex_hooks(&merged("")).unwrap();
        assert!(
            !stripped.contains("dontspeak"),
            "both inner hooks removed with the group"
        );
        assert!(
            !stripped.contains("UserPromptSubmit"),
            "empty event dropped"
        );
    }

    #[test]
    fn preexisting_hooks_subtable_survives_merge_and_strip() {
        // Codex may keep its own bookkeeping under the same `[hooks]` table (e.g. a
        // `[hooks.state]` sub-table holding a `trusted_hash`). Merging must wire our event
        // groups WITHOUT disturbing that sub-table, and stripping must remove only our
        // groups — keeping `[hooks]` alive because `state` remains under it.
        let existing = "[hooks.state]\ntrusted_hash = \"abc123\"\n";
        let out = merged(existing);
        assert!(
            out.contains("[hooks.state]") && out.contains("trusted_hash = \"abc123\""),
            "state sub-table survives merge byte-wise"
        );
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(
            doc["hooks"]["state"]["trusted_hash"].as_str(),
            Some("abc123"),
            "state sub-table survives merge structurally"
        );
        // Our three events wired alongside it (shape asserted by `event_commands`).
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            assert!(!event_commands(&doc, event).is_empty());
        }

        let stripped = strip_codex_hooks(&out).unwrap();
        assert!(!stripped.contains("dontspeak"), "all ours removed");
        assert!(
            stripped.contains("trusted_hash = \"abc123\""),
            "state sub-table survives strip"
        );
        let doc: DocumentMut = stripped.parse().unwrap();
        assert_eq!(
            doc["hooks"]["state"]["trusted_hash"].as_str(),
            Some("abc123"),
            "`[hooks]` kept — `state` still lives under it"
        );
    }

    #[test]
    fn strip_removes_only_ours_and_drops_empty_events() {
        let merged_doc = merged(
            "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"/usr/bin/true\"\n",
        );
        let stripped = strip_codex_hooks(&merged_doc).unwrap();
        assert!(stripped.contains("/usr/bin/true"), "user hook kept");
        assert!(!stripped.contains("dontspeak"), "all ours removed");
        // UserPromptSubmit was ours-only → dropped entirely.
        assert!(
            !stripped.contains("UserPromptSubmit"),
            "ours-only event removed"
        );
    }

    #[test]
    fn unmergeable_scalar_hooks_errors() {
        let bad = "hooks = \"oops\"\n";
        assert!(matches!(
            merge_codex_hooks(bad, BIN, ClientSource::Codex),
            Err(CodexMergeError::UnmergeableShape(_))
        ));
    }

    #[test]
    fn parse_error_surfaces() {
        let bad = "this is = = not toml\n";
        assert!(matches!(
            merge_codex_hooks(bad, BIN, ClientSource::Codex),
            Err(CodexMergeError::Parse(_))
        ));
    }
}
