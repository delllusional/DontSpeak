//! Background model-download state + the auto-fetch / provider-apply orchestration.

use std::sync::{Arc, Mutex};

use ds_config::VoiceConfig;

use crate::config_gate::apple_native_shim_available;
use crate::logging::log;
use crate::tts::TtsManager;

/// Background model-download progress, polled via `model_status` so the app's
/// status dots can show an orange progress ring (downloading) and a red dot
/// (a failed download). `active_target` is "" when idle, else the in-flight
/// "kokoro"|"parakeet"|"all"; `last_error` is the (target, message) of the most
/// recent FAILED download, kept until a new download for that target starts.
#[derive(Default)]
pub(crate) struct DownloadState {
    pub active_target: String,
    pub done: u64,
    pub total: u64,
    pub last_error: Option<(String, String)>,
}
pub(crate) type DownloadProg = Arc<Mutex<DownloadState>>;

/// Kick off a background download for `which` ("kokoro"|"parakeet"|"all") unless
/// one is already running. Returns immediately; progress is observed via
/// `model_status`. Each model setup also pulls the shared onnxruntime dylib.
pub(crate) fn start_download(dl: &DownloadProg, which: &str) {
    {
        let mut s = dl.lock().unwrap();
        if !s.active_target.is_empty() {
            return; // a download is already in flight
        }
        s.active_target = which.to_string();
        s.done = 0;
        s.total = 0;
        // clear a prior failure for this target (or for "all")
        if let Some((t, _)) = &s.last_error
            && (t == which || which == "all" || t == "all")
        {
            s.last_error = None;
        }
    }
    let dl = dl.clone();
    let which = which.to_string();
    std::thread::spawn(move || {
        let prog = |done: u64, total: u64| {
            let mut s = dl.lock().unwrap();
            s.done = done;
            s.total = total;
        };
        let result: std::io::Result<()> = (|| match which.as_str() {
            "kokoro" => ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ()),
            // Voice-tensor pack only (~28 MB) — the ANE/Core ML path needs the voices npz
            // but not the 310 MB ONNX model. Requested by `EnsureKokoroVoices`.
            "kokoro_voices" => ds_model::run_setup_kokoro_voices_with_progress(&prog).map(|_| ()),
            "parakeet" => ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ()),
            // Diarization Core ML models — we fetch them OURSELVES (real %) into the dir the
            // shim loads from offline, like Kokoro/Parakeet. macOS-only (ANE shim).
            "diarization" => {
                #[cfg(target_os = "macos")]
                {
                    ds_model::coreml_repo::ensure_coreml_repo(
                        &ds_model::coreml_repo::DIARIZER_COREML,
                        &prog,
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(std::io::Error::other("diarization is macOS-only"))
                }
            }
            _ => {
                ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ())?;
                ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ())
            }
        })();
        let mut s = dl.lock().unwrap();
        s.active_target = String::new();
        if let Err(e) = result {
            log(&format!("WARN: model download ({which}) failed: {e}"));
            s.last_error = Some((which, e.to_string()));
        }
    });
}

/// On the apple-native (ANE / Core ML) Kokoro path, FluidAudio self-manages its Core ML
/// model chain — but that chain ships only `af_heart.bin`. Every OTHER voice's style tensor
/// lives solely in the shared `voices-v1.0.bin` npz, from which `ds_tts::ane_voices::materialize`
/// extracts it on demand. So the npz must be fetched even on the ANE path (it isn't part of the
/// Core ML repo); otherwise a configured voice other than `af_heart` silently degrades to
/// `af_heart` at synth time. Pure, so the policy is unit-testable without touching the disk.
fn ane_needs_voices_npz(tts_is_kokoro: bool, ane_active: bool, voices_npz_present: bool) -> bool {
    tts_is_kokoro && ane_active && !voices_npz_present
}

/// The next single download target to kick from the computed needs (single-flight — later
/// targets land on subsequent poll ticks). The small voices-only npz is prioritized over
/// Parakeet since it gates the ACTIVE TTS voice. `None` ⇒ nothing missing. Pure/testable.
fn pick_download(
    need_kokoro: bool,
    need_kokoro_voices: bool,
    need_parakeet: bool,
) -> Option<&'static str> {
    match (need_kokoro, need_kokoro_voices, need_parakeet) {
        (true, _, true) => Some("all"),
        (true, _, false) => Some("kokoro"),
        (false, true, _) => Some("kokoro_voices"),
        (false, false, true) => Some("parakeet"),
        (false, false, false) => None,
    }
}

/// Full-auto model fetch: when an engine is ENABLED but a model file it needs is missing,
/// kick off the background download so first activation just works — there is no manual
/// Download button. Idempotent (file-presence gated here; [`start_download`] no-ops if one
/// is already in flight). Mostly ONNX models, plus the Kokoro voices npz on the ANE path
/// (see [`ane_needs_voices_npz`]); the rest of the apple-native / system caches self-manage.
/// Called on startup, on every config reload, and on a slow poll-loop tick (so a download
/// that failed — e.g. no network at launch — retries without any user action).
pub(crate) fn auto_download_missing(downloads: &DownloadProg, cfg: &VoiceConfig) {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    // `uses_apple_native_model()` is an arch-BLIND config preference: the default provider
    // ladder resolves to ANE on ANY macOS (incl. Intel), so on its own it would skip the
    // ONNX fetch believing FluidAudio self-manages the cache. But ANE only actually serves
    // Kokoro when the shim dylib is present (`apple_native_shim_available`); without it — e.g.
    // Intel macOS, or a headless engine with no SMKOKORO_DYLIB_PATH — the warm child falls
    // back to the ONNX path and needs these files. Gate on the SAME runtime truth the status
    // / provider-token downgrade uses, so the model is fetched instead of silently skipped.
    let tts_is_kokoro = cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro);
    let ane_active = cfg.uses_apple_native_model() && apple_native_shim_available();
    let need_kokoro = tts_is_kokoro
        && !ane_active
        && !(exists(ds_model::model_path(ds_model::KOKORO_ONNX_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE))
            && exists(ds_model::onnxruntime_dylib_path()));
    // EXCEPTION to "ANE self-manages its cache": the Core ML chain ships only `af_heart.bin`,
    // so the shared `voices-v1.0.bin` npz (the source for EVERY other voice) must still be
    // fetched on the ANE path — else any configured voice ≠ af_heart silently degrades to
    // af_heart at synth time (`synth_coreml` materializes from this npz, never downloads it).
    let need_kokoro_voices = ane_needs_voices_npz(
        tts_is_kokoro,
        ane_active,
        exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE)),
    );
    let need_parakeet = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        && matches!(
            cfg.resolved_stt_provider(),
            ds_config::Provider::OrtCpu | ds_config::Provider::OrtCuda
        )
        && !(exists(ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_DECODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_JOINER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_TOKENS_FILE))
            && exists(ds_model::onnxruntime_dylib_path()));
    let Some(which) = pick_download(need_kokoro, need_kokoro_voices, need_parakeet) else {
        return;
    };
    start_download(downloads, which);
}

/// Apply the persisted TTS execution-provider preference (`tts_provider`:
/// auto|ort_cpu|ort_cuda|ort_coreml|ane) to the warm child via [`TtsManager::set_provider`]. On
/// Windows, explicitly selecting CUDA while the GPU runtime is absent kicks off a one-time
/// background ~1.4 GB fetch (progress tracked in `downloads` under "cuda", so
/// model_status shows it), then restarts the warm child on the GPU once ready.
///
/// The `provider` setting is SHARED by both engines, so this one runtime serves Kokoro TTS
/// AND Parakeet STT — fetching it here aligns both onto the GPU after the restart. Matches
/// the canonical `ort_cuda` token ([`ds_config::Provider::as_str`], what config and
/// the `SetProvider` IPC both carry). `auto` is intentionally EXCLUDED: it uses the GPU
/// only when the runtime is already present and never pulls the large download silently.
pub(crate) fn apply_tts_provider(tts: &Arc<TtsManager>, downloads: &DownloadProg, which: &str) {
    tts.set_provider(which);
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    if which.eq_ignore_ascii_case("ort_cuda") && !ds_model::cuda_runtime_present() {
        {
            let mut s = downloads.lock().unwrap();
            if !s.active_target.is_empty() {
                return; // a download is already in flight
            }
            s.active_target = "cuda".to_string();
            s.done = 0;
            s.total = 0;
            s.last_error = None;
        }
        let tts = tts.clone();
        let dl = downloads.clone();
        // Restart on the SAME preference token once the runtime lands, so it resolves to
        // CUDA (resolve_provider matches `ort_cuda`/`auto`, not a bare `cuda`).
        let pref = which.to_string();
        std::thread::spawn(move || {
            let prog = |done: u64, total: u64| {
                let mut s = dl.lock().unwrap();
                s.done = done;
                s.total = total;
            };
            let r = ds_model::ensure_cuda_runtime_with_progress(&prog);
            {
                let mut s = dl.lock().unwrap();
                s.active_target = String::new();
                if let Err(e) = &r {
                    log(&format!("WARN: cuda runtime download failed: {e}"));
                    s.last_error = Some(("cuda".to_string(), e.to_string()));
                }
            }
            if r.is_ok() {
                log("cuda runtime ready — restarting warm child on GPU");
                // Restart so BOTH engines come up on the GPU (the child reloads Kokoro and
                // Parakeet, picking up DONTSPEAK_PROVIDER + DONTSPEAK_STT_PROVIDER fresh).
                tts.set_provider(&pref); // now resolves to CUDA → restart on GPU
            }
        });
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    let _ = downloads;
}

#[cfg(test)]
mod tests {
    use super::{ane_needs_voices_npz, pick_download};

    #[test]
    fn ane_path_still_needs_the_voices_npz() {
        // The crux: the apple-native (ANE / Core ML) Kokoro chain self-manages, but it ships
        // only af_heart. The shared voices npz (the source for every OTHER voice, e.g.
        // af_nicole) must STILL be fetched on the ANE path, or the chosen voice silently
        // falls back to af_heart at synth time.
        assert!(
            ane_needs_voices_npz(true, true, false),
            "ANE active + npz missing ⇒ must fetch the voices npz"
        );
        assert!(
            !ane_needs_voices_npz(true, true, true),
            "npz already present ⇒ nothing to fetch"
        );
        // ONNX path (ane_active=false): the npz rides along with the full ONNX `need_kokoro`
        // fetch, so the ANE-specific trigger must stay OFF to avoid a redundant download.
        assert!(
            !ane_needs_voices_npz(true, false, false),
            "ONNX path fetches the npz via need_kokoro, not this trigger"
        );
        // TTS isn't Kokoro at all ⇒ no Kokoro assets needed.
        assert!(
            !ane_needs_voices_npz(false, true, false),
            "non-Kokoro TTS needs no voices npz"
        );
    }

    #[test]
    fn ane_voices_npz_is_queued_not_skipped() {
        // Fresh ANE install: full ONNX Kokoro NOT needed, but the voices npz IS. The policy
        // must queue the voices-only fetch ("kokoro_voices") instead of downloading nothing.
        assert_eq!(pick_download(false, true, false), Some("kokoro_voices"));
        // With Parakeet ONNX also missing, the small npz lands first (it gates the active
        // voice); Parakeet follows on the next poll tick.
        assert_eq!(pick_download(false, true, true), Some("kokoro_voices"));
        // Regression guards on the pre-existing mappings.
        assert_eq!(pick_download(false, false, false), None);
        assert_eq!(pick_download(true, false, false), Some("kokoro"));
        assert_eq!(pick_download(true, false, true), Some("all"));
        assert_eq!(pick_download(false, false, true), Some("parakeet"));
    }
}
