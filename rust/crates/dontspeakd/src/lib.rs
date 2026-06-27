//! dontspeakd — dontspeak dictation engine for Claude Code voice (TAP-TOGGLE).
//!
//! Cross-platform Rust port of the original Swift caps-poll daemon (since removed).
//! Reads the PHYSICAL Caps key
//! (down/up edges, via the platform's IOHIDManager monitor on macOS) every
//! POLL_MS and drives Claude Code's voice TAP mode: a TAP toggles recording.
//!
//! State machine ("tap to dictate, hold to cancel") — decided on the key's RELEASE,
//! NOT its press, so the Caps LED (a pure output we drive) only moves on release:
//! - physical TAP (quick press → release before long_press_ms): toggle dictation ON
//!   THE RELEASE. The start tap barges in (kills any playing TTS via the shared
//!   pidfile) and routes through the boxed STT engine (`stt.start()`); the next tap
//!   `stt.stop()`s it. For the default (ClaudeNative) start/stop each emit ONE Ctrl+G
//!   byte (Claude's tap toggle); for Parakeet start opens the mic and stop runs the
//!   final pass + injects. The LED lights on the start release, extinguishes on the
//!   stop release.
//! - physical LONG-PRESS (hold ≥ long_press_ms): `cancel_all` — discard any in-flight
//!   dictation (`stt.abort()`) AND silence the voice/generation, idle, LED off. A hold
//!   NEVER records and never lights; the release that ends it is NOT counted as a tap.
//!   (A sub-poll tap too fast for the ~POLL_MS sampler to see the key-down is missed —
//!   tap again. The LED is never read back, so there's no latch/LED desync.)
//!
//! Phase 2: the Caps-Lock state machine is UNCHANGED; only what each edge DOES is
//! now behind `Box<dyn Stt>`, selected by config via the `ds-engines` factory.
//! `ClaudeNative` reproduces the Phase-1 emit bodies byte-for-byte.
//!
//! Phase 4 (§E.4 hot-reload): the engine writes its own pid to its
//! `dontspeakd.pid` on startup (removed on clean exit), installs a SIGHUP
//! handler alongside SIGTERM/SIGINT, and watches `config.toml` by mtime each
//! tick. EITHER a SIGHUP (the GUI's explicit "reload now" nudge) OR an mtime
//! change re-runs `VoiceConfig::load` and rebuilds the boxed `Stt` via the
//! factory — no restart. `Engine::reload` ends any in-flight HOLD cleanly
//! (engine `abort()`) before swapping engines, WITHOUT driving the Caps LED or
//! emitting a spurious edge. A debounce window collapses the GUI's write+SIGHUP
//! into one reload.
//!
//! The platform surface (caps read / key inject / frontmost) is behind the
//! ds-platform traits; only the macOS impl is compiled on the build host.
//!
//! ## Module layout
//! The engine was split out of one god-file into focused modules; this `lib.rs` is
//! the crate-doc + facade that re-exports the public API the host consumes:
//! - `boot` — lifecycle/orchestration: [`engine_run`], [`run_headless`],
//!   [`EngineError`], signal handlers, `install_bin`.
//! - `engine` — the `Engine<P>` gesture state machine + the dictation-preview buffer.
//! - `ipc` — the RPC server thread + its request-dispatch arms.
//! - `status` — the `model_status` aggregator + the caps-event status channel.
//! - `downloads` — background model-download state + the auto-fetch orchestration.
//! - `config_gate` — the pure config predicates + reload-decision fns.
//! - `barge` — the mic-barge watcher thread.
//! - `log` — engine logging → the unified activity log.

// Always-listening: `listen` is the pure, unit-tested core (endpointer, stopword,
// turn logic); `listener` is the runtime glue the poll loop drives. The allow
// covers a couple of inspector methods exercised only by `listen`'s unit tests.
mod helper_stt;
#[allow(dead_code)]
mod listen;
mod listener;
mod stats;
mod stt_test;
mod tts;
mod ttsq;

mod barge;
mod boot;
mod config_gate;
mod config_watch;
mod downloads;
mod engine;
mod ipc;
mod logging;
mod status;

// The host (the `dontspeakd` binary's `main.rs` and the `ds-core` FFI crate)
// consumes ONLY these three items.
pub use boot::{EngineError, engine_run, run_headless};

// Crate-root re-exports so the sibling modules that pre-date the split keep
// resolving their historical paths without edits: `crate::log(...)` (the function,
// not the `logging` module) and `crate::{PasteBuf, PasteState}`.
pub(crate) use engine::{PasteBuf, PasteState};
pub(crate) use logging::log;
