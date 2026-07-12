//! `dontspeak` — the single multi-call binary. With NO args it is normally a stdio Model
//! Context Protocol (MCP) server that exposes the DontSpeak engine's TTS/STT to MCP clients
//! (e.g. Claude Code). A no-argument launch carrying Grok's reserved `GROK_HOOK_EVENT`
//! variable is instead its deduplicated native/compatibility hook. With a subcommand it is
//! a Claude Code hook executor or installer step — see the front-door dispatch in `main`
//! and the `hook_speak` / `hook_narrate` modules (the former `ds-speak` /
//! `ds-narrate` binaries, now folded in here).
//!
//! As the MCP server it is a THIN BRIDGE: it speaks newline-delimited JSON-RPC 2.0 over
//! stdio on one side (the MCP spec, revision 2025-11-25) and the existing `ds-ipc`
//! Unix-socket protocol to the resident engine on the other — so MCP is just another
//! client of the SAME engine the hooks and host app use (one warm owner, in sync).
//!
//! Tools (the authoritative catalog — names, schemas, descriptions — lives in
//! `ds_tools::catalog()`; this is just an orientation): speak, stop_speech,
//! listen, get_status, list_voices, diarize, manage_speakers, set_config.
//!
//! `list_voices` is config-DIRECT: it reads DontSpeak's own settings file
//! (`our config.toml`) to mark the active voice, so it needs no engine
//! round-trip and works even with no engine running. The voice itself is a
//! persistent setting: all config writes (the spoken voice included) go through
//! `set_config` (same file; the engine hot-reloads on its mtime) — config is the
//! single source of truth, so there is no transient per-session voice override.
//!
//! Transport rules (spec): stdout carries ONLY JSON-RPC messages, one per line;
//! ALL logging goes to stderr. Each request gets exactly one response (matched by
//! id); notifications (no id) get none.
//!
//! ## Module layout
//! `main.rs` is just the front door (subcommand dispatch). The MCP server core lives in
//! [`mcp`] (envelope helpers + [`mcp::dispatch`] + the `initialize`/`tools` methods),
//! the tool handlers in [`tools`], voice/language enumeration in [`voices`], the engine
//! spawn lifecycle in [`engine_launch`], and the `prompt-context` hook in [`hook_prompt`].
// Windows: GUI subsystem so NO console window appears when a GUI host (Claude
// Code / the WinUI app) spawns this stdio server. stdin/stdout still work over the
// inherited pipes the MCP client provides.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod engine_launch;
mod hook_core;
mod hook_narrate;
mod hook_prompt;
mod hook_speak;
mod mcp;
mod tools;
mod voices;

use ds_config::ClientSource;

/// The four things argv\[1\] can select us into, plus the fallback for an unrecognized
/// token. Pure decision extracted from `main` so it's testable without touching stdio.
#[derive(Debug, PartialEq, Eq)]
enum Subcommand<'a> {
    /// `dontspeak notify` — COMMAND hook sink.
    Notify,
    /// `dontspeak provide` — QUERY hook.
    Provide,
    /// `dontspeak wire <client> [--remove]` — carries argv\[2..\] (the args after `wire`).
    Wire(&'a [String]),
    /// No argv\[1\]: the stdio MCP server (the default).
    Server,
    /// An explicit but unrecognized argv\[1\].
    Unknown(String),
}

/// WHICH client invoked this hook — the `--client <token>` verb the wiring stamps into every
/// hook command (`ds_config::wire::cmdline`). Rides at argv\[2+\], like `--greet-only`, so
/// `resolve_subcommand` (which matches argv\[1\] only) is undisturbed.
///
/// Unrecognised, missing, or NON-CLIENT (`dontspeak`, `unknown`) ⇒ [`ClientSource::Unknown`] —
/// never a hard error: a hook must degrade, never fail the client's turn. That is NOT a legacy
/// path (every wired hook carries the token, and the engine re-wires every client at boot): it
/// is the honest answer when the binary is invoked by hand, or by something we don't recognise.
fn client_from_argv(argv: &[String]) -> ClientSource {
    argv.iter()
        .position(|a| a == "--client")
        .and_then(|i| argv.get(i + 1))
        .and_then(|t| ClientSource::parse(t))
        .filter(|c| c.is_client())
        .unwrap_or(ClientSource::Unknown)
}

/// Decide which of the four subcommand roles (or the no-args MCP server default, or the
/// unknown-subcommand fallback) this invocation selects, based on argv\[1\]. Pure — no I/O,
/// no `process::exit`; `main` performs the actual side effects per variant.
fn resolve_subcommand(argv: &[String]) -> Subcommand<'_> {
    match argv.get(1).map(String::as_str) {
        Some("notify") => Subcommand::Notify,
        Some("provide") => Subcommand::Provide,
        Some("wire") => Subcommand::Wire(&argv[2..]),
        Some(other) => Subcommand::Unknown(other.to_string()),
        None => Subcommand::Server,
    }
}

/// Grok injects this reserved variable into every hook process. Its presence is the
/// unambiguous discriminator between a bare-command hook and the normal bare-command MCP
/// server; the value itself is only a marker because the payload remains the routing source
/// of truth. Kept pure so tests never mutate the process-wide environment in parallel.
fn is_grok_hook_launch(marker: Option<&std::ffi::OsStr>) -> bool {
    marker.is_some_and(|value| !value.is_empty())
}

/// Grok's compatibility adapter drops Claude Code's `args`, and Grok deduplicates handlers
/// by their resulting bare command target. One no-argument process must therefore perform
/// both halves of DontSpeak's hook contract: the event side effect and any synchronous query
/// response. `greet_only=true` matters only for SessionStart and keeps this non-streaming
/// client from seeding Claude's MessageDisplay witness.
fn run_grok_hook() {
    let payload = read_stdin();
    let event = hook_core::event_name(&payload);
    hook_core::notify(&event, &payload, true, ClientSource::Grok);
    if let Some(out) = hook_core::provide(&event, &payload) {
        println!("{out}");
    }
}

fn main() {
    // Subcommand front-door — this ONE `dontspeak` binary is every voice role (busybox-style),
    // selected by argv[1]:
    //   `dontspeak notify`             — COMMAND hook sink: read the hook JSON on stdin, run the
    //                                   event's side effect (greet / mark-active / narrate /
    //                                   barge), reply with nothing. Wired on every fire-and-
    //                                   forget event; routes internally on `hook_event_name`.
    //   `dontspeak provide`            — QUERY hook: read the hook JSON on stdin, print the
    //                                   event's `hookSpecificOutput` JSON (UserPromptSubmit →
    //                                   the narration spec). The only entry Claude Code waits on.
    //   `dontspeak wire <client> [--remove]` — the ONE per-client integration installer: it
    //                                   wires (or removes) EVERYTHING that client needs in a
    //                                   single step — claude_code = hooks + MCP, codex
    //                                   = hooks. See the `ds-wire` crate.
    // With no argv it is the stdio MCP server unless Grok's hook runner marker is present.
    // ALL communication is stdio: the MCP tool surface (JSON-RPC over stdio) and the two
    // Claude Code hook verbs above. There is no HTTP transport.
    ds_log::init();
    let argv: Vec<String> = std::env::args().collect();
    match resolve_subcommand(&argv) {
        Subcommand::Notify => {
            let payload = read_stdin();
            // `--greet-only` (wired on SessionStart for NON-streaming clients like Qwen Code):
            // greet, but skip the streaming-witness seed — on a client with no MessageDisplay
            // stream the seed would mark every session "already narrated" and silence each
            // Stop reply. Rides at argv[2+]; `resolve_subcommand` matches argv[1] only.
            let greet_only = argv.iter().any(|a| a == "--greet-only");
            // `--client <token>`: WHO invoked us (see `client_from_argv`). Rides to the engine
            // on every ds-ipc request this hook sends, and onto the activity log.
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
        // An explicit but UNRECOGNIZED first argument must NOT fall through to the stdio MCP
        // server: that silently blocks on stdin forever (a typo, or an OLD binary handed a
        // subcommand it predates — e.g. `dontspeak wire` on a build without `wire` — would just
        // hang instead of failing). The MCP server is the NO-argument mode ONLY (how MCP clients
        // spawn us: `command: dontspeak`, no args). So error out on any leftover argument.
        Subcommand::Unknown(sub) => {
            let msg = format!(
                "dontspeak: unknown subcommand {sub:?}; expected `notify`, `provide`, or `wire` \
                 (run with no arguments for the stdio MCP server)"
            );
            eprintln!("{msg}");
            log::error!(target: "hook", "{msg}");
            std::process::exit(2);
        }
        // No arguments: Grok hooks and MCP servers use the same bare executable. The hook
        // runner's reserved environment marker distinguishes them without changing Claude's
        // args-array hooks or disabling any of the user's other Claude compatibility hooks.
        Subcommand::Server => {
            if is_grok_hook_launch(std::env::var_os("GROK_HOOK_EVENT").as_deref()) {
                run_grok_hook();
            } else {
                mcp::serve();
            }
        }
    }
}

/// Read the whole hook payload from stdin (single-shot). Empty on any read error — the hook
/// then degrades cleanly (an unknown/empty event is a no-op).
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
        // The `--greet-only` flag (non-streaming SessionStart wiring) rides at argv[2];
        // `resolve_subcommand` matches argv[1] only, so it must not disturb the dispatch.
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
    fn unrecognized_token_resolves_to_unknown() {
        let argv = argv(&["dontspeak", "bogus"]);
        assert_eq!(
            resolve_subcommand(&argv),
            Subcommand::Unknown("bogus".to_string())
        );
    }

    #[test]
    fn no_args_resolves_to_server() {
        // Only argv[0] (the program name) present — the documented no-argument case:
        // the stdio MCP server is the default.
        let argv = argv(&["dontspeak"]);
        assert_eq!(resolve_subcommand(&argv), Subcommand::Server);
    }

    #[test]
    fn truly_empty_argv_resolves_to_server() {
        // Defensive: even with no argv[0] at all, argv.get(1) is still None.
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
        ] {
            let argv = argv(&["dontspeak", "notify", "--client", tok]);
            assert_eq!(client_from_argv(&argv), want, "{tok}");
        }
        // The flag rides alongside the other verbs, in any order.
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
        // A hook must DEGRADE, never fail the client's turn — every one of these is `Unknown`,
        // not a hard error. `dontspeak`/`unknown` are real `ClientSource::parse` tokens now, so
        // the `is_client()` filter is what keeps them out (a hook can never claim to be US).
        for argv_ in [
            vec!["dontspeak", "notify"],             // no flag at all (hand-invoked)
            vec!["dontspeak", "notify", "--client"], // flag with no value
            vec!["dontspeak", "notify", "--client", "gemini"], // a client we haven't wired
            vec!["dontspeak", "notify", "--client", "dontspeak"], // us — never a client
            vec!["dontspeak", "notify", "--client", "unknown"], // the literal token
            vec!["dontspeak", "notify", "--client", ""], // empty
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
        // `resolve_subcommand` matches argv[1] ONLY, so the token (like `--greet-only`) rides
        // at argv[2+] without touching dispatch.
        let notify = argv(&["dontspeak", "notify", "--client", "codex"]);
        assert_eq!(resolve_subcommand(&notify), Subcommand::Notify);
        let provide = argv(&["dontspeak", "provide", "--client", "codex"]);
        assert_eq!(resolve_subcommand(&provide), Subcommand::Provide);
    }
}
