//! Shared core for the two MCP-registration subcommands — `wire-code` (Claude CODE's
//! `~/.claude.json`) and `wire-desktop` (Claude DESKTOP's `claude_desktop_config.json`).
//! Both register the IDENTICAL stdio `mcpServers.dontspeak` entry and differ only in WHICH
//! config file, how they detect the client, and the user-facing labels — so the one
//! read → merge/strip → backup → atomic-write flow lives here ONCE rather than being copied
//! per client. It reuses the SAME `ds-config` primitives the hook installer uses
//! (`merge_mcp_server`/`strip_mcp_server`, `backup_before_write`, `atomic_write_json`), so an
//! MCP registration is crash-safe in exactly the way a `wire-hooks` settings.json write is.
//! The two subcommands are thin adapters: each builds a [`Target`] and calls [`apply`].

use std::path::Path;

use ds_config::Paths;
use serde_json::Value;

/// Outcome of CLI flag parsing: either `--help` was shown (the caller exits 0 without doing
/// anything) or we resolved the two booleans the flow needs.
pub enum Flags {
    /// `--help`/`-h` was passed; usage was printed. Caller returns 0.
    Help,
    /// Normal run with the parsed toggles.
    Run { remove: bool, print_only: bool },
}

/// Parse the `[--remove] [--print-only]` flags both MCP subcommands accept. Unknown args are
/// warned and ignored (matching `wire-hooks`), never fatal. `tool` names the subcommand in
/// the usage/warning lines.
pub fn parse_flags(tool: &str, args: &[String]) -> Flags {
    let mut remove = false;
    let mut print_only = false;
    for a in args {
        match a.as_str() {
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "-h" | "--help" => {
                eprintln!("usage: dontspeak {tool} [--remove] [--print-only]");
                return Flags::Help;
            }
            other => eprintln!("{tool}: ignoring unknown arg {other:?}"),
        }
    }
    Flags::Run { remove, print_only }
}

/// One client's registration target — the config file plus the client-specific gating and
/// labels that specialize the shared flow. Built by each subcommand from its [`Paths`].
pub struct Target<'a> {
    /// Subcommand name for log lines, e.g. `"wire-code"`.
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
/// (Same intent as `wire-hooks`' `sibling_bin`, without the Windows-shadowing guard the hook
/// installer needs — the MCP `command` is fine pointing at either deployed path.)
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

/// Register (or, with `remove`, un-register) our stdio `mcpServers.dontspeak` entry in
/// `target.config`, or PREVIEW the result with `print_only`. The ONE flow shared by
/// `wire-code` and `wire-desktop`:
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

    // Timestamped backup before overwriting — the SAME shared helper `wire-hooks` uses. Surface
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
    fn parse_flags_reads_toggles_and_help_and_ignores_unknown() {
        assert!(matches!(
            parse_flags("t", &["--remove".into()]),
            Flags::Run {
                remove: true,
                print_only: false
            }
        ));
        assert!(matches!(
            parse_flags("t", &["--print-only".into()]),
            Flags::Run {
                remove: false,
                print_only: true
            }
        ));
        // `--print` is an accepted alias of `--print-only`.
        assert!(matches!(
            parse_flags("t", &["--print".into()]),
            Flags::Run {
                print_only: true,
                ..
            }
        ));
        assert!(matches!(parse_flags("t", &["--help".into()]), Flags::Help));
        assert!(matches!(parse_flags("t", &["-h".into()]), Flags::Help));
        // Unknown args are warned-and-ignored, never fatal.
        assert!(matches!(
            parse_flags("t", &["--nope".into()]),
            Flags::Run {
                remove: false,
                print_only: false
            }
        ));
    }

    #[test]
    fn registers_into_missing_file_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        // First wire: file created, our entry present with a non-empty command, stdio (no args).
        assert_eq!(apply(&target(&cfg, true), false, false), 0);
        let v = read(&cfg);
        assert!(
            v["mcpServers"]["dontspeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak")
        );
        assert!(v["mcpServers"]["dontspeak"].get("args").is_none());
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
        assert!(v["mcpServers"]["dontspeak"]["command"].is_string());
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
                "dontspeak": { "command": "/old/dontspeak" },
                "keepme": { "command": "/usr/bin/keep" }
            }})
            .to_string(),
        )
        .unwrap();
        assert_eq!(apply(&target(&cfg, true), true, false), 0);
        let v = read(&cfg);
        assert!(v["mcpServers"].get("dontspeak").is_none());
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
