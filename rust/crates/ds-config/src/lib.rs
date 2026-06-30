//! Centralized paths and runtime config for the dontspeak Rust workspace.
//!
//! The existing system uses these fixed locations (DO NOT relocate — the
//! pidfile is the single-speaker contract shared between the engine's barge-in
//! and the hook executor):
//!   ~/.claude/speak-hook.pid   process-GROUP id of the current speaker
//!   ~/.claude/hooks/           hook helpers (mic-active, ...)
//!
//! The unified activity log lives at `~/Library/Logs/dontspeak.log` with lean,
//! sudo-free in-process size rotation (rename-based) — see `Paths::log_file` and
//! `log()`. No `newsyslog`.
//!
//! Synthesis is NATIVE in-process Kokoro (ds-tts: ort + voice-g2p + rodio).
//! Model assets (kokoro onnx + voices + the onnxruntime dylib) live in the
//! per-OS data dir from `directories` (NOT in the repo, NOT bundled) — see
//! `model_dir()`.
//!
//! This crate is split into focused modules, but its PUBLIC API is flat: every
//! item is re-exported at the crate root, so external crates keep using the
//! `ds_config::X` paths they always have. `enums` is declared first (with
//! `#[macro_use]`) so its declarative deserialize/serialize macros are textually
//! in scope.

// `enums` FIRST: its `macro_rules!` (`fail_open_de!`, `serialize_as_str!`, `strict_de!`)
// are textually scoped, so it must be declared before anything that uses them.
// `#[macro_use]` lifts them to the crate so a future sibling could invoke them too.
#[macro_use]
mod enums;
mod brand;
mod claude_code;
mod earcon;
mod log;
mod narration;
mod paths;
mod pidfile;
mod set_config;
pub mod speakers;
mod voice;
mod wire;

// MCP HTTP transport settings — kept its own module; re-exported flat below.

// ── Flat public re-export facade — preserves every `ds_config::X` path ──────────
pub use brand::{DISPLAY_NAME, VERSION, name_version};
pub use claude_code::{ClaudeCodeVoice, read_claude_code_voice};
pub use earcon::{EarconEvent, SystemSound, resolve_cue, system_sounds};
pub use enums::{
    DiarizerProvider, DropSpeechKind, ListenMode, NarrateKind, Provider, SttEngine, TrayKind,
    TtsEngine, WireTarget,
};
pub use log::{
    LogLevel, aux_log_path, combined_log_json, log, log_tail, open_aux_log, rotate_if_large,
};
pub use narration::{DEFAULT_NARRATION_SPEC, all_blockquotes, all_blockquotes_state};
pub use paths::{Paths, coreml_dir, coreml_model_present, data_dir, model_dir};
pub use pidfile::{engine_pid_alive, evict_stale_engine, pid_alive, read_engine_pid};
pub use set_config::SetConfigArgs;
pub use speakers::{Speaker, SpeakerStore};
pub use voice::{CaptureGain, ConfigChange, DEFAULT_KOKORO_VOICE, HandsFreePhrases, VoiceConfig};
pub use wire::codex::{CodexMergeError, merge_codex_hooks, strip_codex_hooks};
pub use wire::desktop::{merge_mcp_server, strip_mcp_server};
pub use wire::hooks::{HookSpec, INSTALLED_BINS, merge_hooks, strip_hooks};
pub use wire::settings::{
    atomic_write_json, atomic_write_str, backup_before_write, merge_settings, voice_from_value,
    voice_to_value, write_settings,
};
