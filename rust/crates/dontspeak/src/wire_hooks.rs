//! Hook-wiring building blocks for the [`wire`](crate::wire) orchestrator: the Claude Code voice
//! hooks in `~/.claude/settings.json` ([`claude_code_hooks`]) and the OpenAI Codex narration hooks
//! in `~/.codex/config.toml` ([`codex_hooks`]), plus the client-agnostic install housekeeping
//! ([`seed_and_prune`]: seed our `config.toml`, prune stale binaries). Codex's hooks use the same
//! stdin-JSON contract as Claude Code, so the SAME binary serves both; only the file (TOML) and the
//! event set differ. The hook SETS + merges are the ONE definition in `ds-config` (shared by every
//! platform installer); this owns binary-path resolution, backup, and the atomic write — the
//! `wire <client>` orchestrator composes these per client.
//!
//! Safe by construction: additive + idempotent merge (never duplicates ours, never clobbers the
//! user's own hooks/keys), a timestamped backup before writing, and a malformed existing file is
//! treated as empty rather than destroyed. `print_only` emits the merged document without touching
//! disk.

use ds_config::{HookSpec, INSTALLED_BINS, Paths};
use serde_json::Value;

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
            Ok(()) => eprintln!("wire: pruned stale binary {}", path.display()),
            Err(e) => eprintln!(
                "wire: could not prune {} ({e}) — skipping",
                path.display()
            ),
        }
    }
}

/// Client-agnostic install housekeeping, run once on any real (non-remove, non-preview) wire:
/// seed our `config.toml` with defaults if absent (a self-documenting file; the engine still
/// fails-open to defaults without it) AND prune orphan/legacy DontSpeak binaries from the install
/// dir so a renamed/dropped exe can't shadow or be re-wired (covers the legacy ds-mcp/-speak/
/// -narrate). Idempotent — safe to run per client the installer wires. Pruning no-ops when the dir
/// isn't writable (Windows `{app}`, where Inno `[InstallDelete]` owns it).
pub(crate) fn seed_and_prune(paths: &Paths) {
    if !paths.config_toml.exists() {
        if let Err(e) = ds_config::write_settings(paths, &ds_config::VoiceConfig::default()) {
            eprintln!("wire: could not seed {}: {e}", paths.config_toml.display());
        } else {
            eprintln!("wire: seeded {}", paths.config_toml.display());
        }
    }
    // NOTE: we deliberately do NOT seed `narration-spec.md`. The spec lives in the binary
    // (`DEFAULT_NARRATION_SPEC`), which the `provide` hook injects directly; a file on disk is
    // an OPTIONAL override only.
    prune_stale_bins();
}

/// Wire (or strip / print) the Claude Code voice hooks in `~/.claude/settings.json`.
/// Returns 0 on success, 1 on a hard error. One of the two per-surface writers the `wire`
/// orchestrator composes for `claude_code` (the other being the shared MCP registration).
pub(crate) fn claude_code_hooks(paths: &Paths, remove: bool, print_only: bool) -> i32 {
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
                    "wire: existing settings.json is not valid JSON; leaving it unchanged"
                );
                return 1;
            }
        },
    };

    let merged = if remove {
        ds_config::strip_hooks(existing)
    } else {
        let Some(bin) = sibling_bin("dontspeak") else {
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
        };
        ds_config::merge_hooks(existing, &spec)
    };

    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => println!("// ~/.claude/settings.json\n{s}"),
            Err(e) => {
                eprintln!("wire: serialize failed: {e}");
                return 1;
            }
        }
    } else {
        // Best-effort timestamped backup before writing (surface, don't swallow,
        // a copy failure — we still proceed, but the user is warned the overwrite
        // has no recoverable copy).
        if let Err(e) = ds_config::backup_before_write(&paths.settings_json, "json") {
            eprintln!(
                "wire: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
                paths.settings_json.display()
            );
        }
        match ds_config::atomic_write_json(&paths.settings_json, &merged) {
            Ok(()) => {
                eprintln!(
                    "wire: {} {}",
                    if remove {
                        "removed DontSpeak hooks from"
                    } else {
                        "wired DontSpeak hooks ->"
                    },
                    paths.settings_json.display()
                );
            }
            Err(e) => {
                eprintln!("wire: write failed: {e}");
                return 1;
            }
        }
    }

    0
}

/// Wire (or strip / print) DontSpeak's narration hooks in OpenAI Codex's `~/.codex/config.toml`
/// — `UserPromptSubmit`→`provide` (inject the narration spec) and `Stop`→`notify` (speak the
/// reply). Format-preserving (toml_edit). Returns 0 on success, 1 on a hard error; a malformed
/// config is reported and left UNCHANGED (it's the user's file), which is non-fatal. The
/// per-surface writer the `wire` orchestrator uses for `codex`.
pub(crate) fn codex_hooks(paths: &Paths, remove: bool, print_only: bool) -> i32 {
    let existing = std::fs::read_to_string(&paths.codex_config).unwrap_or_default();
    let result = if remove {
        ds_config::strip_codex_hooks(&existing)
    } else {
        let Some(bin) = sibling_bin("dontspeak") else {
            eprintln!("wire: could not resolve the dontspeak path for Codex");
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
                    "wire: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
                    paths.codex_config.display()
                );
            }
            match ds_config::atomic_write_str(&paths.codex_config, &merged) {
                Ok(()) => {
                    eprintln!(
                        "wire: {} {}",
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
                    eprintln!("wire: codex write failed: {e}");
                    1
                }
            }
        }
        Ok(_) => 0, // no change (already wired / nothing to strip)
        // A malformed config.toml is the user's own file — leave it, don't fail the run.
        Err(e) => {
            eprintln!(
                "wire: {} left unchanged ({e})",
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
