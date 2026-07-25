//! Multi-call binary. No args: stdio MCP (or Grok bare hook via `GROK_HOOK_EVENT`).
//! Subcommands: client launch, hooks, installer.
//!
//! MCP: NDJSON-RPC 2.0 on stdio + `ds-ipc`. Catalog: `ds_tools::catalog()`. Config via
//! `set_config` / mtime reload. Stdout = RPC only; log to stderr.
//!
//! Dispatch here; [`mcp`], [`tools`], [`hook_speak`] / [`hook_narrate`].
// Windows console-subsystem: hooks/MCP inherit stdio; Launch blocks shell with console.
// GUI-subsystem would race PowerShell prompt vs TUI. Detach every role except Launch
// (console flash under console-less GUI host).

mod client_launch;
mod engine_launch;
mod hook_core;
mod hook_narrate;
mod hook_prompt;
mod hook_speak;
mod mcp;
mod session_scope;
mod tools;
mod voices;

use ds_config::WiredAgent;

/// Pure argv\[1\] roles (unit-testable without stdio/spawns).
#[derive(Debug, PartialEq, Eq)]
enum Subcommand<'a> {
    Notify,
    Provide,
    Wire(&'a [String]),
    Launch(WiredAgent, &'a [String]),
    Version,
    Help,
    /// Runtime status guidance or plugin-facing JSON.
    Status(&'a [String]),
    /// No argv\[1\]: stdio MCP / Grok bare hook.
    Server,
    Unknown(String),
}

/// `--client` at argv\[2+\]. Unrecognised/missing hooks degrade to a no-op.
fn client_from_argv(argv: &[String]) -> Option<WiredAgent> {
    argv.iter()
        .position(|a| a == "--client")
        .and_then(|i| argv.get(i + 1))
        .and_then(|t| WiredAgent::parse(t))
}

fn resolve_subcommand(argv: &[String]) -> Subcommand<'_> {
    match argv.get(1).map(String::as_str) {
        Some("notify") => Subcommand::Notify,
        Some("provide") => Subcommand::Provide,
        Some("wire") => Subcommand::Wire(&argv[2..]),
        Some("-V" | "--version" | "version") => Subcommand::Version,
        Some("-h" | "--help" | "help") => Subcommand::Help,
        Some("status") => Subcommand::Status(&argv[2..]),
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

const USAGE_PREFIX: &str = "\
dontspeak — local voice layer (MCP + hooks + client launchers)

Usage:
  dontspeak                 stdio MCP server (or Grok hook when GROK_HOOK_EVENT is set)
";

const USAGE_SUFFIX: &str = "\
  dontspeak wire   [args…]  wire/unwire client hooks + MCP
  dontspeak notify          command-hook executor (stdin JSON)
  dontspeak provide         query-hook executor (stdin JSON)
  dontspeak --version       print package version
  dontspeak --help          this help
  dontspeak status [--json [--since N [--timeout-ms N]]]
                           runtime status guidance or machine-readable snapshot

Engine via local socket; speech config is OS data-dir config.toml (not client settings).
";

fn usage_text() -> String {
    use std::fmt::Write as _;

    let mut usage = USAGE_PREFIX.to_string();
    for spec in ds_config::CLIENT_REGISTRY {
        let _ = writeln!(
            usage,
            "  dontspeak {} [args…]  launch {}",
            spec.target.as_str(),
            spec.display_name
        );
    }
    usage.push_str(USAGE_SUFFIX);
    usage
}

fn expected_subcommands() -> String {
    let mut names = ds_config::CLIENT_REGISTRY
        .iter()
        .map(|spec| format!("`{}`", spec.target.as_str()))
        .collect::<Vec<_>>();
    names.extend(
        ["notify", "provide", "wire", "--version", "--help"].map(|name| format!("`{name}`")),
    );
    let last = names.pop().expect("fixed subcommands are nonempty");
    format!("{}, or {last}", names.join(", "))
}

/// Detach every role except `Launch` (only Launch needs the console for the interactive child).
fn should_detach_console(subcommand: &Subcommand) -> bool {
    !matches!(subcommand, Subcommand::Launch(..))
}

/// Grok injects `GROK_HOOK_EVENT` into every hook process — discriminator vs bare MCP.
/// Value is a marker only; payload is routing truth. Pure so tests never mutate process env.
fn is_grok_hook_launch(marker: Option<&std::ffi::OsStr>) -> bool {
    marker.is_some_and(|value| !value.is_empty())
}

/// Grok adapter drops `args` and dedupes by bare command, so one no-arg process does both
/// notify side effect and provide stdout. Native hooks ignore stdout; Claude-compat may not.
/// Also refresh AGENTS.md narrate on every hook (issue #95) — Grok ignores provide stdout.
fn run_grok_hook() {
    let payload = read_stdin();
    let event = hook_core::event_name(&payload);
    hook_core::notify(&event, &payload, true, WiredAgent::Grok);
    if let Some(out) = hook_core::provide(&event, &payload, WiredAgent::Grok) {
        println!("{out}");
    }
    if let Some(paths) = ds_config::Paths::resolve()
        && let Err(e) = ds_config::sync_grok_narrate_from_config(&paths)
    {
        log::warn!(
            target: "hook",
            "could not sync Grok narrate digests in {} ({e})",
            paths.grok_agents_md.display()
        );
    }
}

fn main() {
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
            // `--greet-only`: SessionStart for non-streaming — greet but skip witness seed
            // (would silence Stop when there is no MessageDisplay).
            let greet_only = argv.iter().any(|a| a == "--greet-only");
            if let Some(client) = client_from_argv(&argv) {
                hook_core::notify(
                    &hook_core::event_name(&payload),
                    &payload,
                    greet_only,
                    client,
                );
            }
            std::process::exit(0);
        }
        Subcommand::Provide => {
            let payload = read_stdin();
            if let Some(client) = client_from_argv(&argv)
                && let Some(out) =
                    hook_core::provide(&hook_core::event_name(&payload), &payload, client)
            {
                println!("{out}");
            }
            std::process::exit(0);
        }
        Subcommand::Wire(args) => {
            std::process::exit(ds_wire::run(args));
        }
        Subcommand::Launch(client, args) => {
            let spec = ds_config::client_spec(client);
            std::process::exit(client_launch::run(spec, args));
        }
        Subcommand::Version => {
            println!("dontspeak {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Subcommand::Help => {
            print!("{}", usage_text());
            std::process::exit(0);
        }
        Subcommand::Status(args) => {
            std::process::exit(run_status(args));
        }
        // Unrecognized argv\[1\] must not fall through to MCP (blocks on stdin forever).
        Subcommand::Unknown(sub) => {
            let expected = expected_subcommands();
            let msg = format!(
                "dontspeak: unknown subcommand {sub:?}; expected {expected} \
                 (run with no arguments for the stdio MCP server)"
            );
            eprintln!("{msg}");
            log::error!(target: "hook", "{msg}");
            std::process::exit(2);
        }
        Subcommand::Server => {
            if is_grok_hook_launch(std::env::var_os("GROK_HOOK_EVENT").as_deref()) {
                run_grok_hook();
            } else {
                mcp::serve();
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct StatusCli {
    json: bool,
    since: Option<u64>,
    timeout_ms: Option<u64>,
}

fn parse_status_cli(args: &[String]) -> Result<StatusCli, String> {
    let mut parsed = StatusCli {
        json: false,
        since: None,
        timeout_ms: None,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if !parsed.json => parsed.json = true,
            "--since" if parsed.since.is_none() => {
                index += 1;
                parsed.since = Some(parse_status_number(args.get(index), "--since")?);
            }
            "--timeout-ms" if parsed.timeout_ms.is_none() => {
                index += 1;
                let timeout_ms = parse_status_number(args.get(index), "--timeout-ms")?;
                if !(1..=60_000).contains(&timeout_ms) {
                    return Err("--timeout-ms must be between 1 and 60000".into());
                }
                parsed.timeout_ms = Some(timeout_ms);
            }
            option => return Err(format!("unknown or repeated status option {option:?}")),
        }
        index += 1;
    }
    if !parsed.json && (!args.is_empty()) {
        return Err("status options require `--json`".into());
    }
    if parsed.timeout_ms.is_some() && parsed.since.is_none() {
        return Err("`--timeout-ms` requires `--since`".into());
    }
    Ok(parsed)
}

fn parse_status_number(value: Option<&String>, option: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("{option} requires an integer"))?
        .parse()
        .map_err(|_| format!("{option} requires a non-negative integer"))
}

fn run_status(args: &[String]) -> i32 {
    if args.is_empty() {
        println!(
            "dontspeak {}: runtime status is via MCP tool status, the host app UI, \
             or `dontspeak status --json`",
            env!("CARGO_PKG_VERSION")
        );
        return 0;
    }
    let parsed = match parse_status_cli(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("dontspeak status: {error}");
            return 2;
        }
    };
    match tools::runtime_status_json(parsed.since, parsed.timeout_ms) {
        Ok(status) => match serde_json::to_string(&status) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(error) => {
                eprintln!("dontspeak status: could not encode status: {error}");
                1
            }
        },
        Err(error) => {
            eprintln!("dontspeak status: {error}");
            1
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
    fn wire_token_resolves_to_wire_with_optional_trailing_args() {
        let bare = argv(&["dontspeak", "wire"]);
        assert_eq!(resolve_subcommand(&bare), Subcommand::Wire(&[]));

        let with_args = argv(&["dontspeak", "wire", "claude", "--remove"]);
        assert_eq!(
            resolve_subcommand(&with_args),
            Subcommand::Wire(&["claude".to_string(), "--remove".to_string()])
        );
    }

    #[test]
    fn every_registry_launcher_command_dispatches_with_trailing_args() {
        for spec in ds_config::CLIENT_REGISTRY {
            let command = spec.target.as_str();
            let argv = argv(&["dontspeak", command, "--version"]);
            assert_eq!(
                resolve_subcommand(&argv),
                Subcommand::Launch(spec.target, &argv[2..]),
                "{command}"
            );
        }
    }

    /// Help and the unknown hint are rendered from the registry, with no parallel client list.
    #[test]
    fn usage_and_unknown_hint_list_every_registry_launcher() {
        let usage = usage_text();
        let expected = expected_subcommands();
        for spec in ds_config::CLIENT_REGISTRY {
            let cmd = spec.target.as_str();
            assert!(
                usage.contains(&format!("dontspeak {cmd} ")),
                "usage is missing the `{cmd}` launcher line"
            );
            assert!(
                expected.contains(&format!("`{cmd}`")),
                "unknown-subcommand hint is missing `{cmd}`"
            );
        }
    }

    #[test]
    fn only_launch_keeps_the_console_attached() {
        assert!(!should_detach_console(&Subcommand::Launch(
            WiredAgent::Codex,
            &[]
        )));
        for subcommand in [
            Subcommand::Notify,
            Subcommand::Provide,
            Subcommand::Wire(&[]),
            Subcommand::Version,
            Subcommand::Help,
            Subcommand::Status(&[]),
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
            ("status", Subcommand::Status(&[])),
        ] {
            assert_eq!(
                resolve_subcommand(&argv(&["dontspeak", tok])),
                want,
                "{tok}"
            );
        }
    }

    #[test]
    fn status_json_options_parse_for_snapshot_and_long_poll() {
        assert_eq!(
            parse_status_cli(&argv(&["--json"])).unwrap(),
            StatusCli {
                json: true,
                since: None,
                timeout_ms: None,
            }
        );
        assert_eq!(
            parse_status_cli(&argv(&["--json", "--since", "41", "--timeout-ms", "5000"])).unwrap(),
            StatusCli {
                json: true,
                since: Some(41),
                timeout_ms: Some(5000),
            }
        );
    }

    #[test]
    fn status_json_options_reject_ambiguous_or_invalid_forms() {
        assert!(parse_status_cli(&argv(&["--since", "1"])).is_err());
        assert!(parse_status_cli(&argv(&["--json", "--timeout-ms", "1"])).is_err());
        assert!(parse_status_cli(&argv(&["--json", "--since"])).is_err());
        assert!(parse_status_cli(&argv(&["--json", "--since", "x"])).is_err());
        assert!(
            parse_status_cli(&argv(&["--json", "--since", "1", "--timeout-ms", "60001"])).is_err()
        );
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
        // argv[1] missing — with or without argv[0] — is the bare-server path.
        assert_eq!(
            resolve_subcommand(&argv(&["dontspeak"])),
            Subcommand::Server
        );
        assert_eq!(
            resolve_subcommand(&Vec::<String>::new()),
            Subcommand::Server
        );
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
        for &want in WiredAgent::ALL {
            let tok = want.as_str();
            let argv = argv(&["dontspeak", "notify", "--client", tok]);
            assert_eq!(client_from_argv(&argv), Some(want), "{tok}");
        }
        assert_eq!(
            client_from_argv(&argv(&[
                "dontspeak",
                "notify",
                "--greet-only",
                "--client",
                "qwen"
            ])),
            Some(WiredAgent::QwenCode)
        );
    }

    #[test]
    fn a_missing_malformed_or_unwired_token_is_absent() {
        // Hooks degrade — never hard-error.
        for argv_ in [
            vec!["dontspeak", "notify"],
            vec!["dontspeak", "notify", "--client"],
            vec!["dontspeak", "notify", "--client", "gemini"],
            vec!["dontspeak", "notify", "--client", ""],
        ] {
            assert_eq!(client_from_argv(&argv(&argv_)), None, "{argv_:?}");
        }
    }

    #[test]
    fn client_flag_does_not_disturb_subcommand_dispatch() {
        let client = WiredAgent::Codex.as_str();
        let notify = argv(&["dontspeak", "notify", "--client", client]);
        assert_eq!(resolve_subcommand(&notify), Subcommand::Notify);
        let provide = argv(&["dontspeak", "provide", "--client", client]);
        assert_eq!(resolve_subcommand(&provide), Subcommand::Provide);
    }
}
