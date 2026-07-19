//! Kimi Code hooks (`~/.kimi-code/config.toml`) — flat top-level `[[hooks]]` via `toml_edit`.
//! Commands use the shared quote-free inline-shell dialect ([`cmdline`](super::cmdline)).
//!
//! HARD CONSTRAINT: a `[[hooks]]` entry may carry ONLY `event` / `matcher` / `command` /
//! `timeout` — any extra key makes Kimi fail to load the whole config. We emit
//! `event`/`command`/`timeout` (never `matcher`); timeouts are SECONDS.
//!
//! Non-streaming (no MessageDisplay): SessionStart → `notify --greet-only` (plain `notify`
//! would seed the streaming witness and suppress Stop; see `codex.rs`); SessionEnd/Stop/
//! Notification → `notify`; UserPromptSubmit → `notify` + `provide`.
//!
//! Additive/idempotent: "ours" = binary basename `dontspeak` ([`command_is_ours`]), not a
//! substring — same precision argument as `codex.rs`.

use super::cmdline::{ShellOverride, command_is_ours, host_inline_flavor, inline_command};
use ds_client::ClientSource;
use toml_edit::{ArrayOfTables, DocumentMut, Item as TomlItem, Table as TomlTable, value};

/// Kimi has no `shell`/`args` field → verbs + `--client` inlined; spaced bin → 8.3 short name
/// ([`ShellOverride::Unsupported`]).
fn kimi_command(bin: &str, verb: &str, client: ClientSource) -> String {
    inline_command(
        host_inline_flavor(),
        bin,
        [verb, "--client", client.as_str()],
        ShellOverride::Unsupported,
    )
    .0
}

/// One `[[hooks]]` entry per verb (flat array). Timeouts in SECONDS, synchronous (no `async`
/// key — hard constraint). Kimi caps `timeout` at 600s (`kimi doctor`); Stop uses the cap,
/// not the 1800s Claude/Qwen use.
const KIMI_HOOKS: &[(&str, &[(&str, i64)])] = &[
    ("SessionStart", &[("notify --greet-only", 30)]),
    ("SessionEnd", &[("notify", 30)]),
    ("UserPromptSubmit", &[("notify", 5), ("provide", 5)]),
    ("Stop", &[("notify", 600)]),
    ("Notification", &[("notify", 30)]),
];

/// Same caller contract as [`super::codex::CodexMergeError`]: both variants are non-success —
/// never report a silent success.
#[derive(Debug)]
pub enum KimiMergeError {
    Parse(toml_edit::TomlError),
    /// Valid TOML but `hooks` is neither appendable nor safely coerceable. Do NOT clobber;
    /// installer must warn, not claim success.
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

/// Basename match via [`command_is_ours`] — not substring (see `codex.rs`).
fn kimi_entry_is_ours(entry: &TomlTable) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(command_is_ours)
}

/// Exact content match → re-wire is byte-for-byte no-op; older dialects self-heal on replace.
fn kimi_entry_matches(entry: &TomlTable, event: &str, command: &str, timeout: i64) -> bool {
    entry.get("event").and_then(|v| v.as_str()) == Some(event)
        && entry.get("command").and_then(|v| v.as_str()) == Some(command)
        && entry.get("timeout").and_then(|v| v.as_integer()) == Some(timeout)
}

/// Exactly the three keys Kimi's schema allows us to emit.
fn kimi_entry(event: &str, command: &str, timeout: i64) -> TomlTable {
    let mut entry = TomlTable::new();
    entry.insert("event", value(event));
    entry.insert("command", value(command));
    entry.insert("timeout", value(timeout));
    entry
}

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

/// Additive + idempotent + REPLACE-OURS (self-heal when path/verbs change). User entries
/// never touched.
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

/// Drop every DontSpeak `[[hooks]]` entry; remove empty `hooks` array. User entries kept.
pub fn strip_kimi_hooks(existing: &str) -> Result<String, KimiMergeError> {
    if existing.trim().is_empty() {
        return Ok(existing.to_string());
    }
    let mut doc: DocumentMut = existing.parse()?;
    let Some(item) = doc.get_mut("hooks") else {
        return Ok(doc.to_string());
    };
    let Some(aot) = item.as_array_of_tables_mut() else {
        // Scalar/table `hooks` can't hold entries we wrote → leave alone.
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

    fn cmd(verb: &str) -> String {
        kimi_command(BIN, verb, ClientSource::KimiCode)
    }

    /// Ours entries as `(event, command, timeout)`; asserts allowed keys only.
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
        // Witness-seed invariant (Kimi non-streaming; see codex.rs).
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
        let twice = merged(&once);
        assert_eq!(once, twice, "idempotent");
        let doc: DocumentMut = twice.parse().unwrap();
        assert_eq!(our_entries(&doc), expected(), "no duplicates");
        let all = doc["hooks"].as_array_of_tables().unwrap();
        assert_eq!(all.len(), expected().len() + 1);
    }

    #[test]
    fn user_hook_containing_dontspeak_substring_is_not_misidentified() {
        // Substring "dontspeak" must not be ours (would skip wiring AND delete on strip).
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
        // Every dialect `command_is_ours` accepts must heal on re-wire and strip on unwire.
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
        // Hard constraint: any key outside event/matcher/command/timeout → Kimi rejects config.
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
        let doc: DocumentMut = stripped.parse().unwrap();
        assert_eq!(doc["hooks"].as_array_of_tables().unwrap().len(), 1);

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
