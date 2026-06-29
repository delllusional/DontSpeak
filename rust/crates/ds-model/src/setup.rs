//! Eager pre-download orchestrators: fetch a whole component's asset set
//! (Kokoro TTS / Parakeet STT + the shared onnxruntime dylib) and report a single
//! aggregate `(downloaded, total)` progress stream so the GUI shows one bar.

use std::path::PathBuf;

use crate::download::{ensure, ensure_with_progress};
use crate::ort::{ensure_onnxruntime, ensure_onnxruntime_with_progress};
use crate::spec::{
    kokoro_files, kokoro_onnx_spec, kokoro_voices_spec, parakeet_decoder_spec, parakeet_dir,
    parakeet_encoder_spec, parakeet_files, parakeet_joiner_spec, parakeet_tokens_spec,
};

/// Eager pre-download of the FULL Parakeet asset set: encoder, decoder, preprocessor,
/// vocab, AND the shared onnxruntime dylib (route A). Returns the model dir on success.
pub fn run_setup_parakeet() -> std::io::Result<PathBuf> {
    ensure(&parakeet_encoder_spec())?;
    ensure(&parakeet_decoder_spec())?;
    ensure(&parakeet_joiner_spec())?;
    ensure(&parakeet_tokens_spec())?;
    ensure_onnxruntime()?;
    parakeet_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
}

/// Like [`run_setup_parakeet`] but reports AGGREGATE byte-level progress across
/// the whole asset set (encoder + decoder + preprocessor + vocab + onnxruntime
/// dylib) as a single `(downloaded, total)` stream, so the GUI shows one combined bar.
pub fn run_setup_parakeet_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let files = parakeet_files();
    let grand_total: u64 = files.iter().map(|f| f.size_bytes).sum();
    let enc_size = files.first().map(|f| f.size_bytes).unwrap_or(0);
    let dec_size = files.get(1).map(|f| f.size_bytes).unwrap_or(0);
    let pre_size = files.get(2).map(|f| f.size_bytes).unwrap_or(0);
    let voc_size = files.get(3).map(|f| f.size_bytes).unwrap_or(0);

    ensure_with_progress(&parakeet_encoder_spec(), &|done, _| {
        progress(done.min(grand_total), grand_total);
    })?;
    ensure_with_progress(&parakeet_decoder_spec(), &|done, _| {
        progress((enc_size + done).min(grand_total), grand_total);
    })?;
    ensure_with_progress(&parakeet_joiner_spec(), &|done, _| {
        progress((enc_size + dec_size + done).min(grand_total), grand_total);
    })?;
    ensure_with_progress(&parakeet_tokens_spec(), &|done, _| {
        progress(
            (enc_size + dec_size + pre_size + done).min(grand_total),
            grand_total,
        );
    })?;
    ensure_onnxruntime_with_progress(&|done, _| {
        progress(
            (enc_size + dec_size + pre_size + voc_size + done).min(grand_total),
            grand_total,
        );
    })?;
    progress(grand_total, grand_total);
    parakeet_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
}

/// Eager pre-download of the FULL native-Kokoro asset set: the onnx model, the
/// voices file, AND the onnxruntime dylib (route A). The lazy caller
/// (`ds-helper` on first speak) uses [`crate::kokoro_present`] + these `ensure_*`
/// directly. Returns the model path on success.
pub fn run_setup_kokoro() -> std::io::Result<PathBuf> {
    let model = ensure(&kokoro_onnx_spec())?;
    ensure(&kokoro_voices_spec())?;
    ensure_onnxruntime()?;
    Ok(model)
}

/// Like [`run_setup_kokoro`] but reports AGGREGATE byte-level progress across the
/// whole asset set (onnx + voices + onnxruntime dylib) as a single
/// `(downloaded, total)` stream — so the GUI shows one "X MB of Y MB" bar for the
/// combined Kokoro download. `total` is the summed [`kokoro_files`] size; each
/// file's bytes are offset onto the running base so the bar advances monotonically.
pub fn run_setup_kokoro_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let files = kokoro_files();
    let grand_total: u64 = files.iter().map(|f| f.size_bytes).sum();
    let onnx_size = files.first().map(|f| f.size_bytes).unwrap_or(0);
    let voices_size = files.get(1).map(|f| f.size_bytes).unwrap_or(0);

    let model = ensure_with_progress(&kokoro_onnx_spec(), &|done, _| {
        progress(done.min(grand_total), grand_total);
    })?;
    ensure_with_progress(&kokoro_voices_spec(), &|done, _| {
        progress((onnx_size + done).min(grand_total), grand_total);
    })?;
    ensure_onnxruntime_with_progress(&|done, _| {
        progress(
            (onnx_size + voices_size + done).min(grand_total),
            grand_total,
        );
    })?;
    progress(grand_total, grand_total);
    Ok(model)
}

/// Ensure ONLY the Kokoro voice-tensor packs (`voices-v1.0.bin`, ~28 MB) — the
/// portable `[510,256]` fp32 style packs — WITHOUT the ~310 MB ONNX model or the
/// onnxruntime dylib. This is the voice-tensor concern on its own: the apple-native
/// (Core ML / ANE) backend needs these packs (materialized per voice from this file)
/// but never the ONNX model or runtime, so they download independently of both.
/// Returns the voices file path.
pub fn run_setup_kokoro_voices_with_progress(
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    ensure_with_progress(&kokoro_voices_spec(), progress)
}
