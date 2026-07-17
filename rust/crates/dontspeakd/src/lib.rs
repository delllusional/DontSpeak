//! dontspeakd — in-process dictation engine for Claude Code voice (TAP-TOGGLE),
//! hosted by each platform app via `ds-core` (no standalone binary).
//!
//! Physical Caps edges (via platform IOHIDManager on macOS) every POLL_MS drive
//! Claude Code voice TAP mode. State machine decides on RELEASE, not press, so
//! the Caps LED (pure output) only moves on release:
//! - TAP (release before long_press_ms): toggle dictation. Start barges TTS and
//!   routes through `stt.start()`; next tap `stt.stop()`s. ClaudeNative emits one
//!   Ctrl+G per edge; Parakeet opens/closes the mic + injects. LED on start, off stop.
//! - LONG-PRESS (≥ long_press_ms): `cancel_all` — abort dictation + silence voice,
//!   idle, LED off. Never records; ending release is not a tap.
//!
//! STT is a config-selected `Box<dyn Stt>` from `ds-engines`. Hot reload watches
//! config.toml mtime + explicit reload (C ABI / Reload RPC); `Engine::reload`
//! aborts in-flight HOLD before swapping engines (no LED, no spurious edge).
//! Platform surface: ds-platform traits.
//!
//! ## Modules
//! - `boot` — [`engine_run`], [`EngineError`], `install_bin`
//! - `engine` — `Engine<P>` gesture machine + dictation-preview buffer
//! - `ipc` — RPC server + request arms
//! - `status` — `model_status` aggregator + caps-event channel
//! - `downloads` — background model-download + auto-fetch
//! - `config_gate` — pure config predicates + reload decisions
//! - `barge` — mic-barge watcher
//! - `codex_stream` — Codex app-server mid-turn narration
//! - `grok_stream` — Grok updates.jsonl mid-turn narration
//! - `listen` / `listener` — always-listening pure core / poll-loop glue

// `listen` is pure/tested; `listener` is runtime glue. allow covers inspector
// methods only used by `listen`'s unit tests.
mod child_slot;
mod helper_stt;
#[allow(dead_code)]
mod listen;
mod listener;
mod model_slot;
mod stats;
mod stt_test;
mod tts;
mod ttsq;

mod barge;
mod boot;
mod codex_stream;
mod config_gate;
mod config_watch;
mod downloads;
mod engine;
mod grok_stream;
mod ipc;
mod status;
mod timer;

// In-process host (`ds-core` FFI) consumes only these two.
pub use boot::{EngineError, engine_run};

// Historical crate-root paths for modules that predate the split.
pub(crate) use engine::{FinalState, PasteBuf, PasteState};
