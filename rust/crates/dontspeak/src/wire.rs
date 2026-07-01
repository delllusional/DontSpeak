//! `dontspeak wire <client> [--remove] [--print-only]` — the ONE per-client integration
//! installer. Each client gets its FULL integration wired (or removed) in a single step; there is
//! no separate "install hooks" vs "install MCP" task:
//!   • `claude_code`    → voice hooks (`~/.claude/settings.json`) AND the MCP server (`~/.claude.json`)
//!   • `claude_desktop` → the MCP server (`claude_desktop_config.json`)   [Desktop has no hooks]
//!   • `codex`          → narration hooks (`~/.codex/config.toml`)        [Codex has no MCP]
//!
//! Every surface REUSES the shared cores — the `ds-config` hook/MCP shapers, the
//! [`wire_mcp::apply`](crate::wire_mcp::apply) read→merge→backup→atomic-write flow, and the
//! [`wire_hooks`](crate::wire_hooks) writers — so nothing is copy-pasted per client, and this
//! install-time entry and the `setup_integration` tool drive the IDENTICAL code (they can't drift).
//! Additive + idempotent + backed-up; a client that isn't installed is a clean skip (exit 0).

use crate::{wire_hooks, wire_mcp};
use ds_config::{Paths, WireTarget};

/// Parse `<client> [--remove] [--print-only]` and wire (or unwire) that client's whole integration.
/// Returns a process exit code (0 ok / skipped, 1 hard error). `client` is a [`WireTarget`] token
/// (`claude_code`/`claude_desktop`/`codex`); `narration_spec` is a config-file concern of the
/// `setup_integration` tool, not a client, so it is rejected here.
pub fn run(args: &[String]) -> i32 {
    let mut client: Option<WireTarget> = None;
    let mut remove = false;
    let mut print_only = false;
    let mut all = false;
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--remove" => remove = true,
            "--print-only" | "--print" => print_only = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: dontspeak wire <claude_code|claude_desktop|codex> [--remove] [--print-only]\n       dontspeak wire --all [--remove] [--print-only]   (every known client; each self-skips if absent)"
                );
                return 0;
            }
            other if other.starts_with('-') => eprintln!("wire: ignoring unknown flag {other:?}"),
            other => match WireTarget::parse(other) {
                Some(
                    t @ (WireTarget::ClaudeCode | WireTarget::ClaudeDesktop | WireTarget::Codex),
                ) => client = Some(t),
                _ => {
                    eprintln!(
                        "wire: unknown client {other:?}; expected claude_code, claude_desktop, or codex"
                    );
                    return 1;
                }
            },
        }
    }
    if !all && client.is_none() {
        eprintln!("wire: missing client (claude_code | claude_desktop | codex), or use --all");
        return 1;
    }
    let Some(paths) = Paths::resolve() else {
        eprintln!("wire: $HOME not set; nothing to do");
        return 1;
    };

    // Client-agnostic install housekeeping on any real wire (idempotent; per-client is fine).
    if !remove && !print_only {
        wire_hooks::seed_and_prune(&paths);
    }

    // `--all` wires (or unwires) EVERY known client from the canonical WireTarget::CLIENTS list —
    // the single source the per-platform installers used to hand-copy. Each self-skips when its
    // client is absent; return the WORST exit code so one client's hard error still surfaces.
    if all {
        return WireTarget::CLIENTS
            .iter()
            .map(|&c| wire_client(c, &paths, remove, print_only))
            .max()
            .unwrap_or(0);
    }

    wire_client(client.expect("checked above"), &paths, remove, print_only)
}

/// Wire (or unwire) ONE client's full set of surfaces, composed from the shared writers.
fn wire_client(client: WireTarget, paths: &Paths, remove: bool, print_only: bool) -> i32 {
    match client {
        // Claude Code = hooks (settings.json) + MCP (~/.claude.json), in ONE step. The hooks write
        // creates `~/.claude` first, so the MCP target's presence gate then passes. Attempt BOTH
        // surfaces even if one fails (return the worst exit code): a malformed settings.json that
        // makes the hooks writer bail must NOT skip the MCP surface, or `--remove` would leave the
        // ~/.claude.json entry dangling at a deleted binary path.
        WireTarget::ClaudeCode => {
            let hooks_rc = wire_hooks::claude_code_hooks(paths, remove, print_only);
            let mcp_rc = wire_mcp::apply(&wire_mcp::code_target(paths), remove, print_only);
            hooks_rc.max(mcp_rc)
        }
        // Claude Desktop = MCP only (no hook system). `apply` self-skips when Desktop is absent.
        WireTarget::ClaudeDesktop => {
            wire_mcp::apply(&wire_mcp::desktop_target(paths), remove, print_only)
        }
        // Codex = narration hooks only (no MCP). Presence-gated so we never scatter a `~/.codex`
        // config on a machine without Codex; on --remove strip an existing config.toml only.
        WireTarget::Codex => {
            if !print_only {
                if remove {
                    if !paths.codex_config.exists() {
                        return 0;
                    }
                } else if !paths.codex_dir.exists() {
                    eprintln!(
                        "wire: OpenAI Codex not detected ({}); skipping",
                        paths.codex_dir.display()
                    );
                    return 0;
                }
            }
            wire_hooks::codex_hooks(paths, remove, print_only)
        }
        // `run` rejects narration_spec before we get here.
        WireTarget::NarrationSpec => {
            eprintln!("wire: narration_spec is not a client; use the setup_integration tool");
            1
        }
    }
}
