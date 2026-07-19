//! External-config wiring: pure merge/strip shapers + atomic-write/backup helpers.
//! Additive/idempotent; path resolve and disk writes live in `dontspeak` subcommands.
//!
//! String-runner clients share [`cmdline`] (Windows quoting was wrong per-client before).

pub(crate) mod cmdline;

pub mod codex;
pub mod grok_hooks;
pub mod hooks;
pub mod json_mcp;
pub mod kimi_hooks;
pub mod registry;
pub mod settings;
pub mod toml_mcp;
