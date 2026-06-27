//! `dontspeak wire-hooks [--remove] [--print-only] [--no-codex | --codex-only] [--prune-stale]`
//! — the single cross-platform installer step that wires (or removes) the DontSpeak voice
//! hooks in `~/.claude/settings.json` AND, when OpenAI Codex is present, the narration hooks
//! in `~/.codex/config.toml` (Codex's hooks use the same contract, so the same binary serves
//! both). The hook SETS + merges are the ONE definition in `ds-config` (shared by
//! macOS/Windows/Linux installers); this owns CLI parsing, binary-path resolution, backup,
//! and the atomic write.
//!
//! Codex gating: by default wire Claude + auto-detect Codex (wire it iff `~/.codex` exists).
//! `--no-codex` wires Claude only; `--codex-only` wires Codex only, leaving settings.json
//! untouched — the per-client split the Windows installer's component checkboxes need.
//!
//! Safe by construction: additive + idempotent merge (never duplicates ours, never
//! clobbers the user's own hooks/keys), a timestamped backup before writing, and a
//! malformed existing file is treated as empty rather than destroyed. `--print-only`
//! emits the merged document to stdout without touching disk (the hands-off path).

use serde_json::Value;
use ds_config::{HookSpec, INSTALLED_BINS, Paths};

/// Resolve an installed sibling binary with the platform executable suffix.
///
/// On UNIX, prefer `~/.local/bin/<name>` (the install layout): this lets a dev build run from
/// `target/` wire the hooks at the DEPLOYED binary, not the build dir. On WINDOWS we do NOT
/// consult `~/.local/bin` — Inno is authoritative and lays the binary in `{app}` beside THIS
/// exe, so a stale dev-deploy copy in `~/.local/bin` must not SHADOW the freshly-installed one.
/// Returns the sibling path even if not present yet (the installer lays the binaries down
/// together, so the path is correct regardless).
fn sibling_bin(name: &str) -> Option<String> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    #[cfg(unix)]
    if let Some(p) = Paths::resolve() {
        let cand = p.home.join(".local/bin").join(&file);
        if cand.exists() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(&file).to_string_lossy().into_owned())
}

/// PURE decision: is `name` a STALE DontSpeak binary — in our namespace (`dontspeak*` OR `ds-*`
/// with this platform's exe suffix) but NOT in the canonical [`INSTALLED_BINS`] set? That covers
/// the legacy `ds-mcp/-speak/-narrate` the consolidation replaced and any future renamed/dropped
/// bin. The current bins (`dontspeak`, `dontspeakd`, `ds-helper`, `ds-winui`) are in the set so
/// they're never flagged; foreign tools and non-exe siblings (e.g. `ds_core.dll` on Windows —
/// wrong suffix) are kept. (Our exes use BOTH prefixes — the main app/daemon are `dontspeak*`,
/// the helper/host are `ds-*` — so a single-prefix check would miss the `ds-*` legacy names.)
fn is_stale_ds_bin(name: &str) -> bool {
    match name.strip_suffix(std::env::consts::EXE_SUFFIX) {
        // EXE_SUFFIX is "" on unix, so strip_suffix yields Some(name) there.
        Some(stem) => {
            (stem.starts_with("dontspeak") || stem.starts_with("ds-"))
                && !INSTALLED_BINS.contains(&stem)
        }
        None => false,
    }
}

/// Remove orphan DontSpeak binaries from the install dir (this binary's own directory) so a
/// renamed/dropped executable can't shadow or be re-wired. Best-effort and SAFE: only regular
/// files (subdirs like a `winui/` dev-deploy skipped), only names matching
/// [`is_stale_ds_bin`], and on unix only files with the execute bit (never a stray data
/// file). No-op when the dir isn't writable — the Windows `{app}` case, where the elevated Inno
/// `[InstallDelete]` owns cleanup; a permission error there is logged, not fatal.
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
            Ok(()) => eprintln!("wire-hooks: pruned stale binary {}", path.display()),
            Err(e) => eprintln!(
                "wire-hooks: could not prune {} ({e}) — skipping",
                path.display()
            ),
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut remove = false;
    let mut print_only = false;
    // Codex (~/.codex/config.toml) wiring: by default we wire Claude Code AND auto-detect
    // Codex (wire it when ~/.codex exists). `--no-codex` skips Codex; `--codex-only` wires
    // ONLY Codex (force-create) without touching settings.json — the per-client gating the
    // Windows installer's component checkboxes need.
    let mut no_codex = false;
    let mut codex_only = false;
    // `--prune-stale`: ONLY prune orphan binaries from the install dir and return, without
    // touching settings.json — the standalone form of the cleanup that a normal wire also
    // runs. (A non-remove wire prunes automatically below.)
    let mut prune_only = false;
    for a in args {
        match a.as_str() {
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "--no-codex" => no_codex = true,
            "--codex-only" => codex_only = true,
            "--prune-stale" => prune_only = true,
            "-h" | "--help" => {
                eprintln!("usage: dontspeak wire-hooks [--remove] [--print-only] [--no-codex | --codex-only] [--prune-stale]");
                return 0;
            }
            other => eprintln!("wire-hooks: ignoring unknown arg {other:?}"),
        }
    }
    // `--codex-only` and `--no-codex` are contradictory; `--codex-only` wins (wire Codex).
    if codex_only {
        no_codex = false;
    }
    // Standalone prune: do just that and exit (no $HOME needed beyond current_exe's dir).
    if prune_only {
        prune_stale_bins();
        return 0;
    }

    let Some(paths) = Paths::resolve() else {
        eprintln!("wire-hooks: $HOME not set; nothing to do");
        return 1;
    };

    // Seed our config.toml with defaults if absent, so a fresh install has a
    // self-documenting file (the engine still fails-open to defaults without it).
    // Client-agnostic, so do it on any non-remove wire.
    if !remove && !paths.config_toml.exists() {
        if let Err(e) =
            ds_config::write_settings(&paths, &ds_config::VoiceConfig::default())
        {
            eprintln!(
                "wire-hooks: could not seed {}: {e}",
                paths.config_toml.display()
            );
        } else {
            eprintln!("wire-hooks: seeded {}", paths.config_toml.display());
        }
    }
    // NOTE: we deliberately do NOT seed `narration-spec.md`. The spec lives in the binary
    // (`DEFAULT_NARRATION_SPEC`), which the `provide` hook injects directly; a file on disk is
    // an OPTIONAL override only. So a fresh install writes nothing and just uses the built-in.

    // Prune orphan DontSpeak binaries on every (non-remove) wire. wire-hooks is the ONE
    // cross-platform installer step every path runs (scripts/install.sh, Inno [Run],
    // install.ps1/enable.ps1), so it's the single seam that gives all platforms the cleanup,
    // including the legacy ds-mcp/-speak/-narrate the consolidation replaced. No-op when
    // the dir isn't writable (Windows {app}, where Inno [InstallDelete] owns it).
    if !remove {
        prune_stale_bins();
    }

    // ── Claude Code (~/.claude/settings.json) — skipped under --codex-only ──────────
    if !codex_only {
        let rc = wire_claude_code(&paths, remove, print_only);
        if rc != 0 {
            return rc;
        }
    }

    // ── OpenAI Codex (~/.codex/config.toml) ─────────────────────────────────────────
    // Codex's hooks use the SAME stdin-JSON + `hook_event_name` contract as Claude Code, so
    // the SAME binary handles them; only the file is TOML and the event set differs (the
    // reply is voiced from `Stop`, the narration spec injected at `UserPromptSubmit`→provide).
    // Wire when Codex is present (or forced with --codex-only); skip under --no-codex; on
    // --remove strip an existing config.toml only.
    let do_codex = if no_codex {
        false
    } else if remove {
        paths.codex_config.exists()
    } else {
        codex_only || paths.codex_dir.exists()
    };
    if do_codex {
        let rc = wire_codex_config(&paths, remove, print_only);
        if rc != 0 {
            return rc;
        }
    }

    0
}

/// Wire (or strip / print) the Claude Code voice hooks in `~/.claude/settings.json`.
/// Returns 0 on success, 1 on a hard error.
fn wire_claude_code(paths: &Paths, remove: bool, print_only: bool) -> i32 {
    // Parse the existing settings.json. Missing or empty → treat as `{}`. A present
    // but MALFORMED file is left UNTOUCHED (bail) rather than overwritten — it's also
    // Claude Code's own config, and replacing it would lose a recoverable file.
    let existing = match std::fs::read_to_string(&paths.settings_json) {
        Err(_) => Value::Null,
        Ok(s) if s.trim().is_empty() => Value::Null,
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "wire-hooks: existing settings.json is not valid JSON; leaving it unchanged"
                );
                return 1;
            }
        },
    };

    let merged = if remove {
        ds_config::strip_hooks(existing)
    } else {
        let Some(bin) = sibling_bin("dontspeak") else {
            eprintln!("wire-hooks: could not resolve the dontspeak binary path");
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
        };
        ds_config::merge_hooks(existing, &spec)
    };

    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => println!("// ~/.claude/settings.json\n{s}"),
            Err(e) => {
                eprintln!("wire-hooks: serialize failed: {e}");
                return 1;
            }
        }
    } else {
        // Best-effort timestamped backup before writing (surface, don't swallow,
        // a copy failure — we still proceed, but the user is warned the overwrite
        // has no recoverable copy).
        if let Err(e) = ds_config::backup_before_write(&paths.settings_json, "json") {
            eprintln!(
                "wire-hooks: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
                paths.settings_json.display()
            );
        }
        match ds_config::atomic_write_json(&paths.settings_json, &merged) {
            Ok(()) => {
                eprintln!(
                    "wire-hooks: {} {}",
                    if remove {
                        "removed DontSpeak hooks from"
                    } else {
                        "wired DontSpeak hooks ->"
                    },
                    paths.settings_json.display()
                );
            }
            Err(e) => {
                eprintln!("wire-hooks: write failed: {e}");
                return 1;
            }
        }
    }

    0
}

/// Wire (or strip / print) DontSpeak's narration hooks in OpenAI Codex's `~/.codex/config.toml`
/// — `UserPromptSubmit`→`provide` (inject the narration spec) and `Stop`→`notify` (speak the
/// reply). Format-preserving (toml_edit). Returns 0 on success, 1 on a hard error; a malformed
/// config is reported and left UNCHANGED (it's the user's file), which is non-fatal.
fn wire_codex_config(paths: &Paths, remove: bool, print_only: bool) -> i32 {
    let existing = std::fs::read_to_string(&paths.codex_config).unwrap_or_default();
    let result = if remove {
        ds_config::strip_codex_hooks(&existing)
    } else {
        let Some(bin) = sibling_bin("dontspeak") else {
            eprintln!("wire-hooks: could not resolve the dontspeak path for Codex");
            return 1;
        };
        ds_config::merge_codex_hooks(&existing, &bin)
    };
    match result {
        Ok(merged) if print_only => {
            println!("\n# ~/.codex/config.toml\n{merged}");
            0
        }
        Ok(merged) if merged != existing => {
            if let Err(e) = ds_config::backup_before_write(&paths.codex_config, "toml") {
                eprintln!(
                    "wire-hooks: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
                    paths.codex_config.display()
                );
            }
            match ds_config::atomic_write_str(&paths.codex_config, &merged) {
                Ok(()) => {
                    eprintln!(
                        "wire-hooks: {} {}",
                        if remove {
                            "removed DontSpeak hooks from"
                        } else {
                            "wired DontSpeak hooks ->"
                        },
                        paths.codex_config.display()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("wire-hooks: codex write failed: {e}");
                    1
                }
            }
        }
        Ok(_) => 0, // no change (already wired / nothing to strip)
        // A malformed config.toml is the user's own file — leave it, don't fail the run.
        Err(e) => {
            eprintln!(
                "wire-hooks: {} left unchanged ({e})",
                paths.codex_config.display()
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_stale_ds_bin;

    #[test]
    fn prunes_legacy_and_orphan_binaries_keeps_current_and_foreign() {
        let ext = std::env::consts::EXE_SUFFIX; // ".exe" on Windows, "" on unix
        let f = |b: &str| format!("{b}{ext}");

        // Legacy names the single-binary consolidation replaced → prune.
        assert!(is_stale_ds_bin(&f("ds-mcp")));
        assert!(is_stale_ds_bin(&f("ds-speak")));
        assert!(is_stale_ds_bin(&f("ds-narrate")));
        // Any future renamed/dropped bin in our namespace → prune.
        assert!(is_stale_ds_bin(&f("ds-oldname")));

        // Current canonical binaries → keep (incl. the running dontspeak itself).
        assert!(!is_stale_ds_bin(&f("dontspeak")));
        assert!(!is_stale_ds_bin(&f("dontspeakd")));
        assert!(!is_stale_ds_bin(&f("ds-helper")));
        assert!(!is_stale_ds_bin(&f("ds-winui")));

        // Foreign tools sharing the dir → keep.
        assert!(!is_stale_ds_bin(&f("ripgrep")));
        assert!(!is_stale_ds_bin(&f("node")));

        // On Windows a non-.exe namesake (the cdylib) has the wrong suffix → keep.
        #[cfg(windows)]
        assert!(!is_stale_ds_bin("ds_core.dll"));
    }
}
