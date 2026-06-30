//! `dontspeak wire-code [--remove] [--print-only]` — register (or remove) the DontSpeak
//! stdio MCP server in Claude CODE's user config (`~/.claude.json`), so `claude` CLI
//! sessions can spawn this same bridge and call `speak`/`listen`/… on demand — WITHOUT the
//! user ever running `claude mcp add` by hand.
//!
//! The Claude CODE sibling of `wire-desktop`; both share the read → merge/strip → backup →
//! atomic-write core in [`crate::wire_mcp`] (which itself reuses the `ds-config` primitives
//! `wire-hooks` uses). This adapter only supplies the Code specifics: the target file
//! (`~/.claude.json`), the presence gate (the `~/.claude` dir — which `wire-hooks` creates
//! just before us in the installer), and the user-facing labels.

use crate::wire_mcp::{self, Flags, Target};
use ds_config::Paths;

pub fn run(args: &[String]) -> i32 {
    let (remove, print_only) = match wire_mcp::parse_flags("wire-code", args) {
        Flags::Help => return 0,
        Flags::Run { remove, print_only } => (remove, print_only),
    };
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire-code: $HOME not set; nothing to do");
        return 1;
    };
    wire_mcp::apply(
        &Target {
            tool: "wire-code",
            config: &paths.claude_code_config,
            present: paths.claude_dir.exists(),
            absent_hint: format!("Claude Code not detected ({})", paths.claude_dir.display()),
            load_hint: "start a new Claude Code session to load the server",
        },
        remove,
        print_only,
    )
}
