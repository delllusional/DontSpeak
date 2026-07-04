//! Eager pre-download orchestrators: fetch a whole component's asset set
//! (Kokoro TTS / Parakeet STT + the shared onnxruntime dylib) and report a single
//! aggregate `(downloaded, total)` progress stream so the GUI shows one bar.

use std::path::PathBuf;

use crate::download::{ensure, ensure_with_progress};
use crate::model_path;
use crate::ort::{ensure_onnxruntime, ensure_onnxruntime_with_progress};
use crate::spec::{
    kokoro_files, kokoro_onnx_spec, kokoro_voices_spec, parakeet_decoder_spec, parakeet_dir,
    parakeet_encoder_spec, parakeet_files, parakeet_joiner_spec, parakeet_tokens_spec,
    sepformer_spec,
};

/// One asset fetch in a set: download a single file, streaming its bytes through the
/// callback the aggregator hands in. Boxed so a set's steps — the model files and the
/// shared onnxruntime archive, which have DIFFERENT fetchers ([`ensure_with_progress`]
/// vs [`ensure_onnxruntime_with_progress`]) — live in one uniform list.
///
/// `pub(crate)`: [`ort::ensure_cuda_runtime_with_progress`](crate::ort::ensure_cuda_runtime_with_progress)
/// reuses this SAME aggregator (one `DownloadStep` per CUDA wheel) instead of inventing a
/// second progress mechanism for the shared GPU runtime.
pub(crate) type DownloadStep = Box<dyn FnOnce(&dyn Fn(u64, u64)) -> std::io::Result<()>>;

/// Run a whole asset set as ONE monotonic `(downloaded, total)` stream through a SINGLE
/// callback path, reaching `total` EXACTLY when the last byte lands — "100% == downloaded"
/// (the dot then leaves the ring for its solid-orange warming state, decided separately by
/// the status machine; this function's job ends at bytes-on-disk).
///
/// `total` is the summed manifest size of the set. Each step reports its OWN file's live
/// bytes; the aggregator rolls the ACTUAL transferred bytes of every completed file (for
/// the onnxruntime step, the compressed-archive bytes — which equal that step's manifest
/// slice) into a running `base` (never the manifest estimate — so a file whose real size
/// differs from its estimate can't shift later files' baseline), clamps to `total`, and
/// never emits a value below the last one. The net effect: the bar can only ever move
/// forward. This is the ONE place `(done, total)` is synthesized for every asset set — the
/// per-file offset stitching that used to let the bar jump backward at file boundaries is
/// gone.
///
/// On a FULL fetch (fresh install — every file absent) this climbs smoothly 0→`total`. A
/// step whose asset is ALREADY present streams nothing (its fetcher returns early), so
/// `base` doesn't advance for it: a partial fetch (e.g. the shared onnxruntime dylib
/// already on disk from the sibling engine) tracks only the bytes it actually pulls and
/// then the forced final emit lands it on 100% — still monotonic, but the "missing" bytes
/// resolve as one step to full at the end rather than a steady climb.
pub(crate) fn run_download_set(
    progress: &dyn Fn(u64, u64),
    total: u64,
    steps: Vec<DownloadStep>,
) -> std::io::Result<()> {
    use std::cell::Cell;
    let base = Cell::new(0u64); // actual bytes summed from files already finished
    let cur = Cell::new(0u64); // max bytes seen for the file currently in flight
    let last = Cell::new(0u64); // last value emitted (the monotonic guard)
    for step in steps {
        cur.set(0);
        let emit = |done: u64, _total: u64| {
            if done > cur.get() {
                cur.set(done);
            }
            // Clamp to the total, then never go below what we've already shown.
            let v = (base.get() + done).min(total).max(last.get());
            last.set(v);
            progress(v, total);
        };
        step(&emit)?;
        // Roll THIS file's real byte count into the base for the next file.
        base.set((base.get() + cur.get()).min(total));
    }
    // The last byte has landed — report an exact 100% so "downloaded" is unambiguous.
    progress(total, total);
    Ok(())
}

/// Eager pre-download of the FULL Parakeet asset set: encoder, decoder, joiner,
/// tokens, AND the shared onnxruntime dylib (route A). Returns the model dir on success.
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

/// Like [`run_setup_parakeet`] but reports AGGREGATE byte-level progress across the whole
/// asset set (encoder + decoder + joiner + tokens + onnxruntime dylib) as a single,
/// monotonic `(downloaded, total)` stream via `run_download_set` — one bar that climbs
/// steadily 0→100% and hits 100% exactly when the last byte lands.
pub fn run_setup_parakeet_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let total: u64 = parakeet_files().iter().map(|f| f.size_bytes).sum();
    run_download_set(
        progress,
        total,
        vec![
            Box::new(|p| ensure_with_progress(&parakeet_encoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&parakeet_decoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&parakeet_joiner_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&parakeet_tokens_spec(), p).map(|_| ())),
            Box::new(|p| ensure_onnxruntime_with_progress(p).map(|_| ())),
        ],
    )?;
    parakeet_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
}

/// Eager pre-download of the FULL native-Kokoro asset set: the onnx model, the
/// voices file, AND the onnxruntime dylib (route A). The lazy caller
/// (`ds-helper` on first speak) uses [`crate::is_kokoro_present`] + these `ensure_*`
/// directly. Returns the model path on success.
pub fn run_setup_kokoro() -> std::io::Result<PathBuf> {
    let model = ensure(&kokoro_onnx_spec())?;
    ensure(&kokoro_voices_spec())?;
    ensure_onnxruntime()?;
    Ok(model)
}

/// Like [`run_setup_kokoro`] but reports AGGREGATE byte-level progress across the whole
/// asset set (onnx + voices + onnxruntime dylib) as a single, monotonic
/// `(downloaded, total)` stream via `run_download_set` — one "X MB of Y MB" bar that
/// advances steadily 0→100% and reaches 100% exactly when the last byte lands.
pub fn run_setup_kokoro_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let total: u64 = kokoro_files().iter().map(|f| f.size_bytes).sum();
    run_download_set(
        progress,
        total,
        vec![
            Box::new(|p| ensure_with_progress(&kokoro_onnx_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&kokoro_voices_spec(), p).map(|_| ())),
            Box::new(|p| ensure_onnxruntime_with_progress(p).map(|_| ())),
        ],
    )?;
    // The onnx model is the set's principal artifact — resolve its path for the caller.
    model_path(&kokoro_onnx_spec().file_name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
}

/// Eager pre-download of the SepFormer speaker-lock separator (`sepformer_int8.onnx`,
/// ~29 MB) AND the shared onnxruntime dylib it runs on, with the same aggregate
/// `(downloaded, total)` progress stream as its siblings. Returns the model path.
pub fn run_setup_sepformer_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let total = crate::urls::SEPFORMER.size_bytes
        + if crate::ort::onnxruntime_dist().is_some() {
            crate::urls::ONNXRUNTIME_DIST_SIZE_BYTES
        } else {
            0
        };
    run_download_set(
        progress,
        total,
        vec![
            Box::new(|p| ensure_with_progress(&sepformer_spec(), p).map(|_| ())),
            Box::new(|p| ensure_onnxruntime_with_progress(p).map(|_| ())),
        ],
    )?;
    model_path(&sepformer_spec().file_name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
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

#[cfg(test)]
mod tests {
    use super::{DownloadStep, run_download_set};
    use std::cell::RefCell;

    /// The whole point of the single-path aggregator: across a multi-file set the emitted
    /// `done` is NON-DECREASING, never exceeds `total`, and lands EXACTLY on `total` — even
    /// when a file's real bytes overshoot its manifest estimate (the case that used to make
    /// the old per-file offset math jump backward at a file boundary).
    #[test]
    fn download_set_is_monotonic_and_ends_at_total() {
        let seen = RefCell::new(Vec::<u64>::new());
        let record = |done: u64, total: u64| {
            assert_eq!(total, 100, "total is fixed for the whole set");
            seen.borrow_mut().push(done);
        };
        // Two files budgeted 60 + 40 = 100. File 1 actually streams 0..70 (OVERSHOOTS its
        // 60 estimate); file 2 then reports small values whose naive base would sit BELOW
        // where file 1 left the bar.
        let steps: Vec<DownloadStep> = vec![
            Box::new(|p| {
                p(0, 60);
                p(30, 60);
                p(70, 60); // real bytes exceed the estimate
                Ok(())
            }),
            Box::new(|p| {
                p(0, 40); // base is now 70 → 70+0 == last, held, not dropped
                p(5, 40);
                Ok(())
            }),
        ];
        run_download_set(&record, 100, steps).unwrap();
        let seen = seen.borrow();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert!(
            seen.iter().all(|&v| v <= 100),
            "never exceeds total: {seen:?}"
        );
        assert_eq!(
            *seen.last().unwrap(),
            100,
            "ends at exactly 100 (downloaded)"
        );
    }

    /// A single file that reports a value walking BACKWARD (e.g. a retry restarting the
    /// byte count from 0) must be clamped — the bar holds, it never regresses.
    #[test]
    fn download_set_clamps_a_backward_report() {
        let seen = RefCell::new(Vec::<u64>::new());
        let record = |done: u64, _t: u64| seen.borrow_mut().push(done);
        let steps: Vec<DownloadStep> = vec![Box::new(|p| {
            p(50, 100);
            p(20, 100); // backward (retry restart) — must be clamped up to 50
            p(60, 100);
            Ok(())
        })];
        run_download_set(&record, 100, steps).unwrap();
        let seen = seen.borrow();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert_eq!(*seen.last().unwrap(), 100);
    }

    /// A step whose asset is already present streams nothing (its fetcher returns without
    /// ever calling back); the set must still finish on an exact 100%.
    #[test]
    fn download_set_reaches_100_when_a_step_is_silent() {
        let seen = RefCell::new(Vec::<u64>::new());
        let record = |done: u64, _t: u64| seen.borrow_mut().push(done);
        let steps: Vec<DownloadStep> = vec![
            Box::new(|p| {
                p(40, 40);
                Ok(())
            }),
            Box::new(|_p| Ok(())), // already present: no callback at all
        ];
        run_download_set(&record, 100, steps).unwrap();
        let seen = seen.borrow();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert_eq!(*seen.last().unwrap(), 100);
    }
}
