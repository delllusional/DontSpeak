//! MCP registration (`JsonMcp` / `TomlMcp`): stdio `DontSpeak` entry; target from registry.
//! Shared merge/strip + backup/atomic write. Print-only seed/capture: #30.

use std::path::Path;

use super::io::{self, WriteBody};
use crate::PreviewDoc;
use ds_config::{ClientSpec, Paths, Surface};

/// Per-client MCP write target (file + presence gate + labels).
pub struct Target<'a> {
    pub tool: &'a str,
    pub config: &'a Path,
    /// Gates real writes so we never scatter config without the client installed.
    pub present: bool,
    pub absent_hint: String,
    pub load_hint: &'a str,
}

/// Build [`Target`] from registry entry + surface.
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

/// Register/strip `mcpServers.DontSpeak` (or print-only). Malformed file left untouched.
/// Additive + idempotent. `seed`/`capture`: print-only grouping (JSON).
pub fn apply(
    target: &Target,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    let tool = target.tool;
    let cfg = target.config;

    if !remove && !print_only && !target.present {
        eprintln!("{tool}: {}; skipping registration", target.absent_hint);
        return 0;
    }
    if remove && !print_only && !cfg.exists() {
        return 0;
    }

    let existing = match seed {
        Some(PreviewDoc::Json(v)) => v,
        Some(PreviewDoc::Toml(_) | PreviewDoc::Yaml(_)) => {
            panic!("mcp::apply: seed must be PreviewDoc::Json for a JSON mechanism")
        }
        None => {
            let Ok(v) = io::read_json_or_bail(tool, cfg) else {
                // Match hooks: shared-file clients must not report contradictory outcomes.
                return 0;
            };
            v
        }
    };
    let before = existing.clone();

    let merged = if remove {
        ds_config::strip_mcp_server(existing, crate::SERVER_NAME)
    } else {
        let Some(cmd) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("{tool}: could not resolve the dontspeak binary path");
            return 1;
        };
        // No-arg mode is the stdio MCP server.
        ds_config::merge_mcp_server(existing, crate::SERVER_NAME, &cmd, &[])
    };

    if print_only {
        return match capture {
            Some(slot) => {
                *slot = Some(PreviewDoc::Json(merged));
                0
            }
            None => match serde_json::to_string_pretty(&merged) {
                Ok(s) => {
                    println!("// {}\n{s}", cfg.display());
                    0
                }
                Err(e) => {
                    eprintln!("{tool}: serialize failed: {e}");
                    1
                }
            },
        };
    }

    // Load-bearing: every-boot reconcile must be zero-write when unchanged.
    if merged == before {
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

/// TOML MCP configs (format-preserving). Same contract as [`apply`] (`PreviewDoc::Toml`).
pub fn apply_toml(
    target: &Target,
    remove: bool,
    print_only: bool,
    paths: &Paths,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    let tool = target.tool;
    let cfg = target.config;

    if !remove && !print_only && !target.present {
        eprintln!("{tool}: {}; skipping registration", target.absent_hint);
        return 0;
    }
    if remove && !print_only && !cfg.exists() {
        return 0;
    }

    let existing = match seed {
        Some(PreviewDoc::Toml(s)) => s,
        Some(PreviewDoc::Json(_) | PreviewDoc::Yaml(_)) => {
            panic!("mcp::apply_toml: seed must be PreviewDoc::Toml for a TOML mechanism")
        }
        None => std::fs::read_to_string(cfg).unwrap_or_default(),
    };

    let merged = if remove {
        match ds_config::strip_mcp_server_toml(&existing, crate::SERVER_NAME) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{tool}: {e}");
                return 1;
            }
        }
    } else {
        let Some(cmd) = io::resolve_dontspeak_bin_at(Some(paths)) else {
            eprintln!("{tool}: could not resolve the dontspeak binary path");
            return 1;
        };
        match ds_config::merge_mcp_server_toml(&existing, crate::SERVER_NAME, &cmd, &[]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{tool}: {e}");
                return 1;
            }
        }
    };

    if print_only {
        return match capture {
            Some(slot) => {
                *slot = Some(PreviewDoc::Toml(merged));
                0
            }
            None => {
                println!("// {}\n{}", cfg.display(), merged);
                0
            }
        };
    }

    if merged == existing {
        return 0;
    }

    let action = if remove {
        "removed dontspeak MCP server from"
    } else {
        "registered dontspeak MCP server ->"
    };
    let code = io::backup_then_write(tool, cfg, "toml", &WriteBody::Str(&merged), action);
    if code == 0 && !remove {
        eprintln!("{tool}: {}", target.load_hint);
    }
    code
}

// Tests inject tempdir `Paths` so bin resolution never hits real $HOME.
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

    fn rooted(dir: &Path) -> Paths {
        Paths::rooted_at(dir)
    }

    fn read(cfg: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(cfg).unwrap()).unwrap()
    }

    #[test]
    fn registers_into_missing_file_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        assert_eq!(
            apply(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        let v = read(&cfg);
        assert!(
            v["mcpServers"]["DontSpeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak")
        );
        assert!(v["mcpServers"]["DontSpeak"].get("args").is_none());
        assert_eq!(
            apply(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        assert_eq!(read(&cfg)["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn preserves_sibling_servers_and_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        std::fs::write(
            &cfg,
            json!({
                "projects": { "/x": { "history": [] } },
                "mcpServers": { "keepme": { "command": "/usr/bin/keep" } }
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            apply(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        let v = read(&cfg);
        assert!(v["mcpServers"]["DontSpeak"]["command"].is_string());
        assert_eq!(v["mcpServers"]["keepme"]["command"], "/usr/bin/keep");
        assert_eq!(v["projects"]["/x"]["history"], json!([]));
    }

    #[test]
    fn remove_strips_only_ours_and_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        std::fs::write(
            &cfg,
            json!({ "mcpServers": {
                "DontSpeak": { "command": "/old/dontspeak" },
                "keepme": { "command": "/usr/bin/keep" }
            }})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            apply(&target(&cfg, true), true, false, &paths, None, None),
            0
        );
        let v = read(&cfg);
        assert!(v["mcpServers"].get("DontSpeak").is_none());
        assert_eq!(v["mcpServers"]["keepme"]["command"], "/usr/bin/keep");
    }

    #[test]
    fn malformed_file_is_left_untouched_and_is_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        std::fs::write(&cfg, "{ this is not json").unwrap();
        assert_eq!(
            apply(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "{ this is not json");
    }

    #[test]
    fn print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        assert_eq!(
            apply(&target(&cfg, true), false, true, &paths, None, None),
            0
        );
        assert!(!cfg.exists(), "preview must not create the file");
    }

    #[test]
    fn absent_client_skips_without_scattering_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        assert_eq!(
            apply(&target(&cfg, false), false, false, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
        assert_eq!(
            apply(&target(&cfg, false), false, true, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn backs_up_before_overwriting_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".claude.json");
        let paths = rooted(dir.path());
        std::fs::write(&cfg, json!({ "mcpServers": {} }).to_string()).unwrap();
        assert_eq!(
            apply(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
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
        let paths = rooted(dir.path());
        assert_eq!(
            apply(&target(&cfg, true), true, false, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }

    // `apply_toml` mirrors of the `apply` (JSON) tests above — same `target()` helper (it's
    // format-agnostic), just reading the written file as raw TOML text instead of JSON.

    #[test]
    fn toml_registers_into_missing_file_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        assert_eq!(
            apply_toml(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("[mcp_servers.DontSpeak]"));
        assert!(text.contains("command ="));
        assert!(!text.contains("args ="), "stdio entry carries no args key");
        // Re-wire: still exactly one DontSpeak table (idempotent re-point, not a duplicate).
        assert_eq!(
            apply_toml(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        assert_eq!(
            std::fs::read_to_string(&cfg)
                .unwrap()
                .matches("[mcp_servers.DontSpeak]")
                .count(),
            1
        );
    }

    #[test]
    fn toml_preserves_sibling_tables_and_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        std::fs::write(
            &cfg,
            "theme = \"dark\"\n\n[mcp_servers.keepme]\ncommand = \"/usr/bin/keep\"\n",
        )
        .unwrap();
        assert_eq!(
            apply_toml(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("[mcp_servers.DontSpeak]"));
        assert!(text.contains("[mcp_servers.keepme]"));
        assert!(text.contains("command = \"/usr/bin/keep\""));
        assert!(text.contains("theme = \"dark\""));
    }

    #[test]
    fn toml_remove_strips_only_ours_and_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        std::fs::write(
            &cfg,
            "[mcp_servers.DontSpeak]\ncommand = \"/old/dontspeak\"\n\n[mcp_servers.keepme]\ncommand = \"/usr/bin/keep\"\n",
        )
        .unwrap();
        assert_eq!(
            apply_toml(&target(&cfg, true), true, false, &paths, None, None),
            0
        );
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("DontSpeak"));
        assert!(text.contains("[mcp_servers.keepme]"));
    }

    /// Regression for the bug this PR shipped with: `strip_mcp_server_toml` used to swallow a
    /// parse failure into `Ok(existing)`, so `--remove` against a malformed config.toml printed
    /// "removed dontspeak MCP server from ..." and returned 0 without ever touching the file.
    /// Must match `apply`'s JSON convention: a hard error (1), file left byte-identical.
    #[test]
    fn malformed_toml_file_is_left_untouched_and_errors_on_remove() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        let bad = "this is not [ valid toml";
        std::fs::write(&cfg, bad).unwrap();
        assert_eq!(
            apply_toml(&target(&cfg, true), true, false, &paths, None, None),
            1
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), bad);
    }

    #[test]
    fn malformed_toml_file_is_left_untouched_and_errors_on_merge() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        let bad = "this is not [ valid toml";
        std::fs::write(&cfg, bad).unwrap();
        assert_eq!(
            apply_toml(&target(&cfg, true), false, false, &paths, None, None),
            1
        );
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), bad);
    }

    #[test]
    fn toml_print_only_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        assert_eq!(
            apply_toml(&target(&cfg, true), false, true, &paths, None, None),
            0
        );
        assert!(!cfg.exists(), "preview must not create the file");
    }

    #[test]
    fn toml_absent_client_skips_without_scattering_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        assert_eq!(
            apply_toml(&target(&cfg, false), false, false, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
        assert_eq!(
            apply_toml(&target(&cfg, false), false, true, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn toml_backs_up_before_overwriting_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        std::fs::write(&cfg, "[mcp_servers]\n").unwrap();
        assert_eq!(
            apply_toml(&target(&cfg, true), false, false, &paths, None, None),
            0
        );
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
    fn toml_remove_on_missing_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let paths = rooted(dir.path());
        assert_eq!(
            apply_toml(&target(&cfg, true), true, false, &paths, None, None),
            0
        );
        assert!(!cfg.exists());
    }
}
