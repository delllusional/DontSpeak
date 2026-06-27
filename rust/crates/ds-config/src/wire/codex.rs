//! OpenAI Codex CLI hooks (`~/.codex/config.toml`).
//!
//! Codex grew a hooks system with the SAME contract as Claude Code's — events routed by
//! `hook_event_name`, one JSON object on stdin, `Stop` carrying `last_assistant_message`,
//! `UserPromptSubmit` honouring `hookSpecificOutput.additionalContext` — so the SAME
//! `dontspeak notify` / `dontspeak provide` binary handles them. Only two things differ from
//! the Claude Code wiring: the file is TOML (so we edit it with `toml_edit` to preserve the
//! user's tables + comments), and the per-event set is Codex-shaped:
//!
//!   * `UserPromptSubmit` → `dontspeak provide` — inject the narration spec so Codex WRITES
//!     the spoken-line blockquotes (without this it has nothing to speak).
//!   * `Stop` → `dontspeak notify` — speak the final reply. Codex has no `MessageDisplay`
//!     stream, so end-of-turn `Stop` (with `last_assistant_message`) is where it's voiced.
//!
//! Written as:
//!   [[hooks.Stop]]
//!   [[hooks.Stop.hooks]]
//!   type = "command"
//!   command = "\"…/dontspeak\" notify"
//!   timeout = 1800
//!
//! Additive + idempotent, mirroring `merge_hooks`: a group is "ours" if one of its inner
//! hook commands references the `dontspeak` binary.

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item as TomlItem, Table as TomlTable, value};

/// Marker identifying our Codex hook commands (idempotent merge + clean strip): `dontspeak` is
/// a substring of our `"…/dontspeak" notify|provide` commands.
const CODEX_HOOK_MARKER: &str = "dontspeak";

/// The `(event, verb, timeout)` hooks we wire into Codex. Codex has no MessageDisplay stream,
/// so the reply is voiced from `Stop`; and the narration spec is injected at `UserPromptSubmit`
/// via the synchronous `provide` verb (its stdout `additionalContext` is read by Codex).
const CODEX_HOOKS: &[(&str, &str, i64)] = &[("UserPromptSubmit", "provide", 5), ("Stop", "notify", 1800)];

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
                write!(f, "config.toml has an unexpected `{s}` shape; left unchanged (Codex hooks NOT wired)")
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

/// Does this inner hooks `Item` (the `[[hooks.<event>.hooks]]` array-of-tables OR an inline
/// `hooks = [{…}]` array) hold a `command` referencing our `dontspeak` binary?
fn inner_hooks_are_ours(item: &TomlItem) -> bool {
    let cmd_is_ours = |c: Option<&str>| c.is_some_and(|c| c.contains(CODEX_HOOK_MARKER));
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

/// Build our group (`{ hooks = [[…]] }`) for `command` as a standalone table.
fn codex_our_group(command: &str, timeout: i64) -> TomlTable {
    let mut inner = TomlTable::new();
    inner.insert("type", value("command"));
    inner.insert("command", value(command));
    inner.insert("timeout", value(timeout));
    let mut inner_aot = ArrayOfTables::new();
    inner_aot.push(inner);
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

/// Append our group to the `hooks.<event>` slot — handling both the array-of-tables form we
/// write and a user's inline `<event> = [{…}]` array. Returns `Err` only for a scalar shape we
/// can't coerce. A no-op (already ours) leaves the slot untouched.
fn append_to_event(htbl: &mut TomlTable, event: &str, group: TomlTable) -> Result<(), CodexMergeError> {
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
                if !aot.iter().any(codex_group_is_ours) {
                    aot.push(group);
                }
                Ok(())
            } else if let Some(arr) = item.as_array_mut() {
                // User's inline `event = [{ hooks = … }]`: append ours as an inline table.
                let already = arr.iter().any(|e| {
                    e.as_inline_table()
                        .and_then(|t| t.get("hooks"))
                        .is_some_and(inner_hooks_are_ours_value)
                });
                if !already {
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
                .is_some_and(|c| c.contains(CODEX_HOOK_MARKER))
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

/// Merge DontSpeak's Codex hooks (UserPromptSubmit → provide, Stop → notify) into a Codex
/// `config.toml` (its text), preserving every other key. ADDITIVE + idempotent per event:
/// if one of our groups is already on an event we leave that event untouched. `bin` is the
/// absolute path to the `dontspeak` binary; the command Codex runs is `"<bin>" <verb>`.
pub fn merge_codex_hooks(existing: &str, bin: &str) -> Result<String, CodexMergeError> {
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing.parse()?
    };
    {
        let htbl = hooks_table(&mut doc)?;
        for (event, verb, timeout) in CODEX_HOOKS {
            let command = format!("\"{bin}\" {verb}");
            append_to_event(htbl, event, codex_our_group(&command, *timeout))?;
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
        merge_codex_hooks(existing, BIN).expect("merge ok")
    }

    #[test]
    fn merge_into_empty_wires_both_events() {
        let out = merged("");
        assert!(out.contains("[[hooks.UserPromptSubmit]]") || out.contains("hooks.UserPromptSubmit"));
        assert!(out.contains("[[hooks.Stop]]") || out.contains("hooks.Stop"));
        assert!(out.contains("\"/home/u/.local/bin/dontspeak\" provide"));
        assert!(out.contains("\"/home/u/.local/bin/dontspeak\" notify"));
        // Round-trips to valid TOML.
        let _: DocumentMut = out.parse().unwrap();
    }

    #[test]
    fn merge_preserves_existing_and_is_idempotent() {
        let existing = "model = \"o4\"\n\n[tui]\ntheme = \"dark\"\n";
        let once = merged(existing);
        assert!(once.contains("model = \"o4\""), "unrelated key preserved");
        assert!(once.contains("theme = \"dark\""), "unrelated table preserved");
        // Re-merging must not duplicate our groups.
        let twice = merged(&once);
        assert_eq!(once, twice, "idempotent");
        assert_eq!(twice.matches("\"/home/u/.local/bin/dontspeak\" notify").count(), 1);
        assert_eq!(twice.matches("\"/home/u/.local/bin/dontspeak\" provide").count(), 1);
    }

    #[test]
    fn merge_keeps_a_users_own_hook_on_the_same_event() {
        let existing = "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"/usr/bin/true\"\n";
        let out = merged(existing);
        assert!(out.contains("/usr/bin/true"), "user's Stop hook survives");
        assert!(out.contains("\"/home/u/.local/bin/dontspeak\" notify"), "ours added alongside");
    }

    #[test]
    fn strip_removes_only_ours_and_drops_empty_events() {
        let merged_doc = merged("[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"/usr/bin/true\"\n");
        let stripped = strip_codex_hooks(&merged_doc).unwrap();
        assert!(stripped.contains("/usr/bin/true"), "user hook kept");
        assert!(!stripped.contains("dontspeak"), "all ours removed");
        // UserPromptSubmit was ours-only → dropped entirely.
        assert!(!stripped.contains("UserPromptSubmit"), "ours-only event removed");
    }

    #[test]
    fn unmergeable_scalar_hooks_errors() {
        let bad = "hooks = \"oops\"\n";
        assert!(matches!(merge_codex_hooks(bad, BIN), Err(CodexMergeError::UnmergeableShape(_))));
    }

    #[test]
    fn parse_error_surfaces() {
        let bad = "this is = = not toml\n";
        assert!(matches!(merge_codex_hooks(bad, BIN), Err(CodexMergeError::Parse(_))));
    }
}
