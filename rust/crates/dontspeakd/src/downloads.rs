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
            "kokoro_voices" => {
                ds_model::run_setup_kokoro_voices_with_progress(&prog).map(|_| ())
            }
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

/// Full-auto model fetch: when an engine is ENABLED but its (ONNX) model files are
/// missing, kick off the background download so first activation just works — there is no
/// manual Download button. Idempotent (file-presence gated here; [`start_download`] no-ops
/// if one is already in flight) and ONNX-only — the apple-native / system backends
/// self-manage their model caches. Called on startup, on every config reload, and on a
/// slow poll-loop tick (so a download that failed — e.g. no network at launch — retries
/// without any user action). Downloads both as `"all"` when both are missing.
pub(crate) fn auto_download_missing(downloads: &DownloadProg, cfg: &VoiceConfig) {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    // `uses_apple_native_model()` is an arch-BLIND config preference: the default provider
    // ladder resolves to ANE on ANY macOS (incl. Intel), so on its own it would skip the
    // ONNX fetch believing FluidAudio self-manages the cache. But ANE only actually serves
    // Kokoro when the shim dylib is present (`apple_native_shim_available`); without it — e.g.
    // Intel macOS, or a headless engine with no SMKOKORO_DYLIB_PATH — the warm child falls
    // back to the ONNX path and needs these files. Gate on the SAME runtime truth the status
    // / provider-token downgrade uses, so the model is fetched instead of silently skipped.
    let need_kokoro = cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro)
        && !(cfg.uses_apple_native_model() && apple_native_shim_available())
        && !(exists(ds_model::model_path(ds_model::KOKORO_ONNX_FILE))
            && exists(ds_model::model_path(
                ds_model::KOKORO_VOICES_FILE,
            ))
            && exists(ds_model::onnxruntime_dylib_path()));
    let need_parakeet = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        && matches!(
            cfg.resolved_stt_provider(),
            ds_config::Provider::OrtCpu | ds_config::Provider::OrtCuda
        )
        && !(exists(ds_model::model_path(
            ds_model::PARAKEET_ENCODER_FILE,
        )) && exists(ds_model::model_path(
            ds_model::PARAKEET_DECODER_FILE,
        )) && exists(ds_model::model_path(
            ds_model::PARAKEET_PREPROC_FILE,
        )) && exists(ds_model::model_path(
            ds_model::PARAKEET_VOCAB_FILE,
        )) && exists(ds_model::onnxruntime_dylib_path()));
    let which = match (need_kokoro, need_parakeet) {
        (true, true) => "all",
        (true, false) => "kokoro",
        (false, true) => "parakeet",
        (false, false) => return,
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
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
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
    #[cfg(not(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64")))]
    let _ = downloads;
}
