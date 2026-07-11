//! The client-wiring orchestrator + writers — SHARED by the `dontspeak` CLI's
//! `wire <client> [--remove] [--print-only]` subcommand ([`run`]) and the `dontspeakd`
//! engine's boot-time / config-change convergence ([`reconcile`]). Each client gets its FULL
//! integration wired (or removed) in one step; there is no separate "install hooks" vs
//! "install MCP" task. WHAT to wire is declared, not coded: the client registry
//! (`ds_config::CLIENT_REGISTRY`) lists every client with its presence probe, its config
//! files, the mechanism each file is written with, and the official docs the wiring is
//! derived from — this orchestrator just walks a client's surfaces and dispatches on mechanism:
//!   • `ClaudeJsonHooks` → `hooks::claude_json_hooks` (Claude-contract hooks, JSON file)
//!   • `ClaudeTomlHooks` → `hooks::claude_toml_hooks` (same contract, TOML file)
//!   • `JsonMcp`         → `mcp::apply` (stdio `mcpServers.DontSpeak` entry)
//!   • `TomlMcp`         → `mcp::apply_toml` (stdio `mcp_servers.DontSpeak` in TOML)
//! Adding a client (Qwen Code, Gemini CLI, …) = a `WireTarget` variant + `Paths` fields + a
//! registry entry; a new MECHANISM (a different hook contract) = one new writer + enum arm.
//!
//! Every surface REUSES the shared cores — the `ds-config` hook/MCP shapers, the
//! `mcp::apply` read→merge→backup→atomic-write flow, and the `hooks` writers — so
//! nothing is copy-pasted per client, and the interactive `wire` entry and the engine's
//! automatic reconcile drive the IDENTICAL code (they can't drift). Additive + idempotent +
//! backed-up; a client that isn't installed is a clean skip (exit 0). `wire --list` prints
//! the registry.

pub(crate) mod hooks;
mod io;
pub(crate) mod mcp;

use ds_config::{ClientKind, Paths, WireMechanism, WireTarget};

/// Our MCP server key — the `mcpServers.DontSpeak` / `mcp_servers.DontSpeak` registry name
/// (and the `serverInfo.name` the stdio server reports). MUST stay equal to
/// `dontspeak::mcp::SERVER_NAME`; the moved `mcp` writer registers/strips under this literal.
pub const SERVER_NAME: &str = "DontSpeak";

/// Parse `<client> [--remove] [--print-only]` (or `--all` / `--reconcile` / `--list`) and wire
/// (or unwire) that client's whole integration. Returns a process exit code (0 ok / skipped,
/// 1 hard error). `client` is a [`WireTarget`] token (`claude_code`/`codex`/`qwen_code`/`grok`).
/// `--reconcile` converges every client to `config.toml`'s declared `exclude_clients` (the same
/// core the engine runs at boot, via [`reconcile`]); `--all` wires every client unconditionally.
pub fn run(args: &[String]) -> i32 {
    let mut client: Option<WireTarget> = None;
    let mut remove = false;
    let mut print_only = false;
    let mut all = false;
    let mut do_reconcile = false;
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
            "--reconcile" => do_reconcile = true,
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "--list" => {
                print_registry(ds_config::Paths::resolve().as_ref());
                return 0;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: dontspeak wire <{}> [--remove] [--print-only]\n       dontspeak wire --all [--remove] [--print-only]   (every known client; each self-skips if absent)\n       dontspeak wire --reconcile                        (converge every client to config.toml's exclude_clients)\n       dontspeak wire --list                             (the client registry: surfaces, files, docs)",
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
    // `--reconcile` carries NO client token, so it must be exempt from the missing-client
    // guard (it runs BEFORE `Paths::resolve()`).
    if !all && !do_reconcile && client.is_none() {
        eprintln!("wire: missing client ({}), or use --all", tokens());
        return 1;
    }
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire: $HOME not set; nothing to do");
        return 1;
    };

    // Client-agnostic install housekeeping on any real wire (idempotent; per-client is fine).
    // `--reconcile` runs this too, so an install-time `wire --reconcile` still seeds config +
    // prunes stale bins BEFORE the per-client convergence below — keeping seed/prune on the
    // interactive `wire` path only (the engine's [`reconcile`] never seeds/prunes).
    if !remove && !print_only {
        hooks::seed_and_prune(&paths);
    }

    // `--reconcile`: converge every client to config.toml's declared `exclude_clients` (the SAME
    // core the engine drives at boot). Placed before `--all` so `wire --reconcile` never also
    // does an unconditional `--all`.
    if do_reconcile {
        return reconcile(&paths);
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

/// Converge each registry client's wiring to config.toml's declared `exclude_clients` (absent
/// or empty ⇒ exclude nothing ⇒ all wired). ONLY per-client wiring — it never prunes/deletes
/// binaries and does NOT seed config (the engine seeds narration-spec separately). Called
/// in-process by the engine at boot / on config change, and by `wire --reconcile`. A client
/// LISTED in `exclude_clients` is `--remove`d (a clean no-op when its config was never
/// created); every other client is wired (self-skipping when the client isn't installed).
/// Returns the WORST per-client exit code (0 ok).
pub fn reconcile(paths: &Paths) -> i32 {
    let excluded = ds_config::VoiceConfig::load(paths).excluded_clients();
    WireTarget::CLIENTS
        .iter()
        .map(|&c| {
            wire_client(
                c,
                paths,
                /*remove=*/ excluded.contains(&c),
                /*print_only=*/ false,
            )
        })
        .max()
        .unwrap_or(0)
}

/// Wire (or unwire) ONE client: look its spec up in the registry, apply the presence gate, then
/// walk its surfaces dispatching on mechanism. Surfaces are attempted IN ORDER and ALL of them
/// even if one fails (worst exit code wins): a malformed file behind one surface must not skip
/// the others, or `--remove` would leave a dangling entry (e.g. an MCP `command` pointing at a
/// deleted binary). Order matters for `claude_code`: the hooks write creates `~/.claude`, which
/// the MCP surface's presence probe then sees.
fn wire_client(client: WireTarget, paths: &Paths, remove: bool, print_only: bool) -> i32 {
    let spec = ds_config::client_spec(client)
        .expect("WireTarget is client-only; every variant has a registry entry");

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
            WireMechanism::GrokJsonHooks => {
                hooks::grok_json_hooks((s.config_file)(paths), remove, print_only, paths)
            }
            WireMechanism::JsonMcp => {
                mcp::apply(&mcp::target_for(spec, s, paths), remove, print_only, paths)
            }
            WireMechanism::TomlMcp => {
                mcp::apply_toml(&mcp::target_for(spec, s, paths), remove, print_only, paths)
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
                    WireMechanism::GrokJsonHooks => {
                        "voice hooks (Claude contract, JSON — dedicated Grok file)"
                    }
                    WireMechanism::JsonMcp => "MCP server (stdio, mcpServers entry)",
                    WireMechanism::TomlMcp => "MCP server (stdio, mcp_servers table in TOML)",
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

    /// Qwen Code is the only client whose two surfaces (`ClaudeJsonHooks` + `JsonMcp`) share ONE
    /// config file (`~/.qwen/settings.json`) — prove they coexist without clobbering each other in
    /// either direction (wire creates hooks then merges MCP into the same file; `--remove` strips
    /// both back out cleanly), against a tempdir-rooted `Paths` (never the real `$HOME`).
    ///
    /// Hermetic: `wire_client` threads this test's tempdir-rooted `Paths` all the way into
    /// `mcp::apply`'s bin resolution (`resolve_dontspeak_bin_at(Some(paths))`), so nothing here
    /// reads the real `$HOME`/`~/.local/bin`.
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

    /// Codex is the TOML analog of the Qwen test above: its two surfaces (`ClaudeTomlHooks` +
    /// `TomlMcp`) now share ONE config file (`~/.codex/config.toml`) — prove they coexist
    /// without clobbering each other in either direction, against a tempdir-rooted `Paths`
    /// (never the real `$HOME`).
    ///
    /// Hermetic like the Qwen test above: `wire_client` threads the tempdir-rooted `Paths`
    /// into `mcp::apply_toml`'s bin resolution, so nothing reads the real `$HOME`.
    #[test]
    fn wire_client_codex_wires_hooks_and_mcp_into_one_file_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap(); // satisfy Codex's presence gate

        assert_eq!(wire_client(WireTarget::Codex, &paths, false, false), 0);
        let text = std::fs::read_to_string(&paths.codex_config).unwrap();
        // Hooks present (Codex's own event set, greet-only SessionStart)…
        assert!(text.contains("[[hooks.Stop]]"), "hooks wired: {text}");
        // …AND the MCP entry, in the SAME file, without clobbering the hooks written just above.
        assert!(
            text.contains("[mcp_servers.DontSpeak]"),
            "mcp entry wired alongside hooks in the same file: {text}"
        );
        assert!(
            text.contains("command ="),
            "mcp entry carries a command: {text}"
        );

        // `--remove` cleanly strips BOTH back out.
        assert_eq!(wire_client(WireTarget::Codex, &paths, true, false), 0);
        let text2 = std::fs::read_to_string(&paths.codex_config).unwrap();
        assert!(!text2.contains("hooks"), "hooks stripped: {text2}");
        assert!(
            !text2.contains("mcp_servers"),
            "mcp entry stripped: {text2}"
        );
    }

    /// Grok wires TWO surfaces like Codex/Qwen — but its `GrokJsonHooks` + `TomlMcp` surfaces
    /// live in DIFFERENT files (the dedicated `~/.grok/hooks/dontspeak.json` we own outright,
    /// and `~/.grok/config.toml` for MCP). Prove wire creates both and `--remove` deletes our
    /// hooks file AND strips the MCP entry from the config, against a tempdir-rooted `Paths`.
    ///
    /// Hermetic like the Codex/Qwen two-surface tests above: `mcp::apply_toml` resolves the bin
    /// via the injected tempdir-rooted `Paths`, so nothing reads the real `$HOME` (see the
    /// comment block in `wire/mcp.rs`).
    #[test]
    fn wire_client_grok_wires_both_surfaces_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.grok_dir).unwrap(); // satisfy Grok's presence gate

        assert_eq!(wire_client(WireTarget::Grok, &paths, false, false), 0);
        // MCP entry in the TOML config…
        let text = std::fs::read_to_string(&paths.grok_config).unwrap();
        assert!(
            text.contains("[mcp_servers.DontSpeak]"),
            "mcp entry wired into grok config: {text}"
        );
        // …and the dedicated hooks file, in a SEPARATE file, with a Stop hook that voices the
        // reply (command carries our binary, a numeric seconds timeout, and NO async key).
        assert!(
            paths.grok_hooks_json.exists(),
            "dedicated hooks file created"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.grok_hooks_json).unwrap())
                .unwrap();
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert!(
            stop["command"].as_str().unwrap().contains("dontspeak"),
            "Stop hook command invokes our binary: {stop}"
        );
        assert!(
            stop.get("timeout")
                .and_then(serde_json::Value::as_i64)
                .is_some(),
            "Stop hook carries a numeric timeout: {stop}"
        );
        assert!(
            stop.get("async").is_none(),
            "Grok hooks run synchronously — no async key: {stop}"
        );

        // `--remove` deletes our dedicated hooks file AND strips the MCP entry from the config.
        assert_eq!(wire_client(WireTarget::Grok, &paths, true, false), 0);
        assert!(
            !paths.grok_hooks_json.exists(),
            "dedicated hooks file deleted on unwire"
        );
        let text2 = std::fs::read_to_string(&paths.grok_config).unwrap();
        assert!(
            !text2.contains("mcp_servers"),
            "mcp entry stripped: {text2}"
        );
    }

    // ── reconcile (the engine's boot-time / config-change convergence) ──────────

    /// Recursively count `.bak.<secs>` sibling files under `dir` — the backups the writers
    /// leave before overwriting an existing config. Used to prove a steady-state reconcile
    /// creates NONE.
    fn count_bak_files(dir: &std::path::Path) -> usize {
        let mut n = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    n += count_bak_files(&p);
                } else if p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.contains(".bak."))
                {
                    n += 1;
                }
            }
        }
        n
    }

    /// Create every client's presence-gate dot-dir so a real (non-skip) wire happens for each.
    fn make_all_client_dirs(paths: &Paths) {
        for d in [
            &paths.claude_dir,
            &paths.codex_dir,
            &paths.qwen_dir,
            &paths.grok_dir,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
    }

    /// Presence-gating holds under `reconcile`: with NO config.toml (desired = all supported)
    /// but none of the gated clients installed, each gated client is a clean skip that scatters
    /// nothing. (Claude Code is ungated — the installers wire it unconditionally — so its files
    /// ARE written; only the gated clients must stay untouched.)
    #[test]
    fn reconcile_skips_absent_gated_clients_without_scattering() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(reconcile(&paths), 0);
        assert!(
            !paths.codex_config.exists(),
            "codex not installed → skipped"
        );
        assert!(
            !paths.qwen_settings.exists(),
            "qwen not installed → skipped"
        );
        assert!(!paths.grok_config.exists(), "grok not installed → skipped");
    }

    /// Absent / empty `exclude_clients` ⇒ exclude nothing ⇒ every client wired: with all presence
    /// gates satisfied, reconcile wires each one's surfaces.
    #[test]
    fn reconcile_absent_key_wires_all_supported_clients() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        make_all_client_dirs(&paths);

        assert_eq!(reconcile(&paths), 0);
        assert!(paths.settings_json.exists(), "claude_code hooks wired");
        assert!(paths.claude_code_config.exists(), "claude_code mcp wired");
        assert!(paths.codex_config.exists(), "codex wired");
        assert!(paths.qwen_settings.exists(), "qwen wired");
        assert!(paths.grok_config.exists(), "grok wired");
    }

    /// Listing a previously-wired client in `exclude_clients` strips that client's surfaces:
    /// wire Qwen, then reconcile with `exclude_clients = ["qwen_code"]` — Qwen's hooks + MCP
    /// entry are removed from `~/.qwen/settings.json`.
    #[test]
    fn reconcile_strips_a_client_dropped_from_the_desired_set() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.qwen_dir).unwrap();

        // Pre-wire Qwen (as a prior reconcile / installer would have).
        assert_eq!(wire_client(WireTarget::QwenCode, &paths, false, false), 0);
        let before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(
            before["hooks"]["Stop"].as_array().is_some(),
            "qwen wired first"
        );

        // Exclude Qwen (the wired client), then reconcile → it must be stripped.
        let cfg = ds_config::VoiceConfig {
            exclude_clients: Some(vec![WireTarget::QwenCode]),
            ..ds_config::VoiceConfig::default()
        };
        ds_config::write_settings(&paths, &cfg).unwrap();
        assert_eq!(reconcile(&paths), 0);

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(after.get("hooks").is_none(), "qwen hooks stripped: {after}");
        assert!(
            after.get("mcpServers").is_none(),
            "qwen mcp entry stripped: {after}"
        );
    }

    /// Steady-state idempotency (LOAD-BEARING — the engine reconciles every boot): after a
    /// first reconcile wires everything, a SECOND reconcile writes nothing and creates NO new
    /// `.bak` sibling across the three newly-guarded writers (JSON hooks, JSON MCP, TOML MCP).
    #[test]
    fn reconcile_is_idempotent_and_creates_no_new_backups() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        make_all_client_dirs(&paths);

        assert_eq!(reconcile(&paths), 0);
        // Fresh configs were CREATED (not overwritten), so the first pass leaves no backup.
        let baks_after_first = count_bak_files(dir.path());

        // Second pass: every writer sees an unchanged document → short-circuits before any
        // backup+write. Without the idempotency guards each existing file would be rewritten
        // and a `.bak` sibling dropped.
        assert_eq!(reconcile(&paths), 0);
        let baks_after_second = count_bak_files(dir.path());
        assert_eq!(
            baks_after_second, baks_after_first,
            "a steady-state reconcile must create NO new .bak files"
        );
    }
}
