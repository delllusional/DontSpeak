//! Shared streaming-narration core — one pipeline for every client adapter.
//!
//! From [`StreamBatch`]s: emit each top-level blockquote run verbatim, exactly once,
//! in document order (plus "shorts" for a blockquote-less final reply). Progress lives
//! in `narrate-display-<session>.json`, which is also the cross-process **streaming
//! witness** that keeps `Stop` silent after mid-turn narration.
//!
//! Adapters (see `docs/STREAMING-NARRATION.md`): Claude Code (delta by content-block
//! `index`, racing processes + state lock), Qwen Code (cumulative snapshots), OpenAI
//! Codex (`dontspeakd::codex_stream`, in-process) and Grok (`dontspeakd::grok_stream`,
//! updates.jsonl tail). All use [`deliver_batch`] so the
//! on-disk `offset` prevents double-speak on reconnect.

mod accum;
mod stream;

pub use accum::{Accum, DETECTION_TEXT_MAX_BYTES, SelectedUtterance, cap_detection_text};
pub use stream::{
    BatchPayload, DisplayState, DisplayStep, NarrationUtterance, StreamBatch, clear_session_state,
    deliver_batch, display_state_path, retry_pending, seed_witness, step, stop_utterances,
    witness_exists,
};
