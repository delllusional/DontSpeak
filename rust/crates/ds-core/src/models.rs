//! Model asset present-probes (UI-agnostic).
//!
//! Cheap, network-free, UI-thread-safe (stat + sha-verify under `model_dir()`). Thin
//! delegates to `ds-model` so the C ABI has a stable call site.
//!
//! No download runner here: fetching + progress lives only in
//! `dontspeakd::downloads` → `ds_model::run_setup_*_with_progress`. A second path would drift.

/// Full Kokoro asset set (onnx + voices + dylib) present + valid?
pub fn is_kokoro_present() -> bool {
    ds_model::is_kokoro_present()
}

/// Full Parakeet-ONNX asset set present + valid?
pub fn is_parakeet_onnx_present() -> bool {
    ds_model::is_parakeet_present()
}
