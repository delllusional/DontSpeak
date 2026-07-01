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
mod wire_hooks;
mod wire_mcp;

// Re-exports reached via `crate::` by the hook/installer subcommands.
pub(crate) use mcp::SERVER_NAME;

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
    if argv.get(1).map(String::as_str) == Some("notify") {
        let payload = read_stdin();
        hook_core::notify(&hook_core::event_name(&payload), &payload);
        std::process::exit(0);
    }
    if argv.get(1).map(String::as_str) == Some("provide") {
        let payload = read_stdin();
        if let Some(out) = hook_core::provide(&hook_core::event_name(&payload), &payload) {
            println!("{out}");
        }
        std::process::exit(0);
    }
    if argv.get(1).map(String::as_str) == Some("wire") {
        std::process::exit(wire::run(&argv[2..]));
    }

    // An explicit but UNRECOGNIZED first argument must NOT fall through to the stdio MCP
    // server: that silently blocks on stdin forever (a typo, or an OLD binary handed a
    // subcommand it predates — e.g. `dontspeak wire` on a build without `wire` — would just
    // hang instead of failing). The MCP server is the NO-argument mode ONLY (how MCP clients
    // spawn us: `command: dontspeak`, no args). So error out on any leftover argument.
    if let Some(sub) = argv.get(1) {
        eprintln!(
            "dontspeak: unknown subcommand {sub:?}; expected `notify`, `provide`, or `wire` \
             (run with no arguments for the stdio MCP server)"
        );
        std::process::exit(2);
    }

    // No arguments: run the stdio MCP server loop.
    mcp::serve();
}

/// Read the whole hook payload from stdin (single-shot). Empty on any read error — the hook
/// then degrades cleanly (an unknown/empty event is a no-op).
fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}
