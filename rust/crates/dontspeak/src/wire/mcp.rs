//! Shared MCP-registration core for the [`wire`](crate::wire) orchestrator — the
//! `WireMechanism::JsonMcp` writer of the client registry, used today by Claude Code
//! (`~/.claude.json`) and Qwen Code (`~/.qwen/settings.json`, shared with its hooks surface).
//! Every client registers
//! the IDENTICAL stdio `mcpServers.DontSpeak` entry and differs only in WHICH config file, how
//! it's detected, and the user-facing labels — all declared on the client's
//! `ds_config::ClientSpec`, from which [`target_for`] builds the [`Target`] this one
//! read → merge/strip → backup → atomic-write flow consumes. It reuses the SAME `ds-config`
//! primitives the hook writers use (`merge_mcp_server`/`strip_mcp_server`,
//! `backup_before_write`, `atomic_write_json`), so an MCP registration is crash-safe in exactly
//! the way a settings.json hook write is.

use std::path::Path;

use super::io::{self, WriteBody};
use ds_config::{ClientSpec, Paths, Surface};

/// One client's registration target — the config file plus the client-specific gating and
/// labels that specialize the shared flow. Built by each subcommand from its [`Paths`].
pub struct Target<'a> {
    /// Label for log lines (the `wire` orchestrator sets `"wire"`).
    pub tool: &'a str,
    /// The config file to edit (Code's `~/.claude.json`).
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

/// Build the [`Target`] for one client's `JsonMcp` surface from its registry entry — the
/// config file, presence probe, and labels all come from the [`ClientSpec`], so a new
/// MCP-capable client is a registry entry, not a new constructor here.
pub fn target_for<'a>(
    spec: &'static ClientSpec,
    surface: &'static Surface,
    paths: &'a Paths,
) -> Target<'a> {
    Target {
        tool: "wire",
        config: (surface.config_file)(paths),
        present: (spec.present)(paths),
        absent_hint: format!(
            "{} not detected ({})",
            spec.display_name,
            (spec.detect_dir)(paths).display()
        ),
        load_hint: surface
            .load_hint
            .unwrap_or("reload the client to pick up the server"),
    }
}

/// Register (or, with `remove`, un-register) our stdio `mcpServers.DontSpeak` entry in
/// `target.config`, or PREVIEW the result with `print_only`. The ONE flow every `JsonMcp`
/// surface shares (the orchestrator builds the [`Target`] via [`target_for`]):
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

    // Missing/empty → `{}`; a MALFORMED file is left UNTOUCHED (it is the user's own client
    // config — other MCP servers, and for `~/.claude.json` the project/session state).
    let Ok(existing) = io::read_json_or_bail(tool, cfg) else {
        return 1;
    };

    let merged = if remove {
        ds_config::strip_mcp_server(existing, crate::SERVER_NAME)
    } else {
        let Some(cmd) = io::resolve_dontspeak_bin() else {
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

    let action = if remove {
        "removed dontspeak MCP server from"
    } else {
        "registered dontspeak MCP server ->"
    };
    let code = io::backup_then_write(tool, cfg, "json", &WriteBody::Json(&merged), action);
    if code == 0 && !remove {
        eprintln!("{tool}: {}", target.load_hint);
    }
    code
}

// Known, separate, OUT-OF-SCOPE leak (not fixed by the io.rs/hooks.rs test-isolation pass):
// `apply` with `remove: false` calls `io::resolve_dontspeak_bin()` (unparameterized — see that
// fn's doc comment), which resolves the REAL `ds_config::Paths::resolve()` (real `$HOME`) and
// does a real, READ-ONLY `.exists()` check against `$HOME/.local/bin/dontspeak` on the machine
// running the test. `registers_into_missing_file_then_is_idempotent`,
// `preserves_sibling_servers_and_unrelated_top_level_keys`, and
// `backs_up_before_overwriting_an_existing_file` below all call `apply(..., present: true, remove:
// false, ...)` and so hit this path. Benign (read-only, never asserted on) and identical in
// character to the leaks fixed elsewhere in this crate — flagged here rather than fixed because
// `apply`/`Target` don't yet take an injectable `Paths` param; a future pass could add one
// alongside `io::resolve_dontspeak_bin_at`.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

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
