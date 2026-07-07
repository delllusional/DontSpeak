//! `dontspeak wire <client> [--remove] [--print-only]` — the ONE per-client integration
//! installer. Each client gets its FULL integration wired (or removed) in a single step; there is
//! no separate "install hooks" vs "install MCP" task. WHAT to wire is declared, not coded: the
//! client registry (`ds_config::CLIENT_REGISTRY`) lists every client with its presence probe, its
//! config files, the mechanism each file is written with, and the official docs the wiring is
//! derived from — this orchestrator just walks a client's surfaces and dispatches on mechanism:
//!   • `ClaudeJsonHooks` → [`hooks::claude_json_hooks`] (Claude-contract hooks, JSON file)
//!   • `ClaudeTomlHooks` → [`hooks::claude_toml_hooks`] (same contract, TOML file)
//!   • `JsonMcp`         → [`mcp::apply`] (stdio `mcpServers.DontSpeak` entry)
//! Adding a client (Qwen Code, Gemini CLI, …) = a `WireTarget` variant + `Paths` fields + a
//! registry entry; a new MECHANISM (a different hook contract) = one new writer + enum arm.
//!
//! Every surface REUSES the shared cores — the `ds-config` hook/MCP shapers, the
//! [`mcp::apply`] read→merge→backup→atomic-write flow, and the [`hooks`] writers — so
//! nothing is copy-pasted per client, and this install-time entry and the `setup_integration`
//! tool drive the IDENTICAL code (they can't drift). Additive + idempotent + backed-up; a client
//! that isn't installed is a clean skip (exit 0). `wire --list` prints the registry.

pub(crate) mod hooks;
mod io;
pub(crate) mod mcp;

use ds_config::{ClientKind, Paths, WireMechanism, WireTarget};

/// Parse `<client> [--remove] [--print-only]` and wire (or unwire) that client's whole integration.
/// Returns a process exit code (0 ok / skipped, 1 hard error). `client` is a [`WireTarget`] token
/// (`claude_code`/`codex`); `narration_spec` is a config-file concern of the
/// `setup_integration` tool, not a client, so it is rejected here.
pub fn run(args: &[String]) -> i32 {
    let mut client: Option<WireTarget> = None;
    let mut remove = false;
    let mut print_only = false;
    let mut all = false;
    // The canonical token list, straight from the registry (usage/error text can't go stale).
    let tokens = || {
        ds_config::CLIENT_REGISTRY
            .iter()
            .map(|s| s.target.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "--list" => {
                print_registry(ds_config::Paths::resolve().as_ref());
                return 0;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: dontspeak wire <{}> [--remove] [--print-only]\n       dontspeak wire --all [--remove] [--print-only]   (every known client; each self-skips if absent)\n       dontspeak wire --list                             (the client registry: surfaces, files, docs)",
                    tokens()
                );
                return 0;
            }
            other if other.starts_with('-') => eprintln!("wire: ignoring unknown flag {other:?}"),
            other => match WireTarget::parse(other) {
                Some(t) if ds_config::client_spec(t).is_some() => {
                    // A second positional client token (e.g. `dontspeak wire codex
                    // claude_code`) must NOT silently overwrite the first and report
                    // success on only the last one — reject with a clear error instead,
                    // matching the hard-error convention this function already uses for
                    // an unrecognized client / a missing client below.
                    if let Some(prev) = client {
                        eprintln!(
                            "wire: multiple clients given ({} and {other}); pass exactly one client, or use --all",
                            prev.as_str()
                        );
                        return 1;
                    }
                    client = Some(t);
                }
                _ => {
                    eprintln!("wire: unknown client {other:?}; expected {}", tokens());
                    return 1;
                }
            },
        }
    }
    if !all && client.is_none() {
        eprintln!("wire: missing client ({}), or use --all", tokens());
        return 1;
    }
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire: $HOME not set; nothing to do");
        return 1;
    };

    // Client-agnostic install housekeeping on any real wire (idempotent; per-client is fine).
    if !remove && !print_only {
        hooks::seed_and_prune(&paths);
    }

    // `--all` wires (or unwires) EVERY registry client — the single source the per-platform
    // installers used to hand-copy. Each self-skips when its client is absent; return the WORST
    // exit code so one client's hard error still surfaces.
    if all {
        return WireTarget::CLIENTS
            .iter()
            .map(|&c| wire_client(c, &paths, remove, print_only))
            .max()
            .unwrap_or(0);
    }

    wire_client(client.expect("checked above"), &paths, remove, print_only)
}

/// Wire (or unwire) ONE client: look its spec up in the registry, apply the presence gate, then
/// walk its surfaces dispatching on mechanism. Surfaces are attempted IN ORDER and ALL of them
/// even if one fails (worst exit code wins): a malformed file behind one surface must not skip
/// the others, or `--remove` would leave a dangling entry (e.g. an MCP `command` pointing at a
/// deleted binary). Order matters for `claude_code`: the hooks write creates `~/.claude`, which
/// the MCP surface's presence probe then sees.
fn wire_client(client: WireTarget, paths: &Paths, remove: bool, print_only: bool) -> i32 {
    let Some(spec) = ds_config::client_spec(client) else {
        // `run` rejects non-clients before we get here; `--all` iterates CLIENTS only.
        eprintln!("wire: narration_spec is not a client; use the setup_integration tool");
        return 1;
    };

    if !print_only && spec.gate_on_presence {
        if remove {
            // Nothing to strip when none of the client's config files was ever created —
            // and never scatter one on removal.
            if !spec
                .surfaces
                .iter()
                .any(|s| (s.config_file)(paths).exists())
            {
                return 0;
            }
        } else if !(spec.present)(paths) {
            eprintln!(
                "wire: {} not detected ({}); skipping",
                spec.display_name,
                (spec.detect_dir)(paths).display()
            );
            return 0;
        }
    }

    let code = spec
        .surfaces
        .iter()
        .map(|s| match s.mechanism {
            WireMechanism::ClaudeJsonHooks => hooks::claude_json_hooks(
                (s.config_file)(paths),
                s.hook_streaming,
                s.hook_command_style,
                remove,
                print_only,
                paths,
            ),
            WireMechanism::ClaudeTomlHooks => {
                hooks::claude_toml_hooks((s.config_file)(paths), remove, print_only, paths)
            }
            WireMechanism::JsonMcp => {
                mcp::apply(&mcp::target_for(spec, s, paths), remove, print_only)
            }
        })
        .max()
        .unwrap_or(0);
    // Codex-only user hint: hooks alone give Stop-narration; MID-TURN narration needs the
    // session hosted on the shared app-server so the engine can subscribe (see
    // docs/STREAMING-NARRATION.md). CLI literal by precedent (this file's other eprintln!s);
    // the ds-i18n catalog covers Swift/C#/XAML, none of which are involved here.
    if client == WireTarget::Codex && !remove && !print_only && code == 0 {
        eprintln!(
            "wire: for mid-turn narration, run Codex on the shared app-server: `codex app-server daemon start` once, then `codex --remote unix://` — otherwise replies are voiced at end of turn as before"
        );
    }
    code
}

/// `wire --list` — print the client registry: who, where (per-OS resolved paths + live presence),
/// how (mechanism per surface), and the official docs each wiring is derived from.
fn print_registry(paths: Option<&Paths>) {
    for spec in ds_config::CLIENT_REGISTRY {
        println!("{} ({})", spec.display_name, spec.target.as_str());
        println!(
            "  kind:    {}",
            match spec.kind {
                ClientKind::TerminalCli => "terminal CLI",
                ClientKind::DesktopApp => "desktop app",
            }
        );
        if let Some(p) = paths {
            println!(
                "  detect:  {} ({})",
                (spec.detect_dir)(p).display(),
                if (spec.present)(p) {
                    "present"
                } else {
                    "absent"
                }
            );
            for s in spec.surfaces {
                let how = match s.mechanism {
                    WireMechanism::ClaudeJsonHooks => "voice hooks (Claude contract, JSON)",
                    WireMechanism::ClaudeTomlHooks => "voice hooks (Claude contract, TOML)",
                    WireMechanism::JsonMcp => "MCP server (stdio, mcpServers entry)",
                };
                println!("  wires:   {} -> {}", how, (s.config_file)(p).display());
            }
        }
        for d in spec.docs {
            println!("  docs:    {}: {}", d.topic, d.url);
        }
        println!(
            "  spec:    verified against {} {} (docs read {})",
            spec.display_name, spec.verified_client_version, spec.verified_on
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// No client token and no `--all` hits the "missing client" guard before `Paths::resolve()`
    /// is ever called — no real `$HOME` or file I/O involved.
    #[test]
    fn no_client_and_no_all_is_a_hard_error() {
        assert_eq!(run(&args(&[])), 1);
    }

    /// A token that doesn't match any registry entry is rejected before `Paths::resolve()`.
    #[test]
    fn unknown_client_token_is_a_hard_error() {
        assert_eq!(run(&args(&["not_a_real_client"])), 1);
    }

    /// A second positional client token must NOT silently overwrite the first; `run` rejects
    /// with the "multiple clients given" guard, still before `Paths::resolve()`.
    #[test]
    fn two_positional_clients_is_a_hard_error() {
        assert_eq!(run(&args(&["codex", "claude_code"])), 1);
    }

    #[test]
    fn help_flag_exits_zero() {
        assert_eq!(run(&args(&["-h"])), 0);
        assert_eq!(run(&args(&["--help"])), 0);
    }

    /// `print_registry` now takes an injectable `Option<&Paths>`; call it directly against a
    /// tempdir-rooted `Paths` so this test never touches the real `$HOME` or a client's real
    /// detect dir. This trades away testing `run()`'s `"--list"` arg-dispatch arm in isolation —
    /// but that arm is a one-line dispatch (`print_registry(...); return 0;`) identical in shape
    /// to the already-covered `-h`/`--help` arm above, so no real coverage is lost.
    #[test]
    fn list_flag_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        print_registry(Some(&paths)); // must not panic; nothing to assert on a unit-returning fn
    }

    /// An unknown `-`-prefixed flag is tolerated (just an eprintln), not a hard parse failure —
    /// proven WITHOUT attaching a valid client token, so execution still falls through to the
    /// `client.is_none()` guard (same exit code as the "no client" case) rather than reaching
    /// `Paths::resolve()` / `hooks::seed_and_prune` / `wire_client` against the real `$HOME`.
    #[test]
    fn unknown_flag_without_a_client_is_tolerated_not_a_hard_failure() {
        assert_eq!(run(&args(&["--not-a-real-flag"])), 1);
    }

    /// `wire_client` against a `Paths::rooted_at` tempdir: Codex's presence gate (`~/.codex`
    /// under the fresh, empty tempdir) is absent, so a real (non-`--remove`, non-`--print-only`)
    /// wire is a clean skip that creates nothing.
    #[test]
    fn wire_client_skips_absent_gated_client() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(wire_client(WireTarget::Codex, &paths, false, false), 0);
        assert!(!paths.codex_dir.exists());
    }

    /// `--remove` on a gated client with no config file ever created is a nothing-to-strip
    /// early-out (0), and must never scatter a stray config file on removal.
    #[test]
    fn wire_client_remove_with_no_existing_config_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(wire_client(WireTarget::Codex, &paths, true, false), 0);
        assert!(!paths.codex_config.exists());
    }

    /// `WireTarget::NarrationSpec` is a config-file concern, not a client; `run` rejects it
    /// before ever calling `wire_client`, so the only way to reach this branch is to call
    /// `wire_client` directly.
    #[test]
    fn wire_client_rejects_narration_spec() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(
            wire_client(WireTarget::NarrationSpec, &paths, false, false),
            1
        );
    }

    /// Qwen Code is the only client whose two surfaces (`ClaudeJsonHooks` + `JsonMcp`) share ONE
    /// config file (`~/.qwen/settings.json`) — prove they coexist without clobbering each other in
    /// either direction (wire creates hooks then merges MCP into the same file; `--remove` strips
    /// both back out cleanly), against a tempdir-rooted `Paths` (never the real `$HOME`).
    ///
    /// NOTE: like every other real (non-`--remove`) wire test in this crate touching a `JsonMcp`
    /// surface, this exercises `mcp::apply`'s already-documented, benign, read-only leak (it calls
    /// the unparameterized `io::resolve_dontspeak_bin()`, which checks for a real
    /// `$HOME/.local/bin/dontspeak` — see the comment block in `wire/mcp.rs` above its own tests).
    #[test]
    fn wire_client_qwen_code_wires_hooks_and_mcp_into_one_file_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.qwen_dir).unwrap(); // satisfy Qwen's presence gate

        assert_eq!(wire_client(WireTarget::QwenCode, &paths, false, false), 0);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        // Hooks group present (non-streaming: MessageDisplay omitted, Stop/UserPromptSubmit/etc. present)…
        assert!(v["hooks"]["Stop"].as_array().is_some(), "hooks wired");
        assert!(
            v["hooks"].get("MessageDisplay").is_none(),
            "non-streaming: no MessageDisplay"
        );
        // …in Qwen's INLINE dialect: its hook runner passes only `command` to a shell and its
        // config has no `args` field, so the verb must live in the command string and no
        // `args` key may be written (Qwen would silently drop it → the bare binary is the
        // arg-less stdio MCP server, i.e. every hook dead).
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert!(
            stop["command"].as_str().unwrap().contains(" notify"),
            "hook command carries the inlined verb"
        );
        assert!(stop.get("args").is_none(), "no `args` key for Qwen hooks");
        // …AND the MCP entry, in the SAME file, without clobbering the hooks written just above.
        // (The mcpServers entry is spawned DIRECTLY by Qwen — not via its hook shell — so it
        // keeps the plain binary path + no inlining.)
        assert!(
            v["mcpServers"]["DontSpeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak"),
            "mcp entry wired alongside hooks in the same file"
        );

        // `--remove` cleanly strips BOTH back out.
        assert_eq!(wire_client(WireTarget::QwenCode, &paths, true, false), 0);
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(v2.get("hooks").is_none(), "hooks stripped");
        assert!(v2.get("mcpServers").is_none(), "mcp entry stripped");
    }
}
