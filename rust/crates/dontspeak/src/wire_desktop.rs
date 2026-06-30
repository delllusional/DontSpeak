//! `dontspeak wire-desktop [--remove] [--print-only]` — register (or remove) the DontSpeak
//! stdio MCP server in Claude DESKTOP's `claude_desktop_config.json`, so Desktop can spawn
//! this same bridge and call `speak`/`listen`/… on demand.
//!
//! The Claude DESKTOP sibling of `wire-code`; both share the read → merge/strip → backup →
//! atomic-write core in [`crate::wire_mcp`]. This adapter only supplies the Desktop
//! specifics: the target file, the presence gate ([`Paths::claude_desktop_present`], so we
//! never scatter a stray `Claude/` config dir on a machine without Desktop), and the labels.
//! Desktop has no hook system, so this is MCP registration ONLY — there is no narration
//! wiring (that is `wire-hooks`, against `settings.json`).

use crate::wire_mcp::{self, Flags, Target};
use ds_config::Paths;

pub fn run(args: &[String]) -> i32 {
    let (remove, print_only) = match wire_mcp::parse_flags("wire-desktop", args) {
        Flags::Help => return 0,
        Flags::Run { remove, print_only } => (remove, print_only),
    };
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire-desktop: $HOME not set; nothing to do");
        return 1;
    };
    wire_mcp::apply(
        &Target {
            tool: "wire-desktop",
            config: &paths.claude_desktop_config,
            present: paths.claude_desktop_present(),
            absent_hint: format!(
                "Claude Desktop not detected ({})",
                paths.claude_desktop_dir.display()
            ),
            load_hint: "quit and reopen Claude Desktop to load the server",
        },
        remove,
        print_only,
    )
}
