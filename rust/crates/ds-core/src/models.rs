//! Model asset present-probes. UI-AGNOSTIC.
//!
//! Probes are cheap, network-free, and safe to call on the UI thread (they only
//! stat + sha-verify files under `model_dir()`). They're thin delegates to the
//! `ds-model` probes so the C ABI (`ffi.rs`) has a stable spot to call.
//!
//! There is deliberately NO download runner here: model fetching + progress
//! reporting lives in ONE place — the daemon's background download manager
//! (`dontspeakd::downloads`), which drives `ds_model::run_setup_*_with_progress`
//! (the single monotonic `(done, total)` aggregator) and surfaces per-target
//! progress through the status snapshot. Keeping a second progress path here would
//! duplicate that logic and let the two drift.

/// Is the full Kokoro asset set (onnx + voices + dylib) present + valid?
pub fn is_kokoro_present() -> bool {
    ds_model::is_kokoro_present()
}

/// Is the full Parakeet-ONNX asset set (encoder + decoder + joiner + tokens
/// + dylib) present + valid?
pub fn is_parakeet_onnx_present() -> bool {
    ds_model::is_parakeet_present()
}
