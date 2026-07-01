//! Shared MCP-registration core for the [`wire`](crate::wire) orchestrator — used for BOTH Claude
//! CODE (`~/.claude.json`, [`code_target`]) and Claude DESKTOP (`claude_desktop_config.json`,
//! [`desktop_target`]). Both register the IDENTICAL stdio `mcpServers.DontSpeak` entry and differ
//! only in WHICH config file, how they detect the client, and the user-facing labels — so the one
//! read → merge/strip → backup → atomic-write flow lives here ONCE rather than being copied per
//! client. It reuses the SAME `ds-config` primitives the hook writers use
//! (`merge_mcp_server`/`strip_mcp_server`, `backup_before_write`, `atomic_write_json`), so an MCP
//! registration is crash-safe in exactly the way a settings.json hook write is. Each client builds
//! a [`Target`] (via [`code_target`]/[`desktop_target`]) and calls [`apply`].

use std::path::Path;

use ds_config::Paths;
use serde_json::Value;

/// One client's registration target — the config file plus the client-specific gating and
/// labels that specialize the shared flow. Built by each subcommand from its [`Paths`].
pub struct Target<'a> {
    /// Label for log lines (the `wire` orchestrator sets `"wire"`).
    pub tool: &'a str,
    /// The config file to edit (Code's `~/.claude.json`, Desktop's `claude_desktop_config.json`).
    pub config: &'a Path,
    /// Whether the client is installed. Gates a REAL write so we never scatter a stray config
    /// on a machine without the client; consulted only for a non-remove, non-print wire.
    pub present: bool,
    /// Message shown when `present` is false and we skip — e.g.
    /// `"Claude Code not detected (/home/.claude)"`.
    pub absent_hint: String,
    /// One-line hint printed after a successful wire (how to load the newly registered server).
    pub load_hint: &'a str,
}

/// Resolve the absolute path to register as the MCP `command`: prefer the stable install
/// location (`~/.local/bin/dontspeak`) so the entry survives rebuilds at the same path, else
/// THIS executable (the macOS app bundle or a dev build). `None` only if neither resolves.
/// (Same intent as the hook writer's `sibling_bin`, without the Windows-shadowing guard the hook
/// path needs — the MCP `command` is fine pointing at either deployed path.)
pub fn resolve_self_command() -> Option<String> {
    let file = format!("dontspeak{}", std::env::consts::EXE_SUFFIX);
    if let Some(p) = Paths::resolve() {
        let cand = p.home.join(".local/bin").join(&file);
        if cand.exists() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    std::env::current_exe()
        .ok()
        .map(|e| e.to_string_lossy().into_owned())
}

/// The Claude **Code** MCP target — the server entry in `~/.claude.json`, gated on the `~/.claude`
/// dir (which the Claude Code hook wire creates just before us). Built here so the `wire`
/// orchestrator composes `claude_code` = hooks + this MCP registration from ONE shared flow.
pub fn code_target(paths: &Paths) -> Target<'_> {
    Target {
        tool: "wire",
        config: &paths.claude_code_config,
        present: paths.claude_dir.exists(),
        absent_hint: format!("Claude Code not detected ({})", paths.claude_dir.display()),
        load_hint: "start a new Claude Code session to load the server",
    }
}

/// The Claude **Desktop** MCP target — `claude_desktop_config.json`, gated on
/// [`Paths::claude_desktop_present`] so we never scatter a stray `Claude/` config dir on a machine
/// without Desktop. Desktop has no hook system, so `claude_desktop` = this MCP registration only.
pub fn desktop_target(paths: &Paths) -> Target<'_> {
    Target {
        tool: "wire",
        config: &paths.claude_desktop_config,
        present: paths.claude_desktop_present(),
        absent_hint: format!(
            "Claude Desktop not detected ({})",
            paths.claude_desktop_dir.display()
        ),
        load_hint: "quit and reopen Claude Desktop to load the server",
    }
}

/// Register (or, with `remove`, un-register) our stdio `mcpServers.DontSpeak` entry in
/// `target.config`, or PREVIEW the result with `print_only`. The ONE flow shared by the `wire`
/// orchestrator's Claude Code + Desktop MCP surfaces ([`code_target`]/[`desktop_target`]):
///   presence-gate → parse (a malformed file is left UNTOUCHED) → merge/strip via `ds-config`
///   → either print, or back-up-then-atomic-write.
/// Additive + idempotent (our entry is overwritten so a reinstall re-points `command`; every
/// other server and top-level key is preserved). Returns a process exit code (0 ok, 1 hard error).
pub fn apply(target: &Target, remove: bool, print_only: bool) -> i32 {
    let tool = target.tool;
    let cfg = target.config;

    // A real wire (not removal/preview) requires the client present, so we never scatter a
    // stray config on a machine that doesn't have it. A miss is a clean skip (exit 0), so the
    // installer step that calls us never errors.
    if !remove && !print_only && !target.present {
        eprintln!("{tool}: {}; skipping registration", target.absent_hint);
        return 0;
    }
    // Nothing to strip if the config was never created.
    if remove && !print_only && !cfg.exists() {
        return 0;
    }

    // Parse the existing config. Missing/empty → treat as `{}`. A present but MALFORMED file is
    // left UNTOUCHED (bail): it is the user's own client config (other MCP servers, and for
    // `~/.claude.json` the project/session state), and replacing it would lose a recoverable file.
    let existing = match std::fs::read_to_string(cfg) {
        Err(_) => Value::Null,
        Ok(s) if s.trim().is_empty() => Value::Null,
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "{tool}: existing {} is not valid JSON; leaving it unchanged",
                    cfg.display()
                );
                return 1;
            }
        },
    };

    let merged = if remove {
        ds_config::strip_mcp_server(existing, crate::SERVER_NAME)
    } else {
        let Some(cmd) = resolve_self_command() else {
            eprintln!("{tool}: could not resolve the dontspeak binary path");
            return 1;
        };
        // stdio server → no args (the no-arg mode IS the stdio MCP server).
        ds_config::merge_mcp_server(existing, crate::SERVER_NAME, &cmd, &[])
    };

    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => println!("// {}\n{s}", cfg.display()),
            Err(e) => {
                eprintln!("{tool}: serialize failed: {e}");
                return 1;
            }
        }
        return 0;
    }

    // Timestamped backup before overwriting — the SAME shared helper the hook writer uses. Surface
    // (don't swallow) a copy failure, then proceed: the user is warned the overwrite has no
    // recoverable copy, rather than the write being silently blocked.
    if let Err(e) = ds_config::backup_before_write(cfg, "json") {
        eprintln!(
            "{tool}: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
            cfg.display()
        );
    }
    match ds_config::atomic_write_json(cfg, &merged) {
        Ok(()) => {
            eprintln!(
                "{tool}: {} {}",
                if remove {
                    "removed dontspeak MCP server from"
                } else {
                    "registered dontspeak MCP server ->"
                },
                cfg.display()
            );
            if !remove {
                eprintln!("{tool}: {}", target.load_hint);
            }
            0
        }
        Err(e) => {
            eprintln!("{tool}: write failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target(cfg: &Path, present: bool) -> Target<'_> {
        Target {
            tool: "wire-test",
            config: cfg,
            present,
            absent_hint: "test client not detected (/x)".into(),
            load_hint: "reload to load the server",
        }
    }

    fn read(cfg: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap()
    }

    #[test]
    fn registers_into_missing_file_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        // First wire: file created, our entry present with a non-empty command, stdio (no args).
        assert_eq!(apply(&target(&cfg, true), false, false), 0);
        let v = read(&cfg);
        assert!(
            v["mcpServers"]["DontSpeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak")
        );
        assert!(v["mcpServers"]["DontSpeak"].get("args").is_none());
        // Re-wire: still exactly one entry (idempotent re-point, not a duplicate).
        assert_eq!(apply(&target(&cfg, true), false, false), 0);
        assert_eq!(read(&cfg)["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn preserves_sibling_servers_and_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        std::fs::write(
            &cfg,
            json!({
                "projects": { "/x": { "history": [] } },
                "mcpServers": { "keepme": { "command": "/usr/bin/keep" } }
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(apply(&target(&cfg, true), false, false), 0);
        let v = read(&cfg);
        // Ours added…
        assert!(v["mcpServers"]["DontSpeak"]["command"].is_string());
        // …the sibling server AND the unrelated top-level key are untouched.
        assert_eq!(v["mcpServers"]["keepme"]["command"], "/usr/bin/keep");
        assert_eq!(v["projects"]["/x"]["history"], json!([]));
    }

    #[test]
    fn remove_strips_only_ours_and_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        std::fs::write(
            &cfg,
            json!({ "mcpServers": {
                "DontSpeak": { "command": "/old/dontspeak" },
                "keepme": { "command": "/usr/bin/keep" }
            }})
            .to_string(),
        )
        .unwrap();
        assert_eq!(apply(&target(&cfg, true), true, false), 0);
        let v = read(&cfg);
        assert!(v["mcpServers"].get("DontSpeak").is_none());
        assert_eq!(v["mcpServers"]["keepme"]["command"], "/usr/bin/keep");
    }

    #[test]
    fn malformed_file_is_left_untouched_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        std::fs::write(&cfg, "{ this is not json").unwrap();
        assert_eq!(apply(&target(&cfg, true), false, false), 1);
        // The user's file is preserved byte-for-byte (recoverable), not clobbered.
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "{ this is not json");
    }

    #[test]
    fn print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        assert_eq!(apply(&target(&cfg, true), false, true), 0);
        assert!(!cfg.exists(), "preview must not create the file");
    }

    #[test]
    fn absent_client_skips_without_scattering_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        // present=false → clean skip (exit 0), no stray config created.
        assert_eq!(apply(&target(&cfg, false), false, false), 0);
        assert!(!cfg.exists());
        // …but a PREVIEW still works without the client present.
        assert_eq!(apply(&target(&cfg, false), false, true), 0);
        assert!(!cfg.exists());
    }

    #[test]
    fn backs_up_before_overwriting_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        std::fs::write(&cfg, json!({ "mcpServers": {} }).to_string()).unwrap();
        assert_eq!(apply(&target(&cfg, true), false, false), 0);
        // backup_before_write leaves a timestamped `.bak.<secs>` sibling before the overwrite.
        let has_bak = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".bak."));
        assert!(
            has_bak,
            "a timestamped backup is written before the overwrite"
        );
    }

    #[test]
    fn remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        assert_eq!(apply(&target(&cfg, true), true, false), 0);
        assert!(!cfg.exists());
    }
}
