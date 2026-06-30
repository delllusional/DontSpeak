//! Background model-download state + the auto-fetch / provider-apply orchestration.

use std::sync::{Arc, Mutex};

use ds_config::{Paths, VoiceConfig};
use ds_model::DownloadTarget;

use crate::config_gate::apple_native_shim_available;
use crate::logging::log;
use crate::tts::TtsManager;

/// Background model-download progress, polled via `model_status` so the app's
/// status dots can show an orange progress ring (downloading) and a red dot
/// (a failed download). `active_target` is `None` when idle, else the in-flight
/// [`DownloadTarget`] (e.g. `KokoroModel`/`Parakeet`/`All`); `last_error` is the
/// (target, message) of the most recent FAILED download, kept until a new download
/// for that target starts.
#[derive(Default)]
pub(crate) struct DownloadState {
    pub active_target: Option<DownloadTarget>,
    pub done: u64,
    pub total: u64,
    pub last_error: Option<(DownloadTarget, String)>,
    /// Warm-child reload hook, wired ONCE at boot via [`set_reload_hook`]: the warm-child
    /// owner plus the config paths. On a SUCCESSFUL download, [`start_download`] restarts the
    /// warm child iff it hosts the freshly-downloaded model (see [`download_needs_child_reload`])
    /// — the shared, cross-platform self-heal so a provider switch or a fresh install loads the
    /// new model WITHOUT a manual restart. Both `None` in tests / before boot wires them.
    pub warm: Option<Arc<TtsManager>>,
    pub paths: Option<Paths>,
}
pub(crate) type DownloadProg = Arc<Mutex<DownloadState>>;

/// Wire the warm-child reload hook (call ONCE at boot, after the warm-child owner exists).
/// Lets [`start_download`] restart the child to load a model that finished downloading after
/// the child was already started (a provider switch / fresh install) — the SHARED self-heal
/// used on every platform and by every download caller. See [`download_needs_child_reload`].
pub(crate) fn set_reload_hook(dl: &DownloadProg, warm: Arc<TtsManager>, paths: Paths) {
    let mut s = dl.lock().unwrap();
    s.warm = Some(warm);
    s.paths = Some(paths);
}

/// Map a completed download `target` to whether the WARM CHILD hosts a model it produced —
/// the pure core of [`download_needs_child_reload`], split out so it is unit-testable on ANY
/// host without building a platform-resolved `VoiceConfig`. The warm child hosts Kokoro TTS
/// and/or Parakeet STT; a `cuda` runtime fetch means whichever of those runs must restart to
/// bind the GPU execution provider. `diarization` (a separate Core ML path) and unknown
/// targets never touch the warm child.
fn target_hosts_engine(target: DownloadTarget, kokoro: bool, parakeet: bool) -> bool {
    match target {
        DownloadTarget::KokoroModel | DownloadTarget::KokoroVoices => kokoro,
        DownloadTarget::Parakeet => parakeet,
        DownloadTarget::All | DownloadTarget::Cuda => kokoro || parakeet,
        _ => false,
    }
}

/// Whether a just-COMPLETED download of `target` requires restarting the warm child so it
/// loads the freshly-arrived model(s). SHARED across platforms: the platform/provider
/// differences are already folded into `cfg.resolved_tts()` / `resolved_stt()`, so this
/// decision is identical everywhere — the only per-platform variance lives in those resolvers
/// (covered by their own tests). See [`target_hosts_engine`] for the pure mapping.
pub(crate) fn download_needs_child_reload(target: DownloadTarget, cfg: &VoiceConfig) -> bool {
    target_hosts_engine(
        target,
        cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro),
        cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn),
    )
}

/// Kick off a background download for `which` (e.g. [`DownloadTarget::KokoroModel`] /
/// [`DownloadTarget::Parakeet`] / [`DownloadTarget::All`]) unless one is already running.
/// Returns immediately; progress is observed via `model_status`. Each model setup also pulls
/// the shared onnxruntime dylib.
pub(crate) fn start_download(dl: &DownloadProg, which: DownloadTarget) {
    {
        let mut s = dl.lock().unwrap();
        if s.active_target.is_some() {
            return; // a download is already in flight
        }
        s.active_target = Some(which);
        s.done = 0;
        s.total = 0;
        // clear a prior failure for this target (or for "all")
        if let Some((t, _)) = &s.last_error
            && (*t == which || which == DownloadTarget::All || *t == DownloadTarget::All)
        {
            s.last_error = None;
        }
    }
    let dl = dl.clone();
    std::thread::spawn(move || {
        // Grab the warm-child reload hook up front (wired once at boot); used after the fetch.
        let (warm, paths) = {
            let s = dl.lock().unwrap();
            (s.warm.clone(), s.paths.clone())
        };
        let prog = |done: u64, total: u64| {
            let mut s = dl.lock().unwrap();
            s.done = done;
            s.total = total;
        };
        let result: std::io::Result<()> = (|| match which {
            DownloadTarget::KokoroModel => ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ()),
            // Voice-tensor pack only (~28 MB) — the ANE/Core ML path needs the voices npz
            // but not the 310 MB ONNX model. Requested by `EnsureKokoroVoices`.
            DownloadTarget::KokoroVoices => {
                ds_model::run_setup_kokoro_voices_with_progress(&prog).map(|_| ())
            }
            DownloadTarget::Parakeet => ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ()),
            // Shared GPU runtime (~1.4 GB) for the ONNX CUDA EP — drives BOTH engines. Folded
            // in here (not a bespoke thread in `apply_tts_provider`) so the completion hook
            // below restarts the warm child onto the GPU UNIFORMLY, exactly like a model fetch.
            DownloadTarget::Cuda => {
                #[cfg(all(
                    any(target_os = "windows", target_os = "linux"),
                    target_arch = "x86_64"
                ))]
                {
                    ds_model::ensure_cuda_runtime_with_progress(&prog).map(|_| ())
                }
                #[cfg(not(all(
                    any(target_os = "windows", target_os = "linux"),
                    target_arch = "x86_64"
                )))]
                {
                    Err(std::io::Error::other(
                        "cuda runtime is x86_64 windows/linux only",
                    ))
                }
            }
            // Diarization Core ML models — we fetch them OURSELVES (real %) into the dir the
            // shim loads from offline, like Kokoro/Parakeet. macOS-only (ANE shim).
            DownloadTarget::Diarization => {
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
            // "all" (and any other target without a bespoke fetch) ⇒ both ONNX models.
            _ => {
                ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ())?;
                ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ())
            }
        })();
        {
            let mut s = dl.lock().unwrap();
            s.active_target = None;
            if let Err(e) = &result {
                log(&format!(
                    "WARN: model download ({}) failed: {e}",
                    which.as_str()
                ));
                s.last_error = Some((which, e.to_string()));
            }
        }
        // SHARED self-heal (every platform, every caller): the warm child may have been started
        // BEFORE this model existed (a provider switch or a fresh install), so it couldn't load
        // it. Now that the fetch succeeded, restart the child so it picks the model up — no
        // manual restart needed. No-op when the child is stopped or the target isn't one it
        // hosts (`download_needs_child_reload`). Config is read LIVE so a mid-download config
        // change is honored.
        if result.is_ok()
            && let (Some(tts), Some(paths)) = (warm, paths)
            && download_needs_child_reload(which, &VoiceConfig::load(&paths))
            && tts.reload_models()
        {
            log(&format!(
                "warm child restarted to load freshly-downloaded '{}'",
                which.as_str()
            ));
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
) -> Option<DownloadTarget> {
    match (need_kokoro, need_kokoro_voices, need_parakeet) {
        (true, _, true) => Some(DownloadTarget::All),
        (true, _, false) => Some(DownloadTarget::KokoroModel),
        (false, true, _) => Some(DownloadTarget::KokoroVoices),
        (false, false, true) => Some(DownloadTarget::Parakeet),
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
    // Explicitly choosing CUDA while the GPU runtime is absent kicks off a one-time background
    // fetch via the SHARED `start_download` (target "cuda"); its completion hook then restarts
    // the warm child onto the GPU once the runtime lands — the SAME path a missing-model
    // download takes, so there is no bespoke per-platform restart here. `start_download`
    // single-flights, so a switch while a fetch is already running is a no-op. Platform-gated
    // because the CUDA runtime only exists on x86_64 Windows/Linux.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    if which.eq_ignore_ascii_case("ort_cuda") && !ds_model::cuda_runtime_present() {
        start_download(downloads, DownloadTarget::Cuda);
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    let _ = downloads;
}

#[cfg(test)]
mod tests {
    use super::{ane_needs_voices_npz, pick_download, target_hosts_engine};
    use ds_model::DownloadTarget;

    #[test]
    fn target_hosts_engine_maps_downloads_to_warm_child() {
        // The SHARED, platform-agnostic restart decision: given which engines the warm child
        // resolves to host (kokoro / parakeet booleans — the per-platform part lives in
        // `resolved_tts`/`resolved_stt`, tested separately), a completed download target maps
        // to "must restart the child" iff the child hosts a model that target produced.

        // Kokoro targets (full ONNX model + the voices-only pack) restart iff Kokoro TTS runs.
        assert!(target_hosts_engine(DownloadTarget::KokoroModel, true, false));
        assert!(target_hosts_engine(DownloadTarget::KokoroVoices, true, false));
        assert!(!target_hosts_engine(DownloadTarget::KokoroModel, false, true));
        assert!(!target_hosts_engine(DownloadTarget::KokoroVoices, false, false));

        // The Parakeet ONNX target restarts iff the built-in (Parakeet) STT runs.
        assert!(target_hosts_engine(DownloadTarget::Parakeet, false, true));
        assert!(!target_hosts_engine(DownloadTarget::Parakeet, true, false));

        // The combined model fetch AND the shared CUDA runtime restart iff EITHER engine runs —
        // both engines share the warm child and the compute provider.
        for t in [DownloadTarget::All, DownloadTarget::Cuda] {
            assert!(target_hosts_engine(t, true, false), "{t:?} (tts only)");
            assert!(target_hosts_engine(t, false, true), "{t:?} (stt only)");
            assert!(target_hosts_engine(t, true, true), "{t:?} (both)");
            assert!(!target_hosts_engine(t, false, false), "{t:?} (neither)");
        }

        // Diarization is a SEPARATE Core ML path (not the warm child); other non-hosting
        // targets (the bare runtime / installer groups) never trigger a restart even when
        // both engines run.
        for t in [
            DownloadTarget::Diarization,
            DownloadTarget::Onnx,
            DownloadTarget::Models,
        ] {
            assert!(!target_hosts_engine(t, true, true), "{t:?}");
        }
    }

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
        // must queue the voices-only fetch (`KokoroVoices`) instead of downloading nothing.
        assert_eq!(
            pick_download(false, true, false),
            Some(DownloadTarget::KokoroVoices)
        );
        // With Parakeet ONNX also missing, the small npz lands first (it gates the active
        // voice); Parakeet follows on the next poll tick.
        assert_eq!(
            pick_download(false, true, true),
            Some(DownloadTarget::KokoroVoices)
        );
        // Regression guards on the pre-existing mappings.
        assert_eq!(pick_download(false, false, false), None);
        assert_eq!(
            pick_download(true, false, false),
            Some(DownloadTarget::KokoroModel)
        );
        assert_eq!(pick_download(true, false, true), Some(DownloadTarget::All));
        assert_eq!(
            pick_download(false, false, true),
            Some(DownloadTarget::Parakeet)
        );
    }
}
