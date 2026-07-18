//! `dontspeak` — multi-call binary. No args: stdio MCP server (or Grok's bare-command hook when
//! `GROK_HOOK_EVENT` is set). With a subcommand: client launcher, hook executor, or installer.
//!
//! MCP is a thin bridge: newline-delimited JSON-RPC 2.0 on stdio (MCP 2025-11-25) and `ds-ipc`
//! Unix-socket protocol to the resident engine — same engine as hooks and the host app.
//!
//! Tool catalog lives in `ds_tools::catalog()`. `list_voices` is config-direct (no engine
//! round-trip); all config writes go through `set_config` / `config.toml` (engine hot-reloads
//! on mtime) — no per-session voice override.
//!
//! Transport: stdout is JSON-RPC only (one message per line); all logging to stderr. Each
//! request gets one response by id; notifications (no id) get none.
//!
//! ## Module layout
//! Front-door dispatch here; MCP core in [`mcp`]; handlers in [`tools`]; voice enum in
//! [`voices`]; engine spawn in [`engine_launch`]; `prompt-context` in [`hook_prompt`];
//! speak/narrate hooks in [`hook_speak`] / [`hook_narrate`] (former `ds-speak` / `ds-narrate`).
// Console-subsystem on Windows: hook/MCP inherit stdio; `dontspeak <client>` must block the
// shell with the console attached to the interactive child. A GUI-subsystem binary makes
// PowerShell return immediately and race the prompt against the TUI.
//
// Tradeoff: a console-subsystem child of a console-less GUI host briefly flashes a console
// unless detached. `main` calls `ds_platform::detach_console()` for every role except `Launch`.

mod client_launch;
mod engine_launch;
mod hook_core;
mod hook_narrate;
mod hook_prompt;
mod hook_speak;
mod mcp;
mod tools;
mod voices;

use ds_config::ClientSource;

/// Roles argv\[1\] can select. Extracted so dispatch is unit-testable without stdio/spawns.
#[derive(Debug, PartialEq, Eq)]
enum Subcommand<'a> {
    Notify,
    Provide,
    /// Args after `wire` (argv\[2..\]).
    Wire(&'a [String]),
    /// Registry client + trailing args.
    Launch(ClientSource, &'a [String]),
    /// `-V` / `--version` / `version`.
    Version,
    /// `-h` / `--help` / `help`.
    Help,
    /// `status` (points at MCP `get_status`; no engine call).
    Status,
    /// No argv\[1\]: stdio MCP server (or Grok bare hook).
    Server,
    Unknown(String),
}

/// `--client <token>` from wiring (`ds_config::wire::cmdline`). Rides at argv\[2+\], so
/// `resolve_subcommand` (argv\[1\] only) is undisturbed.
///
/// Unrecognised / missing / non-client (`dontspeak`, `unknown`) ⇒ [`ClientSource::Unknown`] —
/// never a hard error: hooks must degrade, never fail the client's turn. Not a legacy path
/// (every wired hook carries the token; engine re-wires at boot); the honest answer when
/// invoked by hand or by something we don't recognise.
fn client_from_argv(argv: &[String]) -> ClientSource {
    argv.iter()
        .position(|a| a == "--client")
        .and_then(|i| argv.get(i + 1))
        .and_then(|t| ClientSource::parse(t))
        .filter(|c| c.is_client())
        .unwrap_or(ClientSource::Unknown)
}

/// Pure argv\[1\] dispatch — no I/O, no `process::exit`.
fn resolve_subcommand(argv: &[String]) -> Subcommand<'_> {
    match argv.get(1).map(String::as_str) {
        Some("notify") => Subcommand::Notify,
        Some("provide") => Subcommand::Provide,
        Some("wire") => Subcommand::Wire(&argv[2..]),
        // Host/shell probes — exit 0, never ERROR as unknown.
        Some("-V" | "--version" | "version") => Subcommand::Version,
        Some("-h" | "--help" | "help") => Subcommand::Help,
        Some("status") => Subcommand::Status,
        Some(name) if ds_config::client_spec_for_launch(name).is_some() => Subcommand::Launch(
            ds_config::client_spec_for_launch(name)
                .expect("the guarded registry lookup must still resolve")
                .target,
            &argv[2..],
        ),
        Some(other) => Subcommand::Unknown(other.to_string()),
        None => Subcommand::Server,
    }
}

const USAGE: &str = "\
dontspeak — local voice layer (MCP + hooks + client launchers)

Usage:
  dontspeak                 stdio MCP server (or Grok hook when GROK_HOOK_EVENT is set)
  dontspeak claude [args…]  launch Claude Code (starts host if needed)
  dontspeak codex  [args…]  launch Codex
  dontspeak qwen   [args…]  launch Qwen Code
  dontspeak grok   [args…]  launch Grok
  dontspeak kimi   [args…]  launch Kimi Code
  dontspeak wire   [args…]  wire/unwire client hooks + MCP
  dontspeak notify          command-hook executor (stdin JSON)
  dontspeak provide         query-hook executor (stdin JSON)
  dontspeak --version       print package version
  dontspeak --help          this help
  dontspeak status          how to query runtime status (MCP get_status)

Engine via local socket; speech config is OS data-dir config.toml (not client settings).
";

/// Expected-subcommands fragment of the unknown-subcommand hint. Separate const so the
/// registry-drift test can assert every launcher command is listed (same for `USAGE`).
const EXPECTED_SUBCOMMANDS: &str = "`claude`, `codex`, `qwen`, `grok`, `kimi`, `notify`, \
     `provide`, `wire`, `--version`, or `--help`";

/// Detach every role except `Launch` (only Launch needs the console for the interactive child).
fn should_detach_console(subcommand: &Subcommand) -> bool {
    !matches!(subcommand, Subcommand::Launch(..))
}

/// Grok injects this reserved env var into every hook process — discriminator between a
/// bare-command hook and bare-command MCP. Value is a marker only; payload is the routing
/// source of truth. Pure so tests never mutate process-wide env in parallel.
fn is_grok_hook_launch(marker: Option<&std::ffi::OsStr>) -> bool {
    marker.is_some_and(|value| !value.is_empty())
}

/// Grok's compatibility adapter drops Claude Code's `args` and deduplicates handlers by bare
/// command target, so one no-arg process must do both the notify side effect and provide
/// stdout. Native Grok hooks ignore stdout; imported Claude-hook compat may still consume it.
/// `greet_only=true` only for SessionStart (no MessageDisplay witness seed).
///
/// Because Grok ignores provide stdout, also refresh managed `~/.grok/AGENTS.md` narrate
/// section on every hook (issue #95) so digests reach the model next session start.
fn run_grok_hook() {
    let payload = read_stdin();
    let event = hook_core::event_name(&payload);
    hook_core::notify(&event, &payload, true, ClientSource::Grok);
    if let Some(out) = hook_core::provide(&event, &payload) {
        println!("{out}");
    }
    // Best-effort: keep global Grok rules aligned with the live `narrate` set.
    if let Some(paths) = ds_config::Paths::resolve()
        && let Err(e) = ds_config::sync_grok_narrate_from_config(&paths)
    {
        eprintln!(
            "dontspeak: WARNING: could not sync Grok narrate digests in {} ({e})",
            paths.grok_agents_md.display()
        );
    }
}

fn main() {
    // Subcommands: `notify` (command hooks), `provide` (query hooks), `wire`,
    // `dontspeak <client>` (launch). No args → MCP unless GROK_HOOK_EVENT (see module docs).
    ds_log::init();
    let argv: Vec<String> = std::env::args().collect();
    let subcommand = resolve_subcommand(&argv);
    // No-op on non-Windows; see file-level console comment.
    if should_detach_console(&subcommand) {
        ds_platform::detach_console();
    }
    match subcommand {
        Subcommand::Notify => {
            let payload = read_stdin();
            // `--greet-only`: SessionStart for non-streaming clients — greet but skip the
            // streaming-witness seed (would silence Stop on clients with no MessageDisplay).
            let greet_only = argv.iter().any(|a| a == "--greet-only");
            hook_core::notify(
                &hook_core::event_name(&payload),
                &payload,
                greet_only,
                client_from_argv(&argv),
            );
            std::process::exit(0);
        }
        Subcommand::Provide => {
            let payload = read_stdin();
            if let Some(out) = hook_core::provide(&hook_core::event_name(&payload), &payload) {
                println!("{out}");
            }
            std::process::exit(0);
        }
        Subcommand::Wire(args) => {
            std::process::exit(ds_wire::run(args));
        }
        Subcommand::Launch(client, args) => {
            let spec = ds_config::client_spec(client)
                .expect("every launchable client is a registry client");
            std::process::exit(client_launch::run(spec, args));
        }
        Subcommand::Version => {
            println!("dontspeak {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Subcommand::Help => {
            print!("{USAGE}");
            std::process::exit(0);
        }
        Subcommand::Status => {
            println!(
                "dontspeak {}: runtime status is via MCP tool get_status (or the host app UI)",
                env!("CARGO_PKG_VERSION")
            );
            std::process::exit(0);
        }
        // Unrecognized argv\[1\] must not fall through to MCP (blocks on stdin forever —
        // typo or old binary handed a newer subcommand). MCP is no-argument only.
        Subcommand::Unknown(sub) => {
            let msg = format!(
                "dontspeak: unknown subcommand {sub:?}; expected {EXPECTED_SUBCOMMANDS} \
                 (run with no arguments for the stdio MCP server)"
            );
            eprintln!("{msg}");
            log::error!(target: "hook", "{msg}");
            std::process::exit(2);
        }
        // Bare executable: GROK_HOOK_EVENT distinguishes Grok hooks from MCP without
        // changing Claude's args-array hooks.
        Subcommand::Server => {
            if is_grok_hook_launch(std::env::var_os("GROK_HOOK_EVENT").as_deref()) {
                run_grok_hook();
            } else {
                mcp::serve();
            }
        }
    }
}

/// Whole stdin, single-shot. Empty on read error — unknown/empty event is a no-op.
fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn notify_token_resolves_to_notify() {
        let argv = argv(&["dontspeak", "notify"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Notify);
    }

    #[test]
    fn notify_with_greet_only_flag_still_resolves_to_notify() {
        // `--greet-only` rides at argv[2]; resolve matches argv[1] only.
        let argv = argv(&["dontspeak", "notify", "--greet-only"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Notify);
    }

    #[test]
    fn provide_token_resolves_to_provide() {
        let argv = argv(&["dontspeak", "provide"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Provide);
    }

    #[test]
    fn wire_token_resolves_to_wire_with_trailing_args() {
        let argv = argv(&["dontspeak", "wire", "claude_code", "--remove"]);
        assert_eq!(
            resolve_subcommand(&argv),
            Subcommand::Wire(&["claude_code".to_string(), "--remove".to_string()])
        );
    }

    #[test]
    fn wire_token_with_no_trailing_args_resolves_to_wire_with_empty_slice() {
        let argv = argv(&["dontspeak", "wire"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Wire(&[]));
    }

    #[test]
    fn every_registry_launcher_name_dispatches_with_trailing_args() {
        for spec in ds_config::CLIENT_REGISTRY {
            for name in
                std::iter::once(spec.launch.command).chain(spec.launch.aliases.iter().copied())
            {
                let argv = argv(&["dontspeak", name, "--version"]);
                assert_eq!(
                    resolve_subcommand(&argv),
                    Subcommand::Launch(spec.target, &argv[2..]),
                    "{name}"
                );
            }
        }
    }

    /// Drift gate: adding a registry client without updating the help text and the
    /// unknown-subcommand hint would leave the new launcher undiscoverable.
    #[test]
    fn usage_and_unknown_hint_list_every_registry_launcher() {
        for spec in ds_config::CLIENT_REGISTRY {
            let cmd = spec.launch.command;
            assert!(
                USAGE.contains(&format!("dontspeak {cmd} ")),
                "USAGE is missing the `{cmd}` launcher line"
            );
            assert!(
                EXPECTED_SUBCOMMANDS.contains(&format!("`{cmd}`")),
                "unknown-subcommand hint is missing `{cmd}`"
            );
        }
    }

    #[test]
    fn only_launch_keeps_the_console_attached() {
        assert!(!should_detach_console(&Subcommand::Launch(
            ClientSource::Codex,
            &[]
        )));
        for subcommand in [
            Subcommand::Notify,
            Subcommand::Provide,
            Subcommand::Wire(&[]),
            Subcommand::Version,
            Subcommand::Help,
            Subcommand::Status,
            Subcommand::Server,
            Subcommand::Unknown("bogus".to_string()),
        ] {
            assert!(
                should_detach_console(&subcommand),
                "{subcommand:?} must detach"
            );
        }
    }

    #[test]
    fn version_help_and_status_probes_resolve() {
        for (tok, want) in [
            ("-V", Subcommand::Version),
            ("--version", Subcommand::Version),
            ("version", Subcommand::Version),
            ("-h", Subcommand::Help),
            ("--help", Subcommand::Help),
            ("help", Subcommand::Help),
            ("status", Subcommand::Status),
        ] {
            assert_eq!(
                resolve_subcommand(&argv(&["dontspeak", tok])),
                want,
                "{tok}"
            );
        }
    }

    #[test]
    fn unrecognized_token_resolves_to_unknown() {
        let argv = argv(&["dontspeak", "bogus"]);
        assert_eq!(
            resolve_subcommand(&argv),
            Subcommand::Unknown("bogus".to_string())
        );
    }

    #[test]
    fn no_args_resolves_to_server() {
        let argv = argv(&["dontspeak"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Server);
    }

    #[test]
    fn truly_empty_argv_resolves_to_server() {
        // Even with no argv[0], argv.get(1) is still None.
        let argv: Vec<String> = Vec::new();
        assert_eq!(resolve_subcommand(&argv), Subcommand::Server);
    }

    #[test]
    fn grok_hook_marker_distinguishes_bare_hook_from_bare_mcp_server() {
        use std::ffi::OsStr;

        assert!(!is_grok_hook_launch(None));
        assert!(!is_grok_hook_launch(Some(OsStr::new(""))));
        assert!(is_grok_hook_launch(Some(OsStr::new("stop"))));
        assert!(is_grok_hook_launch(Some(OsStr::new("user_prompt_submit"))));
    }

    #[test]
    fn client_token_is_parsed_from_argv() {
        for (tok, want) in [
            ("claude_code", ClientSource::ClaudeCode),
            ("codex", ClientSource::Codex),
            ("qwen_code", ClientSource::QwenCode),
            ("grok", ClientSource::Grok),
            ("kimi_code", ClientSource::KimiCode),
        ] {
            let argv = argv(&["dontspeak", "notify", "--client", tok]);
            assert_eq!(client_from_argv(&argv), want, "{tok}");
        }
        assert_eq!(
            client_from_argv(&argv(&[
                "dontspeak",
                "notify",
                "--greet-only",
                "--client",
                "qwen_code"
            ])),
            ClientSource::QwenCode
        );
    }

    #[test]
    fn a_missing_malformed_or_non_client_token_degrades_to_unknown() {
        // Hooks degrade — never hard-error. `is_client()` keeps dontspeak/unknown out.
        for argv_ in [
            vec!["dontspeak", "notify"],
            vec!["dontspeak", "notify", "--client"],
            vec!["dontspeak", "notify", "--client", "gemini"],
            vec!["dontspeak", "notify", "--client", "dontspeak"],
            vec!["dontspeak", "notify", "--client", "unknown"],
            vec!["dontspeak", "notify", "--client", ""],
        ] {
            assert_eq!(
                client_from_argv(&argv(&argv_)),
                ClientSource::Unknown,
                "{argv_:?}"
            );
        }
    }

    #[test]
    fn client_flag_does_not_disturb_subcommand_dispatch() {
        let notify = argv(&["dontspeak", "notify", "--client", "codex"]);
        assert_eq!(resolve_subcommand(&notify), Subcommand::Notify);
        let provide = argv(&["dontspeak", "provide", "--client", "codex"]);
        assert_eq!(resolve_subcommand(&provide), Subcommand::Provide);
    }
}
