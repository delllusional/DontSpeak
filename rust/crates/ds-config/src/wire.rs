//! External-config wiring: pure merge/strip shapers for Claude Code, Qwen, Codex, Grok, and
//! Kimi Code configs, plus atomic-write / backup helpers. Additive and idempotent; path
//! resolution and the atomic write live in the `dontspeak` subcommands.
//!
//! Clients whose hook runner takes a command STRING (Codex, Grok, Qwen, Kimi Code) share the
//! one [`cmdline`] module — Windows quoting is subtle and was got wrong per-client independently.

pub(crate) mod cmdline;

pub mod codex;
pub mod grok_hooks;
pub mod hooks;
pub mod json_mcp;
pub mod kimi_hooks;
pub mod registry;
pub mod settings;
pub mod toml_mcp;
