//! `dontspeak` — the single multi-call binary. With NO args (this file's default role)
//! it is a stdio Model Context Protocol (MCP) server that exposes the DontSpeak engine's
//! TTS/STT to MCP clients (Claude Code, Claude Desktop). With a subcommand it is instead
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
//! listen, get_status, list_voices, diarize, manage_speakers, set_config,
//! setup_integration.
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
mod narrate;
mod tools;
mod voices;
mod wire;

// Re-exports reached via `crate::` by the hook/installer subcommands.
pub(crate) use mcp::SERVER_NAME;

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
    //                                   single step — claude_code = hooks + MCP, claude_desktop
    //                                   = MCP, codex = hooks. See `wire.rs`.
    // With no argv it is the stdio MCP server (the default, spawned by Claude Code / the app).
    // ALL communication is stdio: the MCP tool surface (JSON-RPC over stdio) and the two
    // Claude Code hook verbs above. There is no HTTP transport.
    let argv: Vec<String> = std::env::args().collect();
    match resolve_subcommand(&argv) {
        Subcommand::Notify => {
            let payload = read_stdin();
            hook_core::notify(&hook_core::event_name(&payload), &payload);
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
            std::process::exit(wire::run(args));
        }
        // An explicit but UNRECOGNIZED first argument must NOT fall through to the stdio MCP
        // server: that silently blocks on stdin forever (a typo, or an OLD binary handed a
        // subcommand it predates — e.g. `dontspeak wire` on a build without `wire` — would just
        // hang instead of failing). The MCP server is the NO-argument mode ONLY (how MCP clients
        // spawn us: `command: dontspeak`, no args). So error out on any leftover argument.
        Subcommand::Unknown(sub) => {
            eprintln!(
                "dontspeak: unknown subcommand {sub:?}; expected `notify`, `provide`, or `wire` \
                 (run with no arguments for the stdio MCP server)"
            );
            std::process::exit(2);
        }
        // No arguments: run the stdio MCP server loop.
        Subcommand::Server => {
            mcp::serve();
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
}
