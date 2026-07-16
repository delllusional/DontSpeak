//! Centralized paths and runtime config for the dontspeak Rust workspace.
//!
//! The existing system uses these fixed locations (DO NOT relocate — the
//! pidfile is the single-speaker contract shared between the engine's barge-in
//! and the hook executor):
//!   speak-hook.pid (in the per-OS state dir)  process-GROUP id of the current speaker
//!   ~/.claude/hooks/                          hook helpers (mic-active, ...)
//!
//! The unified activity log lives in the per-OS logs dir (macOS:
//! `~/Library/Logs/DontSpeak/dontspeak.log`) with lean, sudo-free in-process
//! size rotation (rename-based) — see `Paths::log_file` and `ds_log::log()`
//! (the writer itself lives in the `ds-log` crate, split out per issue #6). No `newsyslog`.
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
//!
//! # What belongs here (and what doesn't)
//!
//! This crate is where configuration is DEFINED and read, not where it is acted
//! on: [`Paths`], the `config.toml` schema and its enums, the read-only
//! `settings.json` bridge, and the client wire shapers.
//!
//! It is NOT a home for runtime state machines, engine behavior, or protocol
//! definitions — nearly everything depends on this crate, so code parked here
//! is code everything transitively rebuilds and links. If a new piece's only
//! tie to config is that it reads some of it, it doesn't belong here: put the
//! behavior next to its owner and pass the config value in.

// `enums` FIRST: its `macro_rules!` (`fail_open_de!`, `serialize_as_str!`, `strict_de!`)
// are textually scoped, so it must be declared before anything that uses them.
// `#[macro_use]` lifts them to the crate so a future sibling could invoke them too.
#[macro_use]
mod enums;
mod brand;
mod claude_code;
mod grok_rules;
mod narration;
mod paths;
mod pidfile;
pub mod speakers;
mod voice;
mod wire;

// MCP HTTP transport settings — kept its own module; re-exported flat below.

// ── Flat public re-export facade — preserves every `ds_config::X` path ──────────
pub use brand::{DISPLAY_NAME, VERSION, name_version};
pub use claude_code::{ClaudeCodeVoice, read_claude_code_voice};
/// The client IDENTITY enum, re-exported from the `ds-client` leaf crate (it used to be this
/// crate's own `WireTarget`). It lives BELOW `ds-log`/`ds-ipc` so both can take a client
/// without a dependency cycle on `ds-config`; re-exporting it here keeps every downstream
/// `ds_config::ClientSource` path working with no Cargo.toml edit.
pub use ds_client::ClientSource;
pub use enums::{
    CancelSpeechScope, DiarizerProvider, ListenMode, NarrateKind, Provider, RealizedProvider,
    SttEngine, TrayKind, TtsEngine, de_opt_pref_stt_engine, de_opt_pref_tts_engine,
    default_provider, intel_mac_builtin_ort_available, normalize_tray_indicator,
    provider_pref_wants_gpu,
};
pub use grok_rules::{
    GROK_NARRATE_BEGIN, GROK_NARRATE_END, apply_grok_narrate_section, clear_grok_narrate_agents_md,
    sync_grok_narrate_agents_md, sync_grok_narrate_from_config,
};
pub use narration::{DEFAULT_NARRATION_SPEC, all_blockquotes, all_blockquotes_state};
pub use paths::{
    Paths, brew_onnxruntime_dylib, coreml_dir, coreml_model_present, data_dir, model_dir,
};
pub use pidfile::{evict_stale_engine, is_engine_pid_alive, is_pid_alive, read_engine_pid};
pub use speakers::{Speaker, SpeakerStore};
pub use voice::{CaptureGain, ConfigChange, DEFAULT_KOKORO_VOICE, HandsFreePhrases, VoiceConfig};
pub use wire::codex::{CodexMergeError, merge_codex_hooks, strip_codex_hooks};
pub use wire::grok_hooks::grok_hooks_value;
pub use wire::hooks::{HookSpec, HooksMergeError, INSTALLED_BINS, merge_hooks, strip_hooks};
pub use wire::json_mcp::{merge_mcp_server, strip_mcp_server};
pub use wire::registry::{
    CLIENT_REGISTRY, ClientKind, ClientSpec, DocRef, HookCommandStyle, LaunchMode, LaunchSpec,
    Surface, WireMechanism, client_from_mcp_name, client_spec, client_spec_for_launch,
};
pub use wire::settings::{
    atomic_write_json, atomic_write_str, backup_before_write, merge_settings, voice_from_value,
    voice_to_value, write_settings,
};
pub use wire::toml_mcp::{merge_mcp_server_toml, strip_mcp_server_toml};
