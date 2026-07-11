//! Hook-wiring building blocks for the [`wire`](crate) orchestrator — the two hook
//! MECHANISMS of the client registry (`ds_config::WireMechanism`): Claude-contract hooks in a
//! JSON settings file ([`claude_json_hooks`] — Claude Code's `~/.claude/settings.json`) and the
//! same contract in a format-preserved TOML config ([`claude_toml_hooks`] — Codex's
//! `~/.codex/config.toml`), plus the client-agnostic install housekeeping ([`seed_and_prune`]:
//! seed our `config.toml`, prune stale binaries). Both writers take the TARGET FILE as a
//! parameter — the registry resolves which client's file — so a new client reusing either
//! contract (e.g. Qwen Code) is a registry entry, not a new writer. The hook SETS + merges are
//! the ONE definition in `ds-config` (shared by every platform installer); binary-path
//! resolution, the JSON read, and the backup+atomic-write tail are the shared
//! `io` core (see `super::io`).
//!
//! Safe by construction: additive + idempotent merge (never duplicates ours, never clobbers the
//! user's own hooks/keys), a timestamped backup before writing, and a malformed OR unmergeable
//! existing file is left completely untouched (non-fatal, reported) rather than destroyed or
//! merged-as-empty. `print_only` emits the merged document without touching disk.

use super::io::{self, WriteBody};
use ds_config::{ClientSource, HookCommandStyle, HookSpec, INSTALLED_BINS, Paths};

/// Binary names this app has shipped and later dropped or renamed. The single-binary
/// consolidation replaced `ds-mcp`/`ds-speak`/`ds-narrate`; the standalone `dontspeakd` engine
/// binary was folded into `dontspeak` itself. A FINITE, EXPLICIT list — see [`is_stale_ds_bin`]
/// for why this is a list of known names rather than a `dontspeak*`/`ds-*` prefix match: a
/// shared install dir (e.g. `~/.local/bin`) is NOT exclusively ours, so "starts with the
/// prefix" is not an ownership signal. When a future bin is renamed/dropped, add its name here.
const KNOWN_LEGACY_BINS: &[&str] = &["ds-mcp", "ds-speak", "ds-narrate", "dontspeakd"];

/// PURE decision: is `name` a KNOWN-STALE DontSpeak binary — an exact match (modulo this
/// platform's exe suffix) against [`KNOWN_LEGACY_BINS`], and (defensively) not one of the
/// current [`INSTALLED_BINS`]? Deliberately NOT a `dontspeak*`/`ds-*` prefix check: that used to
/// flag ANY same-prefixed name not in the current-bins set as stale, which silently deleted
/// this app's OWN `dontspeak-uninstall` script (placed executable in the same install dir by
/// the installer, but not one of the four current bins) on every single wire, and would just as
/// happily have deleted an unrelated user tool like `~/.local/bin/ds-sync` — sharing a name
/// prefix is not an ownership check. Foreign tools, non-exe siblings (e.g. `ds_core.dll` on
/// Windows — wrong suffix), and anything not on the explicit legacy list are kept.
fn is_stale_ds_bin(name: &str) -> bool {
    match name.strip_suffix(std::env::consts::EXE_SUFFIX) {
        // EXE_SUFFIX is "" on unix, so strip_suffix yields Some(name) there.
        Some(stem) => KNOWN_LEGACY_BINS.contains(&stem) && !INSTALLED_BINS.contains(&stem),
        None => false,
    }
}

/// Remove orphan DontSpeak binaries from the install dir (this binary's own directory) so a
/// renamed/dropped executable can't shadow or be re-wired. Best-effort and SAFE: only regular
/// files (subdirs like a `winui/` dev-deploy skipped), only names matching
/// [`is_stale_ds_bin`], and on unix only files with the execute bit (never a stray data
/// file). No-op when the dir isn't writable; a permission error there is logged, not fatal.
fn prune_stale_bins() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue; // skip dirs (e.g. ~/.local/bin/winui/) and anything non-regular
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_stale_ds_bin(name) {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = entry
                .metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !executable {
                continue; // never delete a non-executable namesake (e.g. a lib/data file)
            }
        }
        match std::fs::remove_file(&path) {
            Ok(()) => eprintln!("wire: pruned stale binary {}", path.display()),
            Err(e) => eprintln!("wire: could not prune {} ({e}) — skipping", path.display()),
        }
    }
}

/// Client-agnostic install housekeeping, run once on any real (non-remove, non-preview) wire:
/// seed our `config.toml` with defaults if absent (a self-documenting file; the engine still
/// fails-open to defaults without it) AND prune orphan/legacy DontSpeak binaries from the install
/// dir so a renamed/dropped exe can't shadow or be re-wired (covers the legacy ds-mcp/-speak/
/// -narrate). Idempotent — safe to run per client wired. Pruning no-ops when the install dir
/// isn't writable (a permission error is logged, not fatal).
pub(crate) fn seed_and_prune(paths: &Paths) {
    if !paths.config_toml.exists() {
        if let Err(e) = ds_config::write_settings(paths, &ds_config::VoiceConfig::default()) {
            let msg = format!("wire: could not seed {}: {e}", paths.config_toml.display());
            eprintln!("{msg}");
            ds_log::log(&paths.log_file, ds_log::LogLevel::Error, "hook", &msg);
        } else {
            let msg = format!("wire: seeded {}", paths.config_toml.display());
            eprintln!("{msg}");
            ds_log::log(&paths.log_file, ds_log::LogLevel::Info, "hook", &msg);
        }
    }
    // NOTE: we deliberately do NOT seed `narration-spec.md`. The spec lives in the binary
    // (`DEFAULT_NARRATION_SPEC`), which the `provide` hook injects directly; a file on disk is
    // an OPTIONAL override only.
    prune_stale_bins();
}

/// Wire (or strip / print) the DontSpeak voice hooks into `cfg`, a JSON settings file using
/// Claude Code's hook contract (`WireMechanism::ClaudeJsonHooks` — today Claude Code's
/// `~/.claude/settings.json` and Qwen Code's `~/.qwen/settings.json`; the registry names the
/// file per client). `streaming` selects the hook SET: `true` (Claude Code) wires
/// `MessageDisplay` for per-batch narration; `false` (Qwen Code) omits it, so the reply is
/// voiced whole from `Stop`. `command_style` selects the command DIALECT: `ArgsArray`
/// (Claude Code — bin + `args`, timeout in seconds) or `InlineShell` (Qwen Code — verbs
/// inlined into the one command string its shell runner executes, timeout in ms). `client` is
/// the client whose file this is — stamped into every wired verb slice as `--client <token>`,
/// so the `dontspeak` binary the hook spawns knows who invoked it. Returns 0
/// on success — including a malformed or unmergeable existing file, which is left
/// byte-identical and reported, not treated as fatal (matching `claude_toml_hooks`) — or 1
/// on a hard error (bin-resolution failure, write failure).
#[allow(clippy::too_many_arguments)]
pub(crate) fn claude_json_hooks(
    cfg: &std::path::Path,
    streaming: bool,
    command_style: HookCommandStyle,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
) -> i32 {
    let Ok(existing) = io::read_json_or_bail("wire", cfg) else {
        return 0; // malformed existing file is the user's own — leave it, don't fail the run,
        // matching `claude_toml_hooks`'s convention below.
    };
    // Keep a copy for the steady-state short-circuit below (strip/merge consume `existing`).
    let before = existing.clone();

    let merged = if remove {
        Ok(ds_config::strip_hooks(existing))
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        let notif_channel = if cfg!(target_os = "macos") {
            Some("iterm2_with_bell")
        } else {
            None
        };
        let spec = HookSpec {
            bin: &bin,
            notif_channel,
            streaming,
            command_style,
            client,
        };
        ds_config::merge_hooks(existing, &spec)
    };

    // A malformed existing file (e.g. a non-array `hooks.<Event>` slot) is the user's own
    // file — leave it, don't fail the run, matching `claude_toml_hooks`'s convention below.
    let merged = match merged {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wire: {} left unchanged ({e})", cfg.display());
            return 0;
        }
    };

    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => {
                println!("// {}\n{s}", cfg.display());
                0
            }
            Err(e) => {
                eprintln!("wire: serialize failed: {e}");
                1
            }
        }
    } else {
        // Steady state (already wired / nothing to strip): write NOTHING and create NO `.bak`.
        // LOAD-BEARING — the engine runs this every boot. (Order-independent `Value` equality.)
        if merged == before {
            return 0;
        }
        io::backup_then_write(
            "wire",
            cfg,
            "json",
            &WriteBody::Json(&merged),
            hook_action(remove),
        )
    }
}

/// The report verb for a hook write — shared by both hook mechanisms.
fn hook_action(remove: bool) -> &'static str {
    if remove {
        "removed DontSpeak hooks from"
    } else {
        "wired DontSpeak hooks ->"
    }
}

/// Wire (or strip / print) DontSpeak's narration hooks into `cfg`, a TOML config using Claude
/// Code's hook contract (`WireMechanism::ClaudeTomlHooks` — today Codex's `~/.codex/config.toml`;
/// the registry names the file per client) — `SessionStart`→`notify --greet-only` (spoken
/// greeting, no streaming-witness seed), `UserPromptSubmit`→`notify` + `provide` (mark-active
/// / engine session re-discovery, and the narration spec) and `Stop`→`notify` (speak the
/// reply). Format-preserving (toml_edit). Returns 0 on
/// success, 1 on a hard error; a malformed config is reported and left UNCHANGED (it's the
/// user's file), which is non-fatal — same convention as `claude_json_hooks`.
pub(crate) fn claude_toml_hooks(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
) -> i32 {
    let existing = std::fs::read_to_string(cfg).unwrap_or_default();
    let result = if remove {
        ds_config::strip_codex_hooks(&existing)
    } else {
        let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("wire: could not resolve the dontspeak binary path");
            return 1;
        };
        ds_config::merge_codex_hooks(&existing, &bin, client)
    };
    match result {
        Ok(merged) if print_only => {
            println!("\n# {}\n{merged}", cfg.display());
            0
        }
        Ok(merged) if merged != existing => io::backup_then_write(
            "wire",
            cfg,
            "toml",
            &WriteBody::Str(&merged),
            hook_action(remove),
        ),
        Ok(_) => 0, // no change (already wired / nothing to strip)
        // A malformed config is the user's own file — leave it, don't fail the run.
        Err(e) => {
            eprintln!("wire: {} left unchanged ({e})", cfg.display());
            0
        }
    }
}

/// Wire (or strip / print) DontSpeak's native voice hooks into `cfg`, the DEDICATED JSON file
/// DontSpeak owns for a client (`WireMechanism::GrokJsonHooks` — today Grok's
/// `~/.grok/hooks/dontspeak.json`; the registry names the file per client). Unlike
/// [`claude_json_hooks`], the file is EXCLUSIVELY ours, so there is nothing to merge or
/// preserve: wire OVERWRITES the whole file with the rendered hook set (a timestamped backup is
/// taken first via the shared write tail), and `--remove` DELETES it (backing it up first so
/// the removal is recoverable). Returns 0 on success — including a `--remove` on a file that
/// was never created — or 1 on a hard error (bin-resolution failure, write/remove failure).
pub(crate) fn grok_json_hooks(
    cfg: &std::path::Path,
    client: ClientSource,
    remove: bool,
    print_only: bool,
    paths: &Paths,
) -> i32 {
    if remove {
        if !cfg.exists() {
            return 0; // nothing of ours on disk → clean no-op
        }
        // Back up before deleting so the removal is recoverable — mirrors the write path's
        // pre-write backup (`io::backup_then_write`). A backup failure is a non-fatal warning.
        if let Err(e) = ds_config::backup_before_write(cfg, "json") {
            eprintln!(
                "wire: WARNING: could not back up {} before removing ({e}); proceeding without a backup",
                cfg.display()
            );
        }
        return match std::fs::remove_file(cfg) {
            Ok(()) => {
                eprintln!("wire: {} {}", hook_action(true), cfg.display());
                0
            }
            Err(e) => {
                eprintln!("wire: could not remove {} ({e})", cfg.display());
                1
            }
        };
    }

    let Some(bin) = io::resolve_dontspeak_bin_at(Some(paths)) else {
        eprintln!("wire: could not resolve the dontspeak binary path");
        return 1;
    };
    let v = ds_config::grok_hooks_value(&bin, client);

    if print_only {
        println!(
            "// {}\n{}",
            cfg.display(),
            serde_json::to_string_pretty(&v).unwrap_or_default()
        );
        return 0;
    }

    // Steady state (already wired, shape-identical): write NOTHING and create NO `.bak`.
    // LOAD-BEARING — the engine runs this every boot. A missing/malformed file compares
    // unequal (order-independent `Value` equality), so a real first wire still writes.
    if std::fs::read_to_string(cfg)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .as_ref()
        == Some(&v)
    {
        return 0;
    }

    io::backup_then_write(
        "wire",
        cfg,
        "json",
        &WriteBody::Json(&v),
        hook_action(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prunes_only_known_legacy_binaries_keeps_current_foreign_and_own_non_bins() {
        let ext = std::env::consts::EXE_SUFFIX; // ".exe" on Windows, "" on unix
        let f = |b: &str| format!("{b}{ext}");

        // Known legacy names the single-binary consolidation replaced → prune.
        assert!(is_stale_ds_bin(&f("ds-mcp")));
        assert!(is_stale_ds_bin(&f("ds-speak")));
        assert!(is_stale_ds_bin(&f("ds-narrate")));
        // The dropped `dontspeakd` binary → prune any leftover install.
        assert!(is_stale_ds_bin(&f("dontspeakd")));

        // Current canonical binaries → keep (incl. the running dontspeak itself).
        assert!(!is_stale_ds_bin(&f("dontspeak")));
        assert!(!is_stale_ds_bin(&f("ds-helper")));
        assert!(!is_stale_ds_bin(&f("ds-winui")));
        assert!(!is_stale_ds_bin(&f("ds-gtk")));

        // Regression test: this app's OWN `dontspeak-uninstall` script (placed executable in
        // the install dir by the installer) is NOT a known legacy binary name → keep. A
        // `dontspeak*`/`ds-*` prefix check used to flag it as stale and delete it on every wire.
        assert!(!is_stale_ds_bin(&f("dontspeak-uninstall")));
        // An unrelated user tool that merely shares the prefix is never mistaken for ours.
        assert!(!is_stale_ds_bin(&f("ds-sync")));
        // Any OTHER same-prefixed name not on the explicit legacy list → keep (no more
        // catch-all prefix match; a future rename/drop must be added to `KNOWN_LEGACY_BINS`).
        assert!(!is_stale_ds_bin(&f("ds-oldname")));

        // Foreign tools sharing the dir → keep.
        assert!(!is_stale_ds_bin(&f("ripgrep")));
        assert!(!is_stale_ds_bin(&f("node")));

        // On Windows a non-.exe namesake (the cdylib) has the wrong suffix → keep.
        #[cfg(windows)]
        assert!(!is_stale_ds_bin("ds_core.dll"));
    }

    /// Parse a written `settings.json` (or `""` for a not-yet-created file) into a `Value`.
    fn read_json(cfg: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap()
    }

    // Every test below now builds its own `Paths::rooted_at(dir.path())` (see each test) and
    // threads it through `claude_json_hooks`/`claude_toml_hooks` into
    // `io::resolve_dontspeak_bin_at`, so the unix stable-install-path check is scoped to the
    // tempdir — none of these tests touch the real `$HOME`/`BaseDirs` any more.

    #[test]
    fn claude_json_hooks_wires_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            0
        );
        let v = read_json(&cfg);
        assert_eq!(v["hooks"]["MessageDisplay"].as_array().unwrap().len(), 1);

        // Re-run: REPLACE-OURS merge must not duplicate the group.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            0
        );
        let v2 = read_json(&cfg);
        assert_eq!(v2["hooks"]["MessageDisplay"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn claude_json_hooks_remove_strips_previously_wired_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            0
        );
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                true,
                false,
                &paths
            ),
            0
        );

        let v = read_json(&cfg);
        // `strip_hooks` drops an event array once it's emptied of our groups, and drops the
        // whole `hooks` object once every event is gone — undoing the get-or-create scaffold.
        assert!(v.get("hooks").is_none());
    }

    #[test]
    fn claude_json_hooks_malformed_json_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        let raw = b"{ not json";
        std::fs::write(&cfg, raw).unwrap();

        // Caught in `io::read_json_or_bail`, but non-fatal — reported and the file left
        // byte-identical — never reaches `resolve_dontspeak_bin`.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            0
        );
        assert_eq!(std::fs::read(&cfg).unwrap(), raw);
    }

    #[test]
    fn claude_json_hooks_unmergeable_event_shape_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        // `hooks.MessageDisplay` as an object (not an array) is a hand-edited/foreign shape
        // `merge_hooks` refuses to clobber → `HooksMergeError::UnmergeableShape`, the first
        // canonical event merge tried, so this never reaches the write call.
        let raw = r#"{"hooks":{"MessageDisplay":{"not":"an array"}}}"#;
        std::fs::write(&cfg, raw).unwrap();

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            0
        ); // non-fatal
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw); // byte-identical
    }

    #[test]
    fn claude_json_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                true,
                &paths
            ),
            0
        );
        assert!(!cfg.exists());
    }

    /// `backup_then_write`'s `Err(e) => 1` arm: force `create_dir_all` on the config's parent
    /// dir to fail by pre-creating a plain FILE at the path that would otherwise be that parent
    /// directory — `create_dir_all` errors because `blocked` already exists and isn't a dir.
    #[test]
    fn claude_json_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("settings.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_json_hooks(
                &cfg,
                true,
                HookCommandStyle::ArgsArray,
                ClientSource::ClaudeCode,
                false,
                false,
                &paths
            ),
            1
        );
    }

    /// Non-streaming inline-shell wire (Qwen Code): `MessageDisplay` is omitted AND the
    /// on-disk entries are the INLINED dialect — no `args` key (Qwen's runner silently drops
    /// it), the verb inside the one `command` string — with the same idempotent + additive +
    /// clean-strip guarantees.
    #[test]
    fn claude_json_hooks_non_streaming_omits_messagedisplay() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.json");
        let paths = Paths::rooted_at(dir.path());
        let qwen = HookCommandStyle::InlineShell;

        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                false,
                false,
                &paths
            ),
            0
        );
        let v = read_json(&cfg);
        assert!(
            v["hooks"].get("MessageDisplay").is_none(),
            "non-streaming wire must NOT install MessageDisplay"
        );
        // Stop / UserPromptSubmit / SessionStart / Notification ARE wired — in the inlined
        // shape: no `args` key, verb in the command string.
        for evt in ["Stop", "UserPromptSubmit", "SessionStart", "Notification"] {
            let h = &v["hooks"][evt][0]["hooks"][0];
            assert!(
                h.get("args").is_none(),
                "{evt}: on-disk Qwen shape must be inlined (no `args`), got {h}"
            );
            assert!(
                h["command"].as_str().unwrap().contains(" notify"),
                "{evt}: verb inlined into the command string"
            );
        }
        // Idempotent.
        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                false,
                false,
                &paths
            ),
            0
        );
        // Strips cleanly (remove removes every DontSpeak group regardless of dialect).
        assert_eq!(
            claude_json_hooks(
                &cfg,
                false,
                qwen,
                ClientSource::QwenCode,
                true,
                false,
                &paths
            ),
            0
        );
        assert!(read_json(&cfg).get("hooks").is_none());
    }

    #[test]
    fn claude_toml_hooks_wires_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths),
            0
        );
        let first = std::fs::read_to_string(&cfg).unwrap();

        // SessionStart is wired as a greet-only group: the greeting speaks, but the
        // streaming-witness seed is skipped (Codex is non-streaming — a seed would silence
        // its Stop narration). The command now ENDS with the uniform `--client codex` tail, so
        // the trailing quote pins THAT as the end of the command; toml_edit renders the command
        // as a basic (`"`) or literal (`'`) string depending on what the resolved binary path
        // contains (backslashes on Windows ⇒ literal).
        assert!(first.contains("[[hooks.SessionStart]]"), "got {first}");
        assert!(
            first.contains(" notify --greet-only --client codex\"")
                || first.contains(" notify --greet-only --client codex'"),
            "got {first}"
        );

        // Re-run: an unchanged command is a true byte-for-byte no-op (see `codex_group_matches`).
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths),
            0
        );
        let second = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn claude_toml_hooks_remove_strips_previously_wired_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths),
            0
        );
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, true, false, &paths),
            0
        );

        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("dontspeak"));
    }

    #[test]
    fn claude_toml_hooks_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        // `existing` is `unwrap_or_default()` on the missing-file read → "", and
        // `strip_codex_hooks("")` short-circuits `Ok("")` before ever calling
        // `resolve_dontspeak_bin` (that call is skipped entirely on the `remove` path anyway).
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, true, false, &paths),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn claude_toml_hooks_malformed_toml_is_left_unchanged_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());
        let raw = "hooks = [not valid toml";
        std::fs::write(&cfg, raw).unwrap();

        // `CodexMergeError::Parse` → the final `Err(e)` arm: reported, non-fatal, unchanged —
        // same convention `claude_json_hooks` now matches.
        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths),
            0
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), raw);
    }

    #[test]
    fn claude_toml_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, true, &paths),
            0
        );
        assert!(!cfg.exists());
    }

    /// Same write-failure technique as `claude_json_hooks_write_failure_returns_1`, for the
    /// TOML writer's `backup_then_write` call.
    #[test]
    fn claude_toml_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("config.toml");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            claude_toml_hooks(&cfg, ClientSource::Codex, false, false, &paths),
            1
        );
    }

    #[test]
    fn seed_and_prune_seeds_config_toml_once_then_leaves_it_alone() {
        // `seed_and_prune` also unconditionally runs `prune_stale_bins`, which walks
        // `current_exe()`'s own directory (this test binary's build-output dir) and removes
        // any entry named exactly one of `KNOWN_LEGACY_BINS`. Safe today because no workspace
        // `[[bin]]` target is named `ds-mcp`/`ds-speak`/`ds-narrate`/`dontspeakd` — the
        // workspace DOES contain `ds-narrate` and `dontspeakd` CRATES again, but both are
        // lib-only (no executable ever lands in the install dir, so the prune can never match
        // them). A future `[[bin]]` reusing one of those names would need this comment (and
        // test) revisited.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());

        seed_and_prune(&paths);
        assert!(paths.config_toml.exists());

        // Append a marker to the seeded file, then prove a second call does NOT overwrite it
        // — the `!exists()` gate only seeds once.
        let mut contents = std::fs::read_to_string(&paths.config_toml).unwrap();
        contents.push_str("\n# marker\n");
        std::fs::write(&paths.config_toml, &contents).unwrap();

        seed_and_prune(&paths);
        let after = std::fs::read_to_string(&paths.config_toml).unwrap();
        assert_eq!(after, contents);
    }

    /// `write_settings`'s `Err` branch inside `seed_and_prune`: pre-create a plain FILE at the
    /// path that would be `paths.config_toml`'s parent directory, so the underlying
    /// `create_dir_all` fails. `seed_and_prune` returns `()`, not a `Result` — reading the
    /// function, its `Err(e)` arm only logs (via `ds_log::log`) and continues (does not
    /// propagate, does not panic), so the only observable effect is that `config_toml` is never
    /// created.
    #[test]
    fn seed_and_prune_write_failure_is_logged_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::write(dir.path().join(".dontspeak"), b"blocking file").unwrap();

        seed_and_prune(&paths); // must not panic
        assert!(!paths.config_toml.exists());
    }

    // ── GrokJsonHooks: the DEDICATED own-the-file JSON writer ────────────────────────
    // Grok's `~/.grok/hooks/dontspeak.json` is exclusively ours: wire OVERWRITES it, unwire
    // DELETES it. These mirror the `claude_json_hooks` tests, adapted to the own-the-file
    // semantics (no merge, no user keys to preserve).

    #[test]
    fn grok_json_hooks_wires_the_five_events() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, false, &paths),
            0
        );
        let v = read_json(&cfg);
        let hooks = v["hooks"].as_object().unwrap();
        let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "Notification",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "UserPromptSubmit",
            ]
        );
        // Stop voices the reply — a command carrying our binary, a seconds timeout, no async.
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert!(
            stop["command"].as_str().unwrap().contains("dontspeak"),
            "Stop command invokes the dontspeak binary, got {stop}"
        );
        assert!(
            stop.get("timeout")
                .and_then(serde_json::Value::as_i64)
                .is_some(),
            "Stop entry carries a numeric (seconds) timeout"
        );
        assert!(
            stop.get("async").is_none(),
            "Grok hooks run synchronously — no async key"
        );
    }

    #[test]
    fn grok_json_hooks_is_idempotent_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, false, &paths),
            0
        );
        let first = std::fs::read(&cfg).unwrap();
        // Own-the-file overwrite: a second wire re-renders the SAME content (same resolved bin),
        // so the file is byte-for-byte identical.
        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, false, &paths),
            0
        );
        let second = std::fs::read(&cfg).unwrap();
        assert_eq!(first, second, "re-wire writes byte-identical contents");
    }

    #[test]
    fn grok_json_hooks_remove_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, false, &paths),
            0
        );
        assert!(cfg.exists());
        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, true, false, &paths),
            0
        );
        assert!(!cfg.exists(), "unwire deletes the dedicated file");
    }

    #[test]
    fn grok_json_hooks_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, true, false, &paths),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn grok_json_hooks_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, true, &paths),
            0
        );
        assert!(!cfg.exists());
    }

    /// Same blocked-parent technique as `claude_json_hooks_write_failure_returns_1`: a plain
    /// FILE at the config's would-be parent dir makes `create_dir_all` fail → exit 1.
    #[test]
    fn grok_json_hooks_write_failure_returns_1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocked"), b"not a directory").unwrap();
        let cfg = dir.path().join("blocked").join("dontspeak.json");
        let paths = Paths::rooted_at(dir.path());

        assert_eq!(
            grok_json_hooks(&cfg, ClientSource::Grok, false, false, &paths),
            1
        );
    }
}
