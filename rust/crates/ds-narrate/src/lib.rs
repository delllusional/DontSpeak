//! ds-narrate — the SHARED streaming-narration core: one invariant pipeline behind every
//! client adapter. Given a stream of per-message text batches ([`StreamBatch`]) it decides
//! which top-level blockquote runs become speakable (each verbatim, exactly once, in
//! document order — with the "shorts" fallback for a short blockquote-less final reply),
//! and persists the per-session progress through the SAME on-disk state file
//! (`narrate-display-<session>.json`) that doubles as the cross-process **streaming
//! witness** keeping the `Stop` hook silent for sessions that already narrated mid-turn.
//!
//! Three thin adapters feed it (see `docs/STREAMING-NARRATION.md`):
//!   * **Claude Code** — per-batch `notify` hook processes; DELTA chunks keyed by
//!     content-block `index` (racing processes serialized by the state-file lock).
//!   * **Qwen Code** — the same hook route with CUMULATIVE `displayed_text` snapshots.
//!   * **OpenAI Codex** — the engine's long-lived app-server subscriber
//!     (`dontspeakd::codex_stream`), translating `item/agentMessage/delta` /
//!     `item/completed` in-process.
//!
//! Every adapter persists through [`narrate_batch`], so the witness comes for free for
//! all three and a reconnect/restart can never double-speak (the `offset` high-water
//! mark is on disk). Extracted from the `dontspeak` crate (`narrate.rs` +
//! `hook_narrate.rs`) so BOTH the CLI hooks and the engine can depend on it without the
//! CLI growing an engine dependency.

mod accum;
mod stream;

pub use accum::{Accum, short_reply_utterance};
pub use stream::{
    BatchPayload, DisplayState, DisplayStep, StreamBatch, clear_session_state, display_state_path,
    narrate_batch, seed_witness, step, stop_utterances, witness_exists,
};
