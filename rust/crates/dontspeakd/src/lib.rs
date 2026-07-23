//! In-process dictation engine (host apps via `ds-core`; no standalone binary).
//!
//! Caps edges each POLL_MS; decisions on RELEASE (LED pure output):
//! - TAP (< long_press_ms): toggle dictation (`stt.start`/`stop`; start barges TTS).
//! - LONG-PRESS: `cancel_all` — abort + silence; release is not a tap.
//!
//! STT from `ds-engines`. Reload (mtime + C ABI / Reload RPC) aborts in-flight hold
//! before engine swap. Platform: `ds-platform` traits.
//!
//! Modules: `boot`, `engine`, `ipc`, `status`, `downloads`, `config_gate`, `barge`,
//! `codex_stream`, `grok_stream`, `listen`/`listener`.

// `listen` pure/tested; `listener` runtime. allow: test-only inspectors.
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
#[cfg(test)]
mod test_env;
mod timer;

pub use boot::{EngineError, engine_run};

pub(crate) use engine::{FinalState, PasteBuf, PasteState};
