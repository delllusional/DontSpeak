//! External-config wiring: the PURE merge/strip shapers that edit the config files
//! DontSpeak integrates with — Claude Code's `settings.json` and `~/.claude.json`,
//! Qwen Code's `~/.qwen/settings.json`, and OpenAI Codex's `config.toml` — plus the
//! shared atomic-write / backup helpers. Each shaper is additive and idempotent;
//! path resolution, backups, and the atomic write live in the `dontspeak` subcommands.
//!
//! Every client whose hook runner takes a command STRING (Codex, Grok, Qwen) renders and
//! recognises that string through the ONE shared [`cmdline`] module — never its own copy;
//! the Windows quoting rules there are subtle and were got wrong independently per client.

pub(crate) mod cmdline;

pub mod codex;
pub mod grok_hooks;
pub mod hooks;
pub mod json_mcp;
pub mod registry;
pub mod settings;
pub mod toml_mcp;
