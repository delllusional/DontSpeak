//! Centralized paths and runtime config for the dontspeak Rust workspace.
//!
//! Fixed locations (DO NOT relocate — pidfile is the single-speaker contract shared by
//! barge-in and the hook executor):
//!   speak-hook.pid (per-OS state dir)  process-GROUP id of the current speaker
//!
//! Unified activity log: per-OS logs dir (e.g. macOS `~/Library/Logs/DontSpeak/dontspeak.log`),
//! rename-based size rotation in `ds-log` (issue #6). No `newsyslog`.
//!
//! Synthesis is native in-process Kokoro (`ds-tts`). Model assets live in the per-OS data dir
//! from `directories` (not repo, not bundled) — see `model_dir()`.
//!
//! Modules are focused; PUBLIC API is flat (re-exports at crate root). `enums` is first
//! (`#[macro_use]`) so deserialize/serialize macros are textually in scope.
//!
//! # What belongs here
//!
//! Configuration DEFINED and read: [`Paths`], `config.toml` schema/enums, settings.json bridge,
//! client wire shapers. Not runtime state machines, engine behavior, or protocol defs —
//! nearly everything depends here, so code parked here rebuilds the world. Behavior that only
//! *reads* config belongs with its owner; pass the value in.

// `enums` FIRST: `macro_rules!` are textually scoped; `#[macro_use]` lifts them crate-wide.
#[macro_use]
mod enums;
mod brand;
mod claude_code;
mod grok_rules;
mod grok_sessions;
mod narration;
mod paths;
mod pidfile;
pub mod speakers;
mod voice;
mod wire;

// ── Flat public re-export facade — preserves every `ds_config::X` path ──────────
pub use brand::{DISPLAY_NAME, VERSION, name_version};
pub use claude_code::{ClaudeCodeVoice, read_claude_code_voice};
/// Client identity from the `ds-client` leaf (ex-`WireTarget`). Lives below `ds-log`/`ds-ipc`
/// to avoid a cycle; re-export keeps `ds_config::ClientSource` working.
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
pub use grok_sessions::{
    encode_grok_session_cwd, grok_chat_history_path, grok_session_dir, grok_sessions_root,
    grok_updates_jsonl_path, is_updates_jsonl, prefer_chat_history_transcript,
    resolve_grok_chat_history, resolve_grok_session_dir, resolve_grok_updates_jsonl,
    scan_grok_chat_history_by_mtime,
};
pub use narration::{
    DEFAULT_NARRATION_SPEC, all_blockquotes, all_blockquotes_state, clean_for_speech,
};
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
