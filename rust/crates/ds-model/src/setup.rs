//! Eager pre-download orchestrators: fetch a whole component's asset set
//! (Kokoro TTS / Parakeet STT + the shared onnxruntime dylib) and report a single
//! aggregate `(downloaded, total)` progress stream so the GUI shows one bar.

use std::path::PathBuf;

use crate::download::{ensure, ensure_with_progress};
use crate::kokoro_frontend::{
    ensure_espeak_loader, ensure_espeak_loader_with_progress, ensure_japanese_dictionary,
    ensure_japanese_dictionary_with_progress,
};
use crate::model_path;
use crate::ort::{ensure_onnxruntime, ensure_onnxruntime_with_progress};
use crate::spec::{
    kokoro_files, kokoro_frontend_files, kokoro_g2p_decoder_spec, kokoro_g2p_encoder_spec,
    kokoro_onnx_spec, kokoro_voices_spec, parakeet_decoder_spec, parakeet_dir,
    parakeet_encoder_spec, parakeet_files, parakeet_joiner_spec, parakeet_tokens_spec,
    sepformer_spec,
};

/// One file fetch in a multi-step set (model + ORT fetchers share one list).
/// Also used by CUDA wheel download (same aggregator). `Send` so steps can run on the
/// multi-file worker pool ([`crate::parallel`]).
pub(crate) type DownloadStep =
    Box<dyn FnOnce(&dyn Fn(u64, u64)) -> std::io::Result<()> + Send>;

/// Monotonic aggregate `(done, total)` across steps. Uses actual transferred bytes (not
/// manifest estimates) so size drift can't regress the bar. Already-present steps stream
/// nothing; forced final emit still lands on 100%. Steps run with a bounded thread pool.
pub(crate) fn run_download_set(
    progress: &dyn Fn(u64, u64),
    total: u64,
    steps: Vec<DownloadStep>,
) -> std::io::Result<()> {
    crate::parallel::run_jobs_parallel(progress, total, 0, steps)
}

/// Full Parakeet set + shared ORT dylib. Returns model dir.
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

/// [`run_setup_parakeet`] with aggregate progress via `run_download_set`.
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

/// Full portable Kokoro set + ORT. Lazy path (`ds-helper`) uses presence + `ensure_*` instead.
pub fn run_setup_kokoro() -> std::io::Result<PathBuf> {
    let model = ensure(&kokoro_onnx_spec())?;
    ensure(&kokoro_voices_spec())?;
    ensure(&kokoro_g2p_encoder_spec())?;
    ensure(&kokoro_g2p_decoder_spec())?;
    ensure_espeak_loader()?;
    ensure_japanese_dictionary()?;
    ensure_onnxruntime()?;
    Ok(model)
}

/// [`run_setup_kokoro`] with aggregate progress via `run_download_set`.
pub fn run_setup_kokoro_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let total: u64 = kokoro_files().iter().map(|f| f.size_bytes).sum();
    run_download_set(
        progress,
        total,
        vec![
            Box::new(|p| ensure_with_progress(&kokoro_onnx_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&kokoro_voices_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&kokoro_g2p_encoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&kokoro_g2p_decoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_espeak_loader_with_progress(p).map(|_| ())),
            Box::new(|p| ensure_japanese_dictionary_with_progress(p).map(|_| ())),
            Box::new(|p| ensure_onnxruntime_with_progress(p).map(|_| ())),
        ],
    )?;
    model_path(&kokoro_onnx_spec().file_name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
}

/// SepFormer separator + shared ORT, with aggregate progress.
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

/// Shared Kokoro text frontend assets (English OOV, multilingual G2P, and ORT);
/// not synthesis weights or voices.
pub fn run_setup_kokoro_frontend_with_progress(
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let total: u64 = kokoro_frontend_files().iter().map(|f| f.size_bytes).sum();
    run_download_set(
        progress,
        total,
        vec![
            Box::new(|p| ensure_with_progress(&kokoro_g2p_encoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_with_progress(&kokoro_g2p_decoder_spec(), p).map(|_| ())),
            Box::new(|p| ensure_espeak_loader_with_progress(p).map(|_| ())),
            Box::new(|p| ensure_japanese_dictionary_with_progress(p).map(|_| ())),
            Box::new(|p| ensure_onnxruntime_with_progress(p).map(|_| ())),
        ],
    )?;
    model_path(&kokoro_g2p_encoder_spec().file_name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })
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
