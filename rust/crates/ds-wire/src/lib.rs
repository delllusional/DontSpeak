//! Client-wiring orchestrator — CLI [`run`] and engine boot/config [`reconcile`].
//! Same path over `CLIENT_REGISTRY` (no drift). Additive, idempotent, backed-up.
//!
//! `--print-only` (#30): co-file surfaces share one disk read via `PreviewDoc`.

pub(crate) mod hooks;
mod io;
pub(crate) mod mcp;

use std::path::{Path, PathBuf};

use ds_config::{ClientKind, ClientSource, ClientSpec, Paths, Surface, WireMechanism};

/// Merged surface doc for print-only threading (JSON, format-preserving TOML, or YAML text).
#[derive(Debug)]
pub(crate) enum PreviewDoc {
    Json(serde_json::Value),
    Toml(String),
    Yaml(String),
}

/// MCP registry key / `serverInfo.name`. Must match `dontspeak::mcp::SERVER_NAME`.
pub const SERVER_NAME: &str = "DontSpeak";

/// CLI entry. Exit 0 ok/skip, 1 hard error. Wire-able tokens only.
pub fn run(args: &[String]) -> i32 {
    let mut client: Option<ClientSource> = None;
    let mut remove = false;
    let mut print_only = false;
    let mut all = false;
    let mut do_reconcile = false;
    // Registry-driven usage tokens.
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
            other => match ClientSource::parse(other) {
                Some(t) if ds_config::client_spec(t).is_some() => {
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
    if !all && !do_reconcile && client.is_none() {
        eprintln!("wire: missing client ({}), or use --all", tokens());
        return 1;
    }
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire: $HOME not set; nothing to do");
        return 1;
    };

    // Interactive wire only (engine reconcile never seeds/prunes).
    if !remove && !print_only {
        hooks::seed_and_prune(&paths);
    }

    // Before `--all` so `--reconcile` is not also an unconditional wire.
    if do_reconcile {
        return reconcile(&paths);
    }

    if all {
        return ClientSource::CLIENTS
            .iter()
            .map(|&c| wire_client(c, &paths, remove, print_only))
            .max()
            .unwrap_or(0);
    }

    wire_client(client.expect("checked above"), &paths, remove, print_only)
}

/// Converge every client to `exclude_clients` (absent/empty ⇒ wire all). No seed/prune.
/// Engine boot + `wire --reconcile`. Worst exit code wins.
pub fn reconcile(paths: &Paths) -> i32 {
    let excluded = ds_config::VoiceConfig::load(paths).excluded_clients();
    ClientSource::CLIENTS
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

/// All surfaces run even if one fails (worst code wins) so `--remove` cannot leave dangling MCP.
/// Order matters for `claude_code` (hooks create `~/.claude` that MCP presence then sees).
fn wire_client(client: ClientSource, paths: &Paths, remove: bool, print_only: bool) -> i32 {
    let spec = ds_config::client_spec(client).expect(
        "wire_client only called with CLIENTS members (run gates on client_spec; \
         --all / reconcile iterate CLIENTS)",
    );

    if !print_only && spec.gate_on_presence {
        if remove {
            // No config files ⇒ nothing to strip; don't scatter on remove.
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

    let code = if print_only {
        // Issue #30 — real writes always pass seed/capture = None.
        wire_surfaces_print_only(spec, paths, remove)
            .into_iter()
            .map(|(_file, code, _doc)| code)
            .max()
            .unwrap_or(0)
    } else {
        spec.surfaces
            .iter()
            .map(|s| dispatch_surface(s, spec, paths, remove, false, None, None))
            .max()
            .unwrap_or(0)
    };
    if client == ClientSource::Codex && !remove && !print_only && code == 0 {
        eprintln!("wire: launch interactive Codex with `dontspeak codex` for mid-turn narration");
    }
    code
}

/// `seed`/`capture` are print-only only; always `None` on real write. Grok ignores both.
#[allow(clippy::too_many_arguments)] // 5 mechanisms + seed/capture
fn dispatch_surface(
    s: &'static Surface,
    spec: &'static ClientSpec,
    paths: &Paths,
    remove: bool,
    print_only: bool,
    seed: Option<PreviewDoc>,
    capture: Option<&mut Option<PreviewDoc>>,
) -> i32 {
    match s.mechanism {
        // Writers take `spec.target` as `--client` — none hardcodes its client.
        WireMechanism::ClaudeJsonHooks => hooks::claude_json_hooks(
            (s.config_file)(paths),
            s.hook_streaming,
            s.hook_command_style,
            spec.target,
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
        WireMechanism::ClaudeTomlHooks => hooks::claude_toml_hooks(
            (s.config_file)(paths),
            spec.target,
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
        WireMechanism::KimiTomlHooks => hooks::kimi_toml_hooks(
            (s.config_file)(paths),
            spec.target,
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
        WireMechanism::HermesYamlHooks => hooks::hermes_yaml_hooks(
            (s.config_file)(paths),
            spec.target,
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
        WireMechanism::HermesYamlMcp => hooks::hermes_yaml_mcp(
            (s.config_file)(paths),
            remove,
            print_only,
            paths,
            seed,
            capture,
            s.load_hint
                .unwrap_or("start a new Hermes session to load the server"),
        ),
        WireMechanism::HermesShellAllowlist => hooks::hermes_shell_allowlist(
            (s.config_file)(paths),
            spec.target,
            remove,
            print_only,
            paths,
        ),
        WireMechanism::GrokJsonHooks => {
            hooks::grok_json_hooks((s.config_file)(paths), remove, print_only, paths)
        }
        WireMechanism::JsonMcp => mcp::apply(
            &mcp::target_for(spec, s, paths),
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
        WireMechanism::TomlMcp => mcp::apply_toml(
            &mcp::target_for(spec, s, paths),
            remove,
            print_only,
            paths,
            seed,
            capture,
        ),
    }
}

/// Print-only: group by config file, thread merged `PreviewDoc`s (issue #30).
/// Grok never participates (`doc` is `None`; it prints itself).
pub(crate) fn wire_surfaces_print_only(
    spec: &'static ClientSpec,
    paths: &Paths,
    remove: bool,
) -> Vec<(PathBuf, i32, Option<PreviewDoc>)> {
    let mut groups: Vec<(PathBuf, Vec<&'static Surface>)> = Vec::new();
    for s in spec.surfaces {
        let file = (s.config_file)(paths).to_path_buf();
        match groups.iter_mut().find(|(f, _)| f == &file) {
            Some(group) => group.1.push(s),
            None => groups.push((file, vec![s])),
        }
    }

    groups
        .into_iter()
        .map(|(file, surfaces)| {
            let n = surfaces.len();
            let mut carried: Option<PreviewDoc> = None;
            let mut worst = 0;
            let mut final_doc: Option<PreviewDoc> = None;
            for (i, s) in surfaces.into_iter().enumerate() {
                let mut slot: Option<PreviewDoc> = None;
                worst = worst.max(dispatch_surface(
                    s,
                    spec,
                    paths,
                    remove,
                    true,
                    carried.take(),
                    Some(&mut slot),
                ));
                if i + 1 == n {
                    if let Some(doc) = &slot {
                        worst = worst.max(print_captured_doc(s.mechanism, &file, doc));
                    }
                    final_doc = slot;
                } else {
                    carried = slot;
                }
            }
            (file, worst, final_doc)
        })
        .collect()
}

/// Print captured preview as the writer would (`// {path}\n{body}`, issue #33).
fn print_captured_doc(mechanism: WireMechanism, cfg: &Path, doc: &PreviewDoc) -> i32 {
    match (mechanism, doc) {
        (WireMechanism::ClaudeJsonHooks | WireMechanism::JsonMcp, PreviewDoc::Json(v)) => {
            match serde_json::to_string_pretty(v) {
                Ok(s) => {
                    println!("// {}\n{s}", cfg.display());
                    0
                }
                Err(e) => {
                    eprintln!("wire: serialize failed: {e}");
                    1
                }
            }
        }
        (
            WireMechanism::ClaudeTomlHooks | WireMechanism::TomlMcp | WireMechanism::KimiTomlHooks,
            PreviewDoc::Toml(s),
        ) => {
            println!("// {}\n{s}", cfg.display());
            0
        }
        (WireMechanism::HermesYamlHooks | WireMechanism::HermesYamlMcp, PreviewDoc::Yaml(s)) => {
            println!("// {}\n{s}", cfg.display());
            0
        }
        (WireMechanism::GrokJsonHooks | WireMechanism::HermesShellAllowlist, _) => {
            unreachable!("this mechanism never populates a capture slot")
        }
        (mechanism, doc) => unreachable!(
            "PreviewDoc variant {doc:?} does not match its surface's mechanism {mechanism:?}"
        ),
    }
}

/// `wire --list` dump.
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
                    WireMechanism::KimiTomlHooks => "voice hooks (Kimi flat [[hooks]], TOML)",
                    WireMechanism::HermesYamlHooks => {
                        "voice hooks (Hermes nested hooks.<event>, YAML)"
                    }
                    WireMechanism::HermesYamlMcp => "MCP server (stdio, mcp_servers table in YAML)",
                    WireMechanism::HermesShellAllowlist => {
                        "shell-hook allowlist (Hermes consent JSON)"
                    }
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

    /// Guard hits before `Paths::resolve()` (no $HOME I/O).
    /// `dontspeak` parses as ClientSource but has no client_spec → "unknown client".
    #[test]
    fn missing_or_invalid_client_selection_is_a_hard_error() {
        for argv in [
            &[][..],
            &["not_a_real_client"][..],
            &["dontspeak"][..],
            &["unknown"][..],
            &["codex", "claude_code"][..],
        ] {
            assert_eq!(run(&args(argv)), 1, "{argv:?}");
        }
    }

    #[test]
    fn help_flag_exits_zero() {
        assert_eq!(run(&args(&["-h"])), 0);
        assert_eq!(run(&args(&["--help"])), 0);
    }

    /// Injectable `Paths` (tempdir; no real $HOME).
    #[test]
    fn list_flag_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        print_registry(Some(&paths));
    }

    /// Unknown flag is soft; still falls through to missing-client (exit 1), not $HOME I/O.
    #[test]
    fn unknown_flag_without_a_client_is_tolerated_not_a_hard_failure() {
        assert_eq!(run(&args(&["--not-a-real-flag"])), 1);
    }

    #[test]
    fn wire_client_skips_absent_gated_client() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(wire_client(ClientSource::Codex, &paths, false, false), 0);
        assert!(!paths.codex_dir.exists());
    }

    #[test]
    fn wire_client_remove_with_no_existing_config_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert_eq!(wire_client(ClientSource::Codex, &paths, true, false), 0);
        assert!(!paths.codex_config.exists());
    }

    /// Qwen: hooks + MCP share one file — wire and remove both without clobber (tempdir Paths).
    #[test]
    fn wire_client_qwen_code_wires_hooks_and_mcp_into_one_file_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.qwen_dir).unwrap(); // satisfy Qwen's presence gate

        assert_eq!(wire_client(ClientSource::QwenCode, &paths, false, false), 0);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(v["hooks"]["Stop"].as_array().is_some(), "hooks wired");
        assert!(
            v["hooks"]["MessageDisplay"].as_array().is_some(),
            "streaming hook wired"
        );
        // InlineShell: verb in `command`, no `args` (Qwen would drop args → dead hooks).
        let stop = &v["hooks"]["Stop"][0]["hooks"][0];
        assert!(
            stop["command"].as_str().unwrap().contains(" notify"),
            "hook command carries the inlined verb"
        );
        assert!(stop.get("args").is_none(), "no `args` key for Qwen hooks");
        assert!(
            v["mcpServers"]["DontSpeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak"),
            "mcp entry wired alongside hooks in the same file"
        );

        assert_eq!(wire_client(ClientSource::QwenCode, &paths, true, false), 0);
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(v2.get("hooks").is_none(), "hooks stripped");
        assert!(v2.get("mcpServers").is_none(), "mcp entry stripped");
    }

    /// Codex TOML: hooks + MCP share one file (analog of Qwen JSON test).
    #[test]
    fn wire_client_codex_wires_hooks_and_mcp_into_one_file_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();

        assert_eq!(wire_client(ClientSource::Codex, &paths, false, false), 0);
        let text = std::fs::read_to_string(&paths.codex_config).unwrap();
        assert!(text.contains("[[hooks.Stop]]"), "hooks wired: {text}");
        assert!(
            text.contains("[mcp_servers.DontSpeak]"),
            "mcp entry wired alongside hooks in the same file: {text}"
        );
        assert!(
            text.contains("command ="),
            "mcp entry carries a command: {text}"
        );

        assert_eq!(wire_client(ClientSource::Codex, &paths, true, false), 0);
        let text2 = std::fs::read_to_string(&paths.codex_config).unwrap();
        assert!(!text2.contains("hooks"), "hooks stripped: {text2}");
        assert!(
            !text2.contains("mcp_servers"),
            "mcp entry stripped: {text2}"
        );
    }

    /// Issue #30: print-only must show hooks+MCP union, not a stale second-surface disk read.
    #[test]
    fn wire_surfaces_print_only_codex_shows_the_union_of_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap(); // satisfy Codex's presence gate

        let spec = ds_config::client_spec(ClientSource::Codex).unwrap();
        let results = wire_surfaces_print_only(spec, &paths, false);
        assert_eq!(
            results.len(),
            1,
            "Codex's two surfaces share one file: {results:?}"
        );
        let (file, code, doc) = &results[0];
        assert_eq!(*file, paths.codex_config);
        assert_eq!(*code, 0);
        let Some(PreviewDoc::Toml(text)) = doc else {
            panic!("expected a captured PreviewDoc::Toml, got {doc:?}");
        };
        assert!(
            text.contains("[[hooks.Stop]]"),
            "hooks block present: {text}"
        );
        assert!(
            text.contains("[mcp_servers.DontSpeak]"),
            "mcp block present alongside hooks: {text}"
        );
        assert!(!paths.codex_config.exists(), "print-only never writes");

        assert_eq!(wire_client(ClientSource::Codex, &paths, false, true), 0);
        assert!(
            !paths.codex_config.exists(),
            "print-only via wire_client still never writes"
        );
    }

    /// Qwen JSON analog of the Codex print-only union test.
    #[test]
    fn wire_surfaces_print_only_qwen_shows_the_union_of_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.qwen_dir).unwrap(); // satisfy Qwen's presence gate

        let spec = ds_config::client_spec(ClientSource::QwenCode).unwrap();
        let results = wire_surfaces_print_only(spec, &paths, false);
        assert_eq!(
            results.len(),
            1,
            "Qwen's two surfaces share one file: {results:?}"
        );
        let (file, code, doc) = &results[0];
        assert_eq!(*file, paths.qwen_settings);
        assert_eq!(*code, 0);
        let Some(PreviewDoc::Json(v)) = doc else {
            panic!("expected a captured PreviewDoc::Json, got {doc:?}");
        };
        assert!(
            v["hooks"]["Stop"].as_array().is_some(),
            "hooks present: {v}"
        );
        assert!(
            v["mcpServers"]["DontSpeak"]["command"].as_str().is_some(),
            "mcp entry present alongside hooks: {v}"
        );
        assert!(!paths.qwen_settings.exists(), "print-only never writes");
    }

    /// Grok: hooks file + MCP TOML are separate files; wire creates both, remove deletes/strips.
    #[test]
    fn wire_client_grok_wires_both_surfaces_then_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.grok_dir).unwrap();

        assert_eq!(wire_client(ClientSource::Grok, &paths, false, false), 0);
        let text = std::fs::read_to_string(&paths.grok_config).unwrap();
        assert!(
            text.contains("[mcp_servers.DontSpeak]"),
            "mcp entry wired into grok config: {text}"
        );
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

        assert_eq!(wire_client(ClientSource::Grok, &paths, true, false), 0);
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

    /// Kimi Code: hooks (flat `[[hooks]]` TOML) and MCP (Claude-shape JSON) are separate
    /// files; wire creates both, remove strips both, re-wire is a byte-identical no-op.
    #[test]
    fn wire_client_kimi_code_wires_both_surfaces_then_removes_both_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.kimi_dir).unwrap();

        assert_eq!(wire_client(ClientSource::KimiCode, &paths, false, false), 0);

        // Hooks: five events — six entries, UserPromptSubmit carries notify AND provide.
        let text = std::fs::read_to_string(&paths.kimi_config_toml).unwrap();
        assert_eq!(
            text.matches("[[hooks]]").count(),
            6,
            "six flat entries: {text}"
        );
        for event in ["SessionStart", "SessionEnd", "Stop", "Notification"] {
            assert!(
                text.contains(&format!("event = \"{event}\"")),
                "{event} wired: {text}"
            );
        }
        assert_eq!(
            text.matches("event = \"UserPromptSubmit\"").count(),
            2,
            "UserPromptSubmit is two entries (notify + provide): {text}"
        );
        // InlineShell with the uniform client tail; seconds timeouts; greet-only SessionStart.
        assert!(
            text.contains(" notify --greet-only --client kimi_code"),
            "SessionStart greet-only: {text}"
        );
        assert!(
            text.contains(" provide --client kimi_code"),
            "provide wired: {text}"
        );
        assert!(text.contains("timeout = 600"), "Stop timeout: {text}");
        // Kimi rejects timeouts above 600 (kimi doctor: "expected number to be <=600").
        assert!(!text.contains("timeout = 1800"), "over-cap timeout: {text}");
        // THE HARD CONSTRAINT: nothing but event/command/timeout may appear in an entry —
        // any extra key makes Kimi reject the whole config.
        for forbidden in ["matcher", "async", "args", "shell"] {
            assert!(
                !text.contains(forbidden),
                "Kimi forbids `{forbidden}` in a [[hooks]] entry: {text}"
            );
        }

        // MCP: the Claude-shape mcpServers entry in the SEPARATE mcp.json.
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.kimi_mcp_json).unwrap()).unwrap();
        assert!(
            mcp["mcpServers"]["DontSpeak"]["command"]
                .as_str()
                .unwrap()
                .contains("dontspeak"),
            "mcp entry wired: {mcp}"
        );

        // Re-wire: byte-identical, no new .bak (steady-state reconcile invariant).
        let hooks_before = std::fs::read(&paths.kimi_config_toml).unwrap();
        let mcp_before = std::fs::read(&paths.kimi_mcp_json).unwrap();
        let baks_before = count_bak_files(dir.path());
        assert_eq!(wire_client(ClientSource::KimiCode, &paths, false, false), 0);
        assert_eq!(
            std::fs::read(&paths.kimi_config_toml).unwrap(),
            hooks_before
        );
        assert_eq!(std::fs::read(&paths.kimi_mcp_json).unwrap(), mcp_before);
        assert_eq!(
            count_bak_files(dir.path()),
            baks_before,
            "an unchanged re-wire must create NO new .bak files"
        );

        // Remove: both surfaces stripped cleanly.
        assert_eq!(wire_client(ClientSource::KimiCode, &paths, true, false), 0);
        let text2 = std::fs::read_to_string(&paths.kimi_config_toml).unwrap();
        assert!(!text2.contains("dontspeak"), "hooks stripped: {text2}");
        let mcp2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.kimi_mcp_json).unwrap()).unwrap();
        assert!(
            mcp2.get("mcpServers").is_none(),
            "mcp entry stripped: {mcp2}"
        );
    }

    /// Hermes: hooks + MCP share config.yaml; allowlist is a separate JSON consent file.
    #[test]
    fn wire_client_hermes_wires_all_surfaces_then_removes_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.hermes_dir).unwrap();

        assert_eq!(wire_client(ClientSource::Hermes, &paths, false, false), 0);

        let text = std::fs::read_to_string(&paths.hermes_config_yaml).unwrap();
        // serde-saphyr may fold long Windows paths with `>-` (newlines in the scalar).
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for event in [
            "on_session_start",
            "pre_llm_call",
            "post_llm_call",
            "on_session_finalize",
        ] {
            assert!(text.contains(event), "{event} wired: {text}");
        }
        assert!(
            flat.contains("notify --greet-only --client hermes"),
            "SessionStart greet-only: {text}"
        );
        assert!(
            flat.contains("provide --client hermes"),
            "provide wired: {text}"
        );
        assert!(
            text.contains("mcp_servers") && text.contains("DontSpeak"),
            "mcp entry wired alongside hooks: {text}"
        );

        let allow: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&paths.hermes_shell_hooks_allowlist).unwrap(),
        )
        .unwrap();
        assert_eq!(
            allow["approvals"].as_array().unwrap().len(),
            5,
            "five (event, command) approvals: {allow}"
        );

        // Re-wire: byte-identical, no new .bak.
        let cfg_before = std::fs::read(&paths.hermes_config_yaml).unwrap();
        let allow_before = std::fs::read(&paths.hermes_shell_hooks_allowlist).unwrap();
        let baks_before = count_bak_files(dir.path());
        assert_eq!(wire_client(ClientSource::Hermes, &paths, false, false), 0);
        assert_eq!(
            std::fs::read(&paths.hermes_config_yaml).unwrap(),
            cfg_before
        );
        assert_eq!(
            std::fs::read(&paths.hermes_shell_hooks_allowlist).unwrap(),
            allow_before
        );
        assert_eq!(
            count_bak_files(dir.path()),
            baks_before,
            "unchanged re-wire creates no .bak"
        );

        assert_eq!(wire_client(ClientSource::Hermes, &paths, true, false), 0);
        let text2 = std::fs::read_to_string(&paths.hermes_config_yaml).unwrap();
        assert!(!text2.contains("dontspeak"), "hooks/mcp stripped: {text2}");
        let allow2: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&paths.hermes_shell_hooks_allowlist).unwrap(),
        )
        .unwrap();
        assert!(
            allow2.get("approvals").is_none(),
            "allowlist stripped: {allow2}"
        );
    }

    /// Issue #30: print-only must show hooks+MCP union for Hermes config.yaml.
    #[test]
    fn wire_surfaces_print_only_hermes_shows_hooks_and_mcp_union() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.hermes_dir).unwrap();

        let spec = ds_config::client_spec(ClientSource::Hermes).unwrap();
        let results = wire_surfaces_print_only(spec, &paths, false);
        // config.yaml (hooks+mcp) + allowlist file = 2 groups.
        assert_eq!(
            results.len(),
            2,
            "Hermes surfaces group by file: {results:?}"
        );
        let yaml_group = results
            .iter()
            .find(|(f, _, _)| f == &paths.hermes_config_yaml)
            .expect("config.yaml group");
        assert_eq!(yaml_group.1, 0);
        let Some(PreviewDoc::Yaml(text)) = &yaml_group.2 else {
            panic!("expected PreviewDoc::Yaml, got {:?}", yaml_group.2);
        };
        assert!(text.contains("post_llm_call"), "hooks present: {text}");
        assert!(text.contains("mcp_servers"), "mcp present: {text}");
        assert!(
            !paths.hermes_config_yaml.exists(),
            "print-only never writes"
        );
    }

    /// Count `.bak.` siblings (prove steady-state reconcile creates none).
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

    fn make_all_client_dirs(paths: &Paths) {
        for d in [
            &paths.claude_dir,
            &paths.codex_dir,
            &paths.qwen_dir,
            &paths.grok_dir,
            &paths.kimi_dir,
            &paths.hermes_dir,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
    }

    /// Gated clients absent ⇒ skip without scattering (Claude Code is ungated and may write).
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
        assert!(
            !paths.kimi_config_toml.exists(),
            "kimi not installed → skipped"
        );
        assert!(
            !paths.kimi_mcp_json.exists(),
            "kimi not installed → skipped"
        );
        assert!(
            !paths.hermes_config_yaml.exists(),
            "hermes not installed → skipped"
        );
        assert!(
            !paths.hermes_shell_hooks_allowlist.exists(),
            "hermes not installed → skipped"
        );
    }

    /// No `exclude_clients` ⇒ wire all present clients.
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
        assert!(paths.kimi_config_toml.exists(), "kimi hooks wired");
        assert!(paths.kimi_mcp_json.exists(), "kimi mcp wired");
        assert!(paths.hermes_config_yaml.exists(), "hermes config wired");
        assert!(
            paths.hermes_shell_hooks_allowlist.exists(),
            "hermes allowlist wired"
        );
    }

    /// `exclude_clients` strips a previously-wired client.
    #[test]
    fn reconcile_strips_a_client_dropped_from_the_desired_set() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.qwen_dir).unwrap();

        // Pre-wire Qwen (as a prior reconcile / installer would have).
        assert_eq!(wire_client(ClientSource::QwenCode, &paths, false, false), 0);
        let before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.qwen_settings).unwrap()).unwrap();
        assert!(
            before["hooks"]["Stop"].as_array().is_some(),
            "qwen wired first"
        );

        // Exclude Qwen (the wired client), then reconcile → it must be stripped.
        let cfg = ds_config::VoiceConfig {
            exclude_clients: Some(vec![ClientSource::QwenCode]),
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

    /// Load-bearing: second boot reconcile is zero-write (no new `.bak`) across writers.
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
