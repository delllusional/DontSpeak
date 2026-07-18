//! Kimi Code hooks (`~/.kimi-code/config.toml`) — a FLAT top-level `[[hooks]]`
//! array-of-tables, TOML via `toml_edit`. Stdin payloads carry snake_case `hook_event_name`
//! (the runtime parses it already); commands use the shared quote-free inline-shell dialect
//! ([`cmdline`](super::cmdline)).
//!
//! HARD CONSTRAINT: a `[[hooks]]` entry may carry ONLY `event` / `matcher` / `command` /
//! `timeout` — any extra key (`async`, `args`, `shell`, …) makes Kimi fail to load the whole
//! config. We emit `event`/`command`/`timeout` and never `matcher`; timeouts are SECONDS.
//!
//! Events (non-streaming client — Kimi has no MessageDisplay):
//! - `SessionStart` → `notify --greet-only` (a plain `notify` would seed the streaming witness
//!   and suppress Stop narration; see `codex.rs`).
//! - `SessionEnd` → `notify` (per-session cleanup).
//! - `UserPromptSubmit` → `notify` (MarkActive + session re-discovery) + `provide` (spec).
//! - `Stop` → `notify` (voices the reply).
//! - `Notification` → `notify`.
//!
//! Additive/idempotent: "ours" = the command's binary basename is `dontspeak`
//! ([`command_is_ours`]), not a substring — the same precision argument as `codex.rs`.

use super::cmdline::{ShellOverride, command_is_ours, host_inline_flavor, inline_command};
use ds_client::ClientSource;
use toml_edit::{ArrayOfTables, DocumentMut, Item as TomlItem, Table as TomlTable, value};

/// Render one Kimi hook command. Kimi's entry schema has no `shell`/`args` field, so the verb
/// and the uniform `--client <token>` tail are inlined into the command string and a spaced
/// bin path resolves to the 8.3 short name — the Codex ([`ShellOverride::Unsupported`]) case
/// of [`inline_command`].
fn kimi_command(bin: &str, verb: &str, client: ClientSource) -> String {
    inline_command(
        host_inline_flavor(),
        bin,
        [verb, "--client", client.as_str()],
        ShellOverride::Unsupported,
    )
    .0
}

/// The `(event, [(verb, timeout)])` hooks we wire into Kimi Code — ONE `[[hooks]]` entry per
/// verb (the array is flat; a multi-verb event like UserPromptSubmit is two sibling entries).
/// All timeouts are SECONDS and every entry runs synchronously (there is no `async` key to
/// emit — the hard key constraint above forbids it). Kimi caps `timeout` at 600s and rejects
/// the whole config above that (`kimi doctor`: "expected number to be <=600"), so Stop gets
/// the cap rather than the 1800s Claude/Qwen use.
const KIMI_HOOKS: &[(&str, &[(&str, i64)])] = &[
    ("SessionStart", &[("notify --greet-only", 30)]),
    ("SessionEnd", &[("notify", 30)]),
    ("UserPromptSubmit", &[("notify", 5), ("provide", 5)]),
    ("Stop", &[("notify", 600)]),
    ("Notification", &[("notify", 30)]),
];

/// Why a [`merge_kimi_hooks`]/[`strip_kimi_hooks`] call could not apply. Same caller contract
/// as [`super::codex::CodexMergeError`]: both variants are non-success — never report a silent
/// success.
#[derive(Debug)]
pub enum KimiMergeError {
    /// The file is not valid TOML (passes through `toml_edit`'s parse error).
    Parse(toml_edit::TomlError),
    /// The file is valid TOML, but `hooks` has a shape we can neither append to nor safely
    /// coerce (e.g. `hooks = "x"` or a `[hooks]` table). We do NOT clobber the user's file;
    /// we report so the installer can warn instead of claiming success.
    UnmergeableShape(String),
}

impl std::fmt::Display for KimiMergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KimiMergeError::Parse(e) => write!(f, "config.toml is not valid TOML: {e}"),
            KimiMergeError::UnmergeableShape(s) => {
                write!(
                    f,
                    "config.toml has an unexpected `{s}` shape; left unchanged (Kimi hooks NOT wired)"
                )
            }
        }
    }
}

impl std::error::Error for KimiMergeError {}

impl From<toml_edit::TomlError> for KimiMergeError {
    fn from(e: toml_edit::TomlError) -> Self {
        KimiMergeError::Parse(e)
    }
}

/// True if this `[[hooks]]` entry's `command` invokes OUR `dontspeak` binary — the shared
/// [`command_is_ours`] basename match (deliberately NOT a substring check: see `codex.rs` for
/// the misidentification bug that caused on merge AND unwire).
fn kimi_entry_is_ours(entry: &TomlTable) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(command_is_ours)
}

/// True if this entry is ALREADY exactly one desired `(event, command, timeout)` — the
/// identical-content check that makes an unchanged re-wire a true byte-for-byte no-op (see
/// `codex_group_matches` for the same pattern). Exact matching also self-heals entries wired
/// by an older dialect (quoted path, no `--client` tail): they read "differs" and are replaced.
fn kimi_entry_matches(entry: &TomlTable, event: &str, command: &str, timeout: i64) -> bool {
    entry.get("event").and_then(|v| v.as_str()) == Some(event)
        && entry.get("command").and_then(|v| v.as_str()) == Some(command)
        && entry.get("timeout").and_then(|v| v.as_integer()) == Some(timeout)
}

/// One `[[hooks]]` entry — exactly the three keys Kimi's schema allows us to emit.
fn kimi_entry(event: &str, command: &str, timeout: i64) -> TomlTable {
    let mut entry = TomlTable::new();
    entry.insert("event", value(event));
    entry.insert("command", value(command));
    entry.insert("timeout", value(timeout));
    entry
}

/// The full desired DontSpeak entry set, in `KIMI_HOOKS` order.
fn desired_entries(bin: &str, client: ClientSource) -> Vec<(String, String, i64)> {
    KIMI_HOOKS
        .iter()
        .flat_map(|(event, verbs)| {
            verbs.iter().map(move |(verb, timeout)| {
                (
                    (*event).to_string(),
                    kimi_command(bin, verb, client),
                    *timeout,
                )
            })
        })
        .collect()
}

/// Merge DontSpeak's Kimi Code hooks into a `config.toml` (its text), preserving every other
/// key. ADDITIVE + idempotent, and REPLACE-OURS (same self-healing contract as
/// [`super::codex::merge_codex_hooks`]): a re-wire after the resolved `dontspeak` path or the
/// wired verb set changes replaces the stale entries instead of duplicating them. A user's own
/// `[[hooks]]` entries (any event) are never touched.
pub fn merge_kimi_hooks(
    existing: &str,
    bin: &str,
    client: ClientSource,
) -> Result<String, KimiMergeError> {
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing.parse()?
    };
    let desired = desired_entries(bin, client);
    match doc.get_mut("hooks") {
        None => {
            let mut aot = ArrayOfTables::new();
            for (event, command, timeout) in &desired {
                aot.push(kimi_entry(event, command, *timeout));
            }
            doc.insert("hooks", TomlItem::ArrayOfTables(aot));
        }
        Some(item) => {
            let aot = item
                .as_array_of_tables_mut()
                .ok_or_else(|| KimiMergeError::UnmergeableShape("hooks".into()))?;
            let already_current = {
                let ours: Vec<&TomlTable> = aot.iter().filter(|t| kimi_entry_is_ours(t)).collect();
                ours.len() == desired.len()
                    && ours
                        .iter()
                        .zip(&desired)
                        .all(|(t, (e, c, to))| kimi_entry_matches(t, e, c, *to))
            };
            if !already_current {
                aot.retain(|t| !kimi_entry_is_ours(t));
                for (event, command, timeout) in &desired {
                    aot.push(kimi_entry(event, command, *timeout));
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// Remove EVERY DontSpeak `[[hooks]]` entry from a Kimi Code `config.toml`, dropping the
/// `hooks` array if it becomes empty. Leaves all other config — including the user's own
/// hook entries — untouched.
pub fn strip_kimi_hooks(existing: &str) -> Result<String, KimiMergeError> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let mut doc: DocumentMut = existing.parse()?;
    let Some(item) = doc.get_mut("hooks") else {
        return Ok(doc.to_string()); // no `hooks` key → nothing of ours
    };
    let Some(aot) = item.as_array_of_tables_mut() else {
        // A scalar/table `hooks` can't hold entries we wrote → leave the file alone.
        return Ok(doc.to_string());
    };
    aot.retain(|t| !kimi_entry_is_ours(t));
    if aot.is_empty() {
        doc.as_table_mut().remove("hooks");
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "/home/u/.local/bin/dontspeak";

    fn merged(existing: &str) -> String {
        merge_kimi_hooks(existing, BIN, ClientSource::KimiCode).expect("merge ok")
    }

    /// The command string Kimi should carry for `verb` — including the uniform
    /// `--client kimi_code` tail (the dialect itself is pinned per-flavor by
    /// `wire::cmdline`'s tests; these pin Kimi's structure).
    fn cmd(verb: &str) -> String {
        kimi_command(BIN, verb, ClientSource::KimiCode)
    }

    /// Every DontSpeak-owned `[[hooks]]` entry in a parsed doc as `(event, command, timeout)`,
    /// in file order — asserting along the way that each entry carries EXACTLY the allowed
    /// keys (the hard Kimi schema constraint).
    fn our_entries(doc: &DocumentMut) -> Vec<(String, String, i64)> {
        let aot = doc["hooks"]
            .as_array_of_tables()
            .expect("hooks: array-of-tables");
        aot.iter()
            .filter(|t| kimi_entry_is_ours(t))
            .map(|t| {
                let keys: Vec<&str> = t.iter().map(|(k, _)| k).collect();
                assert_eq!(
                    keys,
                    ["event", "command", "timeout"],
                    "entry keys are exactly event/command/timeout — no matcher/async/args: {t}"
                );
                (
                    t["event"].as_str().expect("event is a string").to_string(),
                    t["command"]
                        .as_str()
                        .expect("command is a string")
                        .to_string(),
                    t["timeout"].as_integer().expect("timeout is an integer"),
                )
            })
            .collect()
    }

    /// The full desired set for assertions, in wiring order.
    fn expected() -> Vec<(String, String, i64)> {
        desired_entries(BIN, ClientSource::KimiCode)
    }

    #[test]
    fn merge_into_empty_wires_all_events_with_exact_timeouts() {
        let out = merged("");
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(
            our_entries(&doc),
            vec![
                ("SessionStart".into(), cmd("notify --greet-only"), 30),
                ("SessionEnd".into(), cmd("notify"), 30),
                ("UserPromptSubmit".into(), cmd("notify"), 5),
                ("UserPromptSubmit".into(), cmd("provide"), 5),
                ("Stop".into(), cmd("notify"), 600),
                ("Notification".into(), cmd("notify"), 30),
            ]
        );
        // SessionStart is greet-only, Stop is plain notify — the witness-seed invariant
        // (Kimi is non-streaming; see codex.rs for the Qwen bug this avoids).
        let ss = &our_entries(&doc)[0];
        assert!(ss.1.ends_with(" notify --greet-only --client kimi_code"));
        let stop = &our_entries(&doc)[4];
        assert!(stop.1.ends_with(" notify --client kimi_code"));
        assert!(!stop.1.contains("--greet-only"));
    }

    #[test]
    fn merge_preserves_unrelated_hooks_and_config_and_is_idempotent() {
        let existing = "theme = \"dark\"\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"/usr/bin/true\"\ntimeout = 10\n";
        let once = merged(existing);
        assert!(once.contains("theme = \"dark\""), "unrelated key preserved");
        assert!(once.contains("/usr/bin/true"), "user's hook preserved");
        // Re-merging must be a byte-for-byte no-op (no duplicate entries).
        let twice = merged(&once);
        assert_eq!(once, twice, "idempotent");
        let doc: DocumentMut = twice.parse().unwrap();
        assert_eq!(our_entries(&doc), expected(), "no duplicates");
        // The user's entry is still there alongside ours.
        let all = doc["hooks"].as_array_of_tables().unwrap();
        assert_eq!(all.len(), expected().len() + 1);
    }

    #[test]
    fn user_hook_containing_dontspeak_substring_is_not_misidentified() {
        // Same regression class as codex.rs: a user's command that merely CONTAINS the
        // substring "dontspeak" must not be seen as ours (would skip wiring AND delete the
        // user's entry on strip).
        let existing = "[[hooks]]\nevent = \"Stop\"\ncommand = \"/home/u/bin/my-dontspeak-checker\"\ntimeout = 5\n";
        let out = merged(existing);
        assert!(
            out.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives merge"
        );
        let doc: DocumentMut = out.parse().unwrap();
        assert_eq!(our_entries(&doc), expected(), "our hooks wired alongside");

        let stripped = strip_kimi_hooks(&out).unwrap();
        assert!(
            stripped.contains("/home/u/bin/my-dontspeak-checker"),
            "user's look-alike hook survives strip"
        );
        let doc: DocumentMut = stripped.parse().unwrap();
        assert!(our_entries(&doc).is_empty(), "ours removed");
    }

    #[test]
    fn rewire_heals_a_changed_binary_path_by_replacing_stale_entries() {
        let first = merged("");
        let new_bin = "/opt/dontspeak/bin/dontspeak";
        let second = merge_kimi_hooks(&first, new_bin, ClientSource::KimiCode).expect("merge ok");
        assert!(!second.contains(BIN), "stale bin path healed away");
        assert!(second.contains(new_bin), "re-wire re-points the command");
        let doc: DocumentMut = second.parse().unwrap();
        assert_eq!(
            our_entries(&doc),
            desired_entries(new_bin, ClientSource::KimiCode),
            "stale entries replaced, not duplicated"
        );
    }

    #[test]
    fn legacy_quoted_entry_is_still_ours_so_strip_removes_it_and_merge_heals_it() {
        // Every dialect `command_is_ours` accepts must heal on re-wire and strip on unwire —
        // not strand or duplicate.
        let legacy = format!(
            "[[hooks]]\nevent = \"Stop\"\ncommand = \"\\\"{BIN}\\\" notify\"\ntimeout = 1800\n"
        );
        let stripped = strip_kimi_hooks(&legacy).expect("strip ok");
        assert!(
            !stripped.contains("dontspeak"),
            "legacy quoted entry removed on unwire, got: {stripped}"
        );
        let doc: DocumentMut = merged(&legacy).parse().expect("merge round-trips");
        assert_eq!(our_entries(&doc), expected(), "healed, not duplicated");
    }

    #[test]
    fn emitted_entries_carry_exactly_the_allowed_keys() {
        // THE HARD CONSTRAINT: Kimi rejects a config whose `[[hooks]]` entry holds any key
        // outside event/matcher/command/timeout. `our_entries` already asserts the exact key
        // set; this test makes the forbidden keys explicit.
        let doc: DocumentMut = merged("").parse().unwrap();
        for entry in doc["hooks"].as_array_of_tables().unwrap().iter() {
            for forbidden in ["matcher", "async", "args", "shell", "type"] {
                assert!(
                    entry.get(forbidden).is_none(),
                    "never emit `{forbidden}`: {entry}"
                );
            }
        }
    }

    #[test]
    fn strip_removes_only_ours_and_drops_the_empty_hooks_array() {
        let stripped = strip_kimi_hooks(&merged(
            "[[hooks]]\nevent = \"Stop\"\ncommand = \"/usr/bin/true\"\ntimeout = 10\n",
        ))
        .unwrap();
        assert!(stripped.contains("/usr/bin/true"), "user hook kept");
        assert!(!stripped.contains("dontspeak"), "all ours removed");
        // The user entry remains, so `hooks` survives — but ours are gone from it.
        let doc: DocumentMut = stripped.parse().unwrap();
        assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 1);

        // Ours-only config: the emptied `hooks` array is dropped entirely.
        let stripped = strip_kimi_hooks(&merged("")).unwrap();
        assert!(
            !stripped.contains("hooks"),
            "empty hooks array dropped: {stripped}"
        );
    }

    #[test]
    fn strip_on_empty_input_is_a_noop() {
        assert_eq!(strip_kimi_hooks("").unwrap(), "");
    }

    #[test]
    fn unmergeable_scalar_hooks_errors() {
        let bad = "hooks = \"oops\"\n";
        assert!(matches!(
            merge_kimi_hooks(bad, BIN, ClientSource::KimiCode),
            Err(KimiMergeError::UnmergeableShape(_))
        ));
    }

    #[test]
    fn parse_error_surfaces() {
        let bad = "this is = = not toml\n";
        assert!(matches!(
            merge_kimi_hooks(bad, BIN, ClientSource::KimiCode),
            Err(KimiMergeError::Parse(_))
        ));
    }
}
