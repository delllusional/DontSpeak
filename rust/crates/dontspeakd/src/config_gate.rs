//! Pure config predicates + reload-decision functions for the engine.
//!
//! Everything here is side-effect-light (`reconcile_helper_models` touches the
//! warm helper; the rest are pure) and unit-testable in isolation.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ds_config::VoiceConfig;
use ds_platform::Platform;
use ds_stt::Stt;

use crate::engine::PasteState;
use crate::tts;

/// §F physical-hold threshold default.
pub(crate) const DEFAULT_LONG_PRESS_MS: u64 = 600;

/// Normalize a configured `long_press_ms`: 0 means "use the default", any other
/// value is honored verbatim. Factored out so startup AND `Engine::reload` apply
/// it identically. PURE + unit-tested.
pub(crate) fn normalize_long_press(ms: u64) -> u64 {
    if ms == 0 { DEFAULT_LONG_PRESS_MS } else { ms }
}

/// Whether the Caps-Lock dictation loop should run. Gated solely by the
/// `caps_enabled` toggle. PURE + unit-tested.
pub(crate) fn caps_loop_enabled(cfg: &VoiceConfig) -> bool {
    cfg.caps_enabled
}

/// The warm helper must run when EITHER engine is in use — it hosts both Kokoro
/// (TTS) and Parakeet (STT). PURE.
pub(crate) fn helper_needed(cfg: &VoiceConfig) -> bool {
    helper_uses_tts(cfg) || helper_uses_stt(cfg)
}

/// Does the helper's Kokoro (TTS) model serve the current config? (Kokoro implies TTS on.)
/// Reads the RESOLVED engine — the first usable rung of the `tts_engine` ladder.
pub(crate) fn helper_uses_tts(cfg: &VoiceConfig) -> bool {
    cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro)
}

/// Does the helper serve local STT for the current config? Both LOCAL STT engines run
/// through the warm helper: Parakeet (ONNX/CPU or FluidAudio Core ML / ANE) and System
/// (macOS SpeechAnalyzer). ClaudeCode (Claude Code's own voice) and Off do not. Reads the
/// RESOLVED engine — the first usable rung of the `stt_engine` ladder.
pub(crate) fn helper_uses_stt(cfg: &VoiceConfig) -> bool {
    matches!(
        cfg.resolved_stt(),
        Some(ds_config::SttEngine::BuiltIn | ds_config::SttEngine::System)
    )
}

/// Is the apple-native (FluidAudio Core ML / ANE) backend usable right now? macOS +
/// the `libsmkokoro` shim dylib present (the app sets SMKOKORO_DYLIB_PATH). The shim
/// hosts BOTH the Kokoro TTS and the Parakeet STT backends, and FluidAudio
/// self-manages its model cache (downloads on first use), so this capability probe
/// is the right "present" gate for either apple-native engine — no on-disk model gate.
#[cfg(target_os = "macos")]
pub(crate) fn apple_native_shim_available() -> bool {
    std::env::var_os("SMKOKORO_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).exists())
        .unwrap_or(false)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn apple_native_shim_available() -> bool {
    false
}

/// Is the apple-native Parakeet STT backend usable right now? See
/// [`apple_native_shim_available`] (the shim hosts both STT and TTS).
pub(crate) fn parakeet_available() -> bool {
    apple_native_shim_available()
}

/// PROVIDER-AWARE Parakeet availability — the right gate for "can dictation run?".
/// The raw `ds_model::parakeet_present()` only knows the ONNX model FILES, so on the
/// ANE (FluidAudio Core ML) path — where those files are never downloaded — it wrongly
/// reports "missing" and blocks dictation even though Core ML is ready. This honors the
/// RESOLVED runtime: ONNX (CPU/CUDA) needs the downloaded files; ANE needs only the shim
/// (FluidAudio self-fetches its models). Use this at every Parakeet readiness gate.
pub(crate) fn parakeet_present_for(cfg: &VoiceConfig) -> bool {
    match cfg.resolved_stt_provider() {
        ds_config::Provider::OrtCpu | ds_config::Provider::OrtCuda => ds_model::parakeet_present(),
        ds_config::Provider::Ane => parakeet_available(),
        // `resolved_stt_provider()` only ever returns OrtCpu, OrtCuda, or Ane.
        _ => false,
    }
}


/// Is the System STT engine (macOS on-device `SFSpeechRecognizer`) usable right now?
/// Probes the shim WITHOUT prompting — authorized + on-device-capable + recognizer live.
/// False off macOS / without the shim. Drives both the build_stt gate and the
/// model_status `system` row.
pub(crate) fn system_stt_available() -> bool {
    ds_stt::system_available()
}

/// The local-STT backend token the warm helper should run, derived from the engine +
/// provider: `"system"` (SFSpeechRecognizer) when the System engine is selected, else
/// the resolved Parakeet runtime (`onnx`/`apple-native`). Carried to the helper via
/// `DONTSPEAK_STT_PROVIDER` (see [`tts::TtsManager::set_stt_provider_pref`]); System and
/// the Parakeet runtimes are mutually exclusive, so one token selects the backend.
pub(crate) fn helper_stt_provider(cfg: &VoiceConfig) -> &'static str {
    match cfg.resolved_stt() {
        Some(ds_config::SttEngine::System) => "system",
        _ => cfg.resolved_stt_provider().as_str(),
    }
}

/// Whether the Kokoro-TTS status row should read "present", per the ACTIVE backend
/// (mirrors the Parakeet STT row). apple-native gates on FluidAudio capability
/// (`shim`), since FluidAudio self-manages its Core ML cache; the ONNX providers gate
/// on the downloaded model+voices+runtime (`onnx_files`).
pub(crate) fn kokoro_present_for(apple_native: bool, shim: bool, onnx_files: bool) -> bool {
    if apple_native { shim } else { onnx_files }
}

/// Should the warm helper run in full-duplex AEC mode? Only when the user opted in
/// AND the helper is doing BOTH sides locally — Parakeet STT (we own the mic) and
/// Kokoro TTS (there is something to echo-cancel). With TTS off there is no echo to
/// cancel, so opening the echo-cancelled unit would seize the output device and
/// take the mic gain hit for nothing; with Claude Code STT, Claude Code owns the mic.
/// Works wherever `ds-aec` has a backend (macOS VPIO, Windows WASAPI Communications);
/// elsewhere the helper's `DuplexAudio::open()` fails and it degrades to half-duplex.
/// See docs/AEC.md and docs/FULL-DUPLEX-PORT.md.
pub(crate) fn full_duplex_wanted(cfg: &VoiceConfig) -> bool {
    // Parakeet-only: the AEC duplex path is wired for Parakeet capture; the System
    // (SFSpeechRecognizer) engine stays half-duplex (it owns its own recognition), so
    // gate on the Parakeet engine specifically rather than `helper_uses_stt` (which now
    // also covers System).
    cfg.full_duplex
        && cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        && helper_uses_tts(cfg)
}

/// Reconcile the warm helper's resident models with the config: eagerly LOAD the
/// model for each selected engine and UNLOAD the deselected one. This keeps a single
/// residency truth (the helper's `Option`s, mirrored in `tts_*_loaded`) that BOTH
/// the status-dot and the stats screen read — so "loaded" means the same thing
/// everywhere, a selected engine is resident before first use (Parakeet is
/// otherwise lazy), and a deselected model's RAM is reclaimed while the helper stays
/// warm for the other. No-op when the helper isn't running; when neither engine
/// needs it the helper is stopped elsewhere and all its memory goes with the process.
pub(crate) fn reconcile_helper_models(tts: &Arc<tts::TtsManager>, cfg: &VoiceConfig) {
    if !helper_needed(cfg) || !tts.is_running() {
        return;
    }
    if helper_uses_tts(cfg) {
        tts.load_engine("tts");
    } else {
        tts.unload_engine("tts");
    }
    if helper_uses_stt(cfg) {
        tts.load_engine("stt");
    } else {
        tts.unload_engine("stt");
    }
}

/// Build the dictation `Stt`: Parakeet now runs THROUGH the warm helper
/// (`HelperStt`) so the model isn't loaded in-process; everything else (the
/// ClaudeNative default, System) comes from the `ds-engines` factory. Falls back
/// to the factory when the helper isn't available (e.g. tests) or Parakeet isn't
/// present (the factory then degrades to ClaudeNative).
pub(crate) fn build_stt<P: Platform + 'static>(
    cfg: &VoiceConfig,
    plat: std::rc::Rc<P>,
    tts: Option<&Arc<tts::TtsManager>>,
    paste: &PasteState,
) -> Box<dyn Stt> {
    if let Some(tts) = tts {
        let local_available = match cfg.resolved_stt() {
            // Built-in Parakeet: provider-aware (ONNX files vs the ANE shim).
            Some(ds_config::SttEngine::BuiltIn) => parakeet_present_for(cfg),
            // System (SpeechAnalyzer): gated on the recognizer being authorized +
            // on-device-capable. When false, this falls through to the factory, which
            // returns the INERT SystemStt — NOT the ClaudeNative tap path (no silent fallback).
            Some(ds_config::SttEngine::System) => system_stt_available(),
            _ => false,
        };
        if local_available {
            return Box::new(crate::helper_stt::HelperStt::new(
                tts.clone(),
                paste.clone(),
            ));
        }
    }
    ds_engines::make_stt(cfg, plat)
}

/// §E.4 mtime-watch decision (PURE). Returns true iff settings.json should be
/// treated as changed since `last_seen`: a file that newly appeared OR whose
/// mtime advanced triggers a reload; a file that DISAPPEARED does NOT (we keep
/// the last-loaded config rather than reloading to defaults on a transient
/// stat/unlink). Equal mtimes never trigger. No disk, no clock.
pub(crate) fn should_reload_on_mtime(
    last_seen: Option<SystemTime>,
    current: Option<SystemTime>,
) -> bool {
    match current {
        Some(_) => current != last_seen,
        None => false,
    }
}

/// §E.4 debounce gate (PURE). Honor a reload trigger only if at least `window`
/// has elapsed since the last applied reload. No disk; the clock is the caller's
/// `Instant` so the test drives it deterministically.
pub(crate) fn debounce_ok(now: Instant, last_reload: Instant, window: Duration) -> bool {
    now.duration_since(last_reload) >= window
}

/// Read the config file's mtime, if it exists and stat succeeds. `None` for a
/// missing file or any stat error (the watcher treats None as "no change", not
/// "reload" — see `should_reload_on_mtime`).
pub(crate) fn config_mtime(config_toml: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(config_toml)
        .and_then(|m| m.modified())
        .ok()
}

/// §E.4 mtime watermark after a reload (PURE-ish; `stat_now` is the only side channel).
/// On a STAT-tick reload (`mtime_changed`) `current` is the value we just statted, so reuse
/// it. On a HUP-only reload (push watcher / SIGHUP / Reload RPC, which did NOT stat this
/// tick) `current` is stale (== `last_seen`), so take a fresh reading via `stat_now` —
/// otherwise the watermark stays behind the file, the ≤3 s stat backstop then sees a "new"
/// mtime and fires a SECOND redundant reload for the same edit. Tested via `stat_now` so the
/// disk read is injectable.
pub(crate) fn reload_watermark(
    mtime_changed: bool,
    current: Option<SystemTime>,
    stat_now: impl FnOnce() -> Option<SystemTime>,
) -> Option<SystemTime> {
    if mtime_changed { current } else { stat_now() }
}

/// GUARD: whether a TTS reply for the SELECTED engine can PLAY right now. `System` (macOS
/// `say`) needs no model → always ready; `Kokoro` plays only when its model is resident +
/// warm (`tts_loaded`); `Off` never plays. The worker uses this so a not-yet-downloaded /
/// still-loading model never produces silent or garbage playback. PURE.
pub(crate) fn tts_can_play(engine: ds_config::TtsEngine, tts_loaded: bool) -> bool {
    use ds_config::TtsEngine;
    match engine {
        TtsEngine::Off => false,
        TtsEngine::System => true,
        TtsEngine::Kokoro => tts_loaded,
    }
}

/// GUARD: whether dictation can START for the SELECTED STT engine. `BuiltIn` (Parakeet)
/// records only when its model is resident + warm (`stt_loaded`); `System` (OS recognizer)
/// only when its model is ready (`system_ready`); `ClaudeCode` delegates to Claude Code's own
/// dictation (no local model) → always startable; `Off` never dictates. The Caps start-tap
/// uses this so the dictation overlay never opens when STT can't actually transcribe. PURE.
pub(crate) fn stt_can_start(
    engine: ds_config::SttEngine,
    stt_loaded: bool,
    system_ready: bool,
) -> bool {
    use ds_config::SttEngine;
    match engine {
        SttEngine::Off => false,
        SttEngine::BuiltIn => stt_loaded,
        SttEngine::System => system_ready,
        SttEngine::ClaudeCode => true,
    }
}

pub(crate) fn debug_enabled() -> bool {
    std::env::var("DONTSPEAK_DEBUG").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_present_reflects_active_backend() {
        // apple-native: gated on the shim (FluidAudio capability), NOT the ONNX files —
        // so it reads present on a clean apple-native install with no ONNX models.
        assert!(kokoro_present_for(true, true, false));
        assert!(!kokoro_present_for(true, false, true));
        // onnx providers: gated on the downloaded ONNX model+voices+runtime, shim irrelevant.
        assert!(kokoro_present_for(false, false, true));
        assert!(!kokoro_present_for(false, true, false));
    }

    #[test]
    fn normalize_long_press_uses_default_on_zero() {
        assert_eq!(normalize_long_press(0), DEFAULT_LONG_PRESS_MS);
        assert_eq!(normalize_long_press(750), 750);
        assert_eq!(normalize_long_press(1), 1);
    }

    #[test]
    fn should_reload_on_mtime_decision_table() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(1001);
        // File appears (None -> Some): reload.
        assert!(should_reload_on_mtime(None, Some(t)));
        // Unchanged mtime: no reload.
        assert!(!should_reload_on_mtime(Some(t), Some(t)));
        // Newer mtime: reload.
        assert!(should_reload_on_mtime(Some(t), Some(t2)));
        // File disappears (Some -> None): NO reload (keep running config).
        assert!(!should_reload_on_mtime(Some(t), None));
        // Still missing (None -> None): no reload.
        assert!(!should_reload_on_mtime(None, None));
    }

    #[test]
    fn reload_watermark_takes_fresh_stat_only_on_hup_only_reload() {
        let stale = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let fresh = SystemTime::UNIX_EPOCH + Duration::from_secs(1001);

        // Stat-tick reload: `current` is already the fresh stat → reuse it, never re-stat.
        let mut statted = false;
        let r = reload_watermark(true, Some(fresh), || {
            statted = true;
            Some(stale)
        });
        assert_eq!(r, Some(fresh));
        assert!(!statted, "a stat-tick reload must not stat again");

        // Hup-only reload (push watcher / SIGHUP): `current` is stale (== last_seen), so a
        // fresh stat must advance the watermark — else the backstop fires a 2nd reload.
        let r = reload_watermark(false, Some(stale), || Some(fresh));
        assert_eq!(
            r,
            Some(fresh),
            "a hup-only reload must advance last_seen to the file's real mtime"
        );
    }

    #[test]
    fn debounce_ok_gates_on_window() {
        let base = Instant::now();
        let window = Duration::from_millis(250);
        // Within the window since the last reload: not yet allowed.
        assert!(!debounce_ok(
            base + Duration::from_millis(100),
            base,
            window
        ));
        // Exactly at the window boundary: allowed (>=).
        assert!(debounce_ok(base + window, base, window));
        // Well past the window: allowed.
        assert!(debounce_ok(base + Duration::from_millis(500), base, window));
    }

    #[test]
    fn tts_can_play_gates_on_engine_readiness() {
        use ds_config::TtsEngine;
        // Off never plays, regardless of the loaded flag.
        assert!(!tts_can_play(TtsEngine::Off, true));
        assert!(!tts_can_play(TtsEngine::Off, false));
        // System (macOS `say`) needs no model — always playable.
        assert!(tts_can_play(TtsEngine::System, false));
        assert!(tts_can_play(TtsEngine::System, true));
        // Kokoro plays ONLY when its model is resident + warm — never mid-download/load.
        assert!(!tts_can_play(TtsEngine::Kokoro, false));
        assert!(tts_can_play(TtsEngine::Kokoro, true));
    }

    #[test]
    fn helper_gates_read_the_resolved_engine() {
        use ds_config::{SttEngine, TtsEngine};
        let cfg = |tts: Vec<TtsEngine>, stt: Vec<SttEngine>| VoiceConfig {
            tts_engine: tts,
            stt_engine: stt,
            ..VoiceConfig::default()
        };
        // claude_code STT + an empty TTS ladder never use the warm helper, on EVERY platform.
        let c = cfg(Vec::new(), vec![SttEngine::ClaudeCode]);
        assert!(!helper_uses_tts(&c));
        assert!(!helper_uses_stt(&c));
        assert!(!helper_needed(&c));
        // `helper_stt_provider` is "system" ONLY when System resolves; claude_code → the
        // compute-provider token (never "system").
        assert_ne!(helper_stt_provider(&c), "system");
        // Full-duplex AEC needs a resolved built_in STT + Kokoro TTS — claude_code never qualifies.
        assert!(!full_duplex_wanted(&VoiceConfig {
            full_duplex: true,
            ..c.clone()
        }));

        // Where the on-device stack is usable, a built_in ladder DOES drive the helper + AEC.
        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        {
            let c2 = cfg(vec![TtsEngine::Kokoro], vec![SttEngine::BuiltIn]);
            assert!(helper_uses_tts(&c2));
            assert!(helper_uses_stt(&c2));
            assert!(helper_needed(&c2));
            assert!(full_duplex_wanted(&VoiceConfig {
                full_duplex: true,
                ..c2
            }));
        }
        // On x86_64 macOS a built_in-only ladder resolves to OFF (no usable rung) → no helper.
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            let c2 = cfg(vec![TtsEngine::Kokoro], vec![SttEngine::BuiltIn]);
            assert!(!helper_uses_tts(&c2));
            assert!(!helper_uses_stt(&c2));
            assert!(!helper_needed(&c2));
        }
    }

    #[test]
    fn stt_can_start_gates_on_engine_availability() {
        use ds_config::SttEngine;
        // Off never dictates.
        assert!(!stt_can_start(SttEngine::Off, true, true));
        // BuiltIn (Parakeet) records ONLY when its model is resident + warm.
        assert!(!stt_can_start(SttEngine::BuiltIn, false, true));
        assert!(stt_can_start(SttEngine::BuiltIn, true, false));
        // System (OS recognizer) only when its on-device model is ready.
        assert!(!stt_can_start(SttEngine::System, true, false));
        assert!(stt_can_start(SttEngine::System, false, true));
        // Claude Code delegates (no local model) — always startable, ignoring local flags.
        assert!(stt_can_start(SttEngine::ClaudeCode, false, false));
    }
}
