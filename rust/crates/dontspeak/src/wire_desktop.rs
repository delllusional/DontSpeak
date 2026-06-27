//! `dontspeak wire-desktop [--remove] [--print-only]` — register (or remove) the
//! DontSpeak stdio MCP server in Claude DESKTOP's `claude_desktop_config.json`, so
//! Desktop can spawn this same bridge and call `speak`/`listen`/… on demand.
//!
//! This is the Desktop sibling of `wire-hooks`, and DELIBERATELY narrower: Claude
//! Desktop has no hook system, so there is NO narration wiring — only the
//! `mcpServers.dontspeak` registration. The merge/strip themselves live in `ds-config`
//! (the one definition, shared by every installer); this owns CLI parsing, the
//! self-path resolution, detection gating, the backup, and the atomic write.
//!
//! Safe by construction (same contract as wire-hooks): additive + idempotent (our
//! entry is overwritten so a reinstall re-points the command; other servers/keys are
//! preserved), a timestamped backup before writing, and a MALFORMED existing file is
//! left untouched rather than destroyed. `--print-only` previews to stdout.

use serde_json::Value;
use ds_config::Paths;

/// Resolve the absolute path to register as the MCP `command`. Prefer the stable
/// install location (`~/.local/bin/dontspeak`) so the entry survives rebuilds at
/// the same path; otherwise fall back to THIS executable (Windows `{app}`, the macOS
/// app bundle, or a dev build). Returns `None` only if neither can be resolved.
fn resolve_self_command() -> Option<String> {
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

pub fn run(args: &[String]) -> i32 {
    let mut remove = false;
    let mut print_only = false;
    for a in args {
        match a.as_str() {
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "-h" | "--help" => {
                eprintln!("usage: dontspeak wire-desktop [--remove] [--print-only]");
                return 0;
            }
            other => eprintln!("wire-desktop: ignoring unknown arg {other:?}"),
        }
    }

    let Some(paths) = Paths::resolve() else {
        eprintln!("wire-desktop: $HOME not set; nothing to do");
        return 1;
    };
    let cfg_path = paths.claude_desktop_config.clone();

    // For a real wire (not a removal or a preview), require Claude Desktop to actually
    // be present, so we never scatter a stray `Claude/` config dir on a machine that
    // doesn't have Desktop. Best-effort: a miss is a clean skip (exit 0), not a failure,
    // so the installer step that calls us never errors.
    if !remove && !print_only && !paths.claude_desktop_present() {
        eprintln!(
            "wire-desktop: Claude Desktop not detected ({}); skipping registration",
            paths.claude_desktop_dir.display()
        );
        return 0;
    }
    // Nothing to strip if the config file was never created.
    if remove && !print_only && !cfg_path.exists() {
        return 0;
    }

    // Parse the existing config. Missing/empty → treat as `{}`. A present but MALFORMED
    // file is left UNTOUCHED (bail) — it's the user's own Desktop config, and replacing
    // it would lose a recoverable file (and every other MCP server in it).
    let existing = match std::fs::read_to_string(&cfg_path) {
        Err(_) => Value::Null,
        Ok(s) if s.trim().is_empty() => Value::Null,
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(_) => {
                eprintln!(
                    "wire-desktop: existing {} is not valid JSON; leaving it unchanged",
                    cfg_path.display()
                );
                return 1;
            }
        },
    };

    let merged = if remove {
        ds_config::strip_mcp_server(existing, crate::SERVER_NAME)
    } else {
        let Some(cmd) = resolve_self_command() else {
            eprintln!("wire-desktop: could not resolve the dontspeak binary path");
            return 1;
        };
        // stdio server → no args (the no-arg mode IS the stdio MCP server).
        ds_config::merge_mcp_server(existing, crate::SERVER_NAME, &cmd, &[])
    };

    if print_only {
        match serde_json::to_string_pretty(&merged) {
            Ok(s) => println!("// {}\n{s}", cfg_path.display()),
            Err(e) => {
                eprintln!("wire-desktop: serialize failed: {e}");
                return 1;
            }
        }
        return 0;
    }

    // Best-effort timestamped backup before writing (mirrors wire-hooks).
    if cfg_path.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bak = cfg_path.with_extension(format!("json.bak.{ts}"));
        let _ = std::fs::copy(&cfg_path, &bak);
    }
    // atomic_write_json create_dir_all's the parent, so this also creates the
    // `Claude/` config dir for an installed-but-never-launched Desktop.
    match ds_config::atomic_write_json(&cfg_path, &merged) {
        Ok(()) => {
            eprintln!(
                "wire-desktop: {} {}",
                if remove {
                    "removed dontspeak MCP server from"
                } else {
                    "registered dontspeak MCP server ->"
                },
                cfg_path.display()
            );
            if !remove {
                eprintln!("wire-desktop: quit and reopen Claude Desktop to load the server");
            }
            0
        }
        Err(e) => {
            eprintln!("wire-desktop: write failed: {e}");
            1
        }
    }
}
