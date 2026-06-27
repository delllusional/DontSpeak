//! External-config wiring: the PURE merge/strip shapers that edit the config files
//! DontSpeak integrates with — Claude Code's `settings.json`, Claude Desktop's
//! `claude_desktop_config.json`, and OpenAI Codex's `config.toml` — plus the shared
//! atomic-write / backup helpers. Each shaper is additive and idempotent; path
//! resolution, backups, and the atomic write live in the `dontspeak` subcommands.

pub mod codex;
pub mod desktop;
pub mod hooks;
pub mod settings;
