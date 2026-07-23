//! Paths and runtime config for the dontspeak Rust workspace.
//!
//! Unified activity log (per-OS logs dir; `ds-log` size rotation, #6).
//! In-process Kokoro (`ds-tts`); models in per-OS data dir — `model_dir()`.
//!
//! Flat public re-exports. `enums` first (`#[macro_use]`) for de/se macros.
//!
//! # What belongs here
//!
//! Config defined/read: [`Paths`], `config.toml` schema/enums, settings bridge, wire shapers.
//! Nearly everything depends here — park only config ownership; pass values to readers.

// `enums` FIRST: `macro_rules!` are textually scoped; `#[macro_use]` lifts them crate-wide.
#[macro_use]
mod enums;
mod brand;
mod claude_code;
mod client_binary;
mod grok_rules;
mod grok_sessions;
mod narration;
mod paths;
mod pidfile;
pub mod speakers;
mod tts_model;
mod voice;
mod wire;

// Flat public re-export facade — preserves every `ds_config::X` path.
pub use brand::{DISPLAY_NAME, VERSION, name_version};
pub use claude_code::{ClaudeCodeVoice, read_claude_code_voice};
pub use client_binary::{
    resolve_client_binary, resolve_configured_client_binary, resolve_native_client_binary,
};
/// Wired-client identity (`ds-client` leaf; re-export avoids cycle with `ds-log`/`ds-ipc`).
pub use ds_client::WiredAgent;
pub use enums::{
    CancelSpeechScope, DiarizerProvider, ListenMode, NarrateKind, Provider, RealizedProvider,
    SttEngine, TrayKind, TtsEngine, de_opt_pref_stt_engine, de_opt_pref_tts_engine,
    default_provider, normalize_tray, provider_pref_wants_gpu,
};
pub use grok_rules::{
    GROK_NARRATE_BEGIN, GROK_NARRATE_END, apply_grok_narrate_section, sync_grok_narrate_agents_md,
    sync_grok_narrate_from_config,
};
pub use grok_sessions::{
    encode_grok_session_cwd, grok_chat_history_path, grok_session_dir, grok_sessions_root,
    grok_updates_jsonl_path, is_updates_jsonl, prefer_chat_history_transcript,
    resolve_grok_chat_history, resolve_grok_session_dir, resolve_grok_updates_jsonl,
    scan_grok_chat_history_by_mtime,
};
pub use narration::{DEFAULT_NARRATION_SPEC, all_blockquotes, all_blockquotes_state};
pub use paths::{Paths, brew_onnxruntime_dylib, data_dir, mlx_dir, model_dir};
pub use pidfile::{evict_stale_engine, is_engine_pid_alive, is_pid_alive, read_engine_pid};
pub use speakers::{Speaker, SpeakerStore};
pub use tts_model::{
    ResolvedTtsParams, SYSTEM_TTS_PARAMS, TTS_MODELS, TtsArgPools, TtsFrontend, TtsModel,
    TtsModelDescriptor, TtsParamDefault, TtsParamDescriptor, TtsParamKind, TtsParamMap,
    TtsParamValue, TtsTargetArgs, tts_model_descriptor, validate_tts_param,
};
pub use voice::{
    CaptureGain, ConfigChange, HandsFreePhrases, TtsParamPools, TtsVoicePools, VoiceConfig,
};
pub use wire::codex::{CodexMergeError, merge_codex_hooks, strip_codex_hooks};
pub use wire::grok_hooks::grok_hooks_value;
pub use wire::hermes_allowlist::{
    desired_approvals as hermes_desired_approvals, merge_hermes_allowlist, strip_hermes_allowlist,
};
pub use wire::hermes_hooks::{merge_hermes_hooks, strip_hermes_hooks};
pub use wire::hermes_mcp::{merge_hermes_mcp, strip_hermes_mcp};
pub use wire::hooks::{HookSpec, HooksMergeError, merge_hooks, strip_hooks};
pub use wire::json_mcp::{merge_mcp_server, strip_mcp_server};
pub use wire::kimi_hooks::{merge_kimi_hooks, strip_kimi_hooks};
pub use wire::registry::{
    CLIENT_REGISTRY, ClientKind, ClientSpec, DocRef, HookCommandStyle, LaunchMode, LaunchSpec,
    Surface, WireMechanism, client_from_mcp_name, client_spec, client_spec_for_launch,
};
pub use wire::settings::{
    atomic_write_json, atomic_write_str, backup_before_write, merge_settings, set_clients_excluded,
    voice_from_value, voice_to_value, write_settings,
};
pub use wire::toml_mcp::{merge_mcp_server_toml, strip_mcp_server_toml};
