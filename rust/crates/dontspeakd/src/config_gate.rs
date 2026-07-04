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

/// Is the apple-native Parakeet STT backend usable right now? The shim dylib must be
/// loadable AND its Core ML model sets on disk (the ENGINE's download manager fetches them —
/// target `parakeet_coreml` — and FluidAudio only LOADS, enforceOffline; so "shim present"
/// alone no longer implies runnable). Presence uses the SAME revision-pinned completion
/// markers the downloader writes, so this gate and the fetch can never disagree.
pub(crate) fn parakeet_available() -> bool {
    apple_native_shim_available()
        && ds_model::coreml_repo::is_coreml_set_present(&ds_model::coreml_repo::PARAKEET_COREML_SET)
}

/// PROVIDER-AWARE Parakeet availability — the right gate for "can dictation run?".
/// The raw `ds_model::is_parakeet_present()` only knows the ONNX model FILES, so on the
/// ANE (FluidAudio Core ML) path — where those files are never downloaded — it wrongly
/// reports "missing" and blocks dictation even though Core ML is ready. This honors the
/// RESOLVED runtime: ONNX (CPU/CUDA) needs the downloaded ONNX files; ANE needs the shim
/// plus ITS downloaded Core ML sets. Use this at every Parakeet readiness gate.
pub(crate) fn parakeet_present_for(cfg: &VoiceConfig) -> bool {
    if stt_uses_onnx_runtime(cfg.resolved_stt_provider(), apple_native_shim_available()) {
        ds_model::is_parakeet_present() // ONNX-CPU/CUDA: needs the downloaded model FILES
    } else {
        parakeet_available() // genuine ANE: shim + its downloaded Core ML sets
    }
}

/// Does the built-in (Parakeet) STT run on the ONNX runtime (needs the downloaded model files)
/// rather than genuine ANE Core ML? PURE — the shared runtime-truth used by both
/// [`parakeet_present_for`] and `status.rs`'s `stt_uses_onnx`. `resolved_stt_provider()` returns
/// `Ane` arch-BLINDLY on ANY macOS, so ANE is only REAL when the FluidAudio shim is present;
/// without it (Intel, or no `SMKOKORO_DYLIB_PATH`) an `Ane` preference DOWNGRADES to ONNX-CPU.
/// `OrtCpu`/`OrtCuda` are always ONNX. Gating raw on `provider == Ane` (the old bug) checked Core
/// ML availability on Intel and reported Parakeet "missing" even with the ONNX files downloaded,
/// so `build_stt` degraded dictation to the Claude Code fallback though Parakeet ran on CPU.
pub(crate) fn stt_uses_onnx_runtime(provider: ds_config::Provider, shim_available: bool) -> bool {
    !(provider == ds_config::Provider::Ane && shim_available)
}

/// Whether TTS runs the GENUINE apple-native (FluidAudio Core ML / ANE) Kokoro path right now:
/// the arch-blind `uses_apple_native_model()` preference AND the shim actually present. The TTS
/// twin of `!stt_uses_onnx_runtime(...)`; the single definition shared by the download gate
/// (`auto_download_missing`) and the status Kokoro row so they can't drift.
pub(crate) fn apple_native_tts_active(cfg: &VoiceConfig) -> bool {
    cfg.uses_apple_native_model() && apple_native_shim_available()
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
/// (mirrors the Parakeet STT row). `files` is the ACTIVE backend's on-disk model gate —
/// the downloaded Core ML sets (revision-pinned completion markers) on the apple-native
/// path, the downloaded model+voices+runtime on the ONNX providers. apple-native
/// additionally requires the shim dylib (the loader).
pub(crate) fn kokoro_present_for(apple_native: bool, shim: bool, files: bool) -> bool {
    if apple_native { shim && files } else { files }
}

/// Should the warm helper run in full-duplex AEC mode? Only when the user opted in
/// AND the helper is doing BOTH sides locally — Parakeet STT (we own the mic) and
/// Kokoro TTS (there is something to echo-cancel). With TTS off there is no echo to
/// cancel, so opening the echo-cancelled unit would seize the output device and
/// take the mic gain hit for nothing; with Claude Code STT, Claude Code owns the mic.
/// Works wherever `ds-aec` has a backend (macOS VPIO, Windows WASAPI Communications);
/// elsewhere the helper's `DuplexAudio::open()` fails and it degrades to half-duplex.
/// See docs/AEC.md.
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
/// (`HelperStt`) so the model isn't loaded in-process; everything else (ClaudeCode,
/// System) comes from the `ds-engines` factory. Falls back to the factory when the
/// helper isn't available (e.g. tests) or Parakeet isn't present — the factory has
/// NO silent substitution: BuiltIn with no local model degrades to the same inert
/// placeholder `None`/off uses, never to ClaudeCode.
pub(crate) fn build_stt<P: Platform + 'static>(
    cfg: &VoiceConfig,
    plat: std::rc::Rc<P>,
    tts: Option<&Arc<tts::TtsManager>>,
    paste: &PasteState,
) -> Box<dyn Stt> {
    if let Some(tts) = tts
        && local_stt_available(cfg)
    {
        crate::logging::log(&format!(
            "dictation STT = local helper ({} / {})",
            cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off"),
            cfg.resolved_stt_provider().as_str()
        ));
        return Box::new(crate::helper_stt::HelperStt::new(
            tts.clone(),
            paste.clone(),
        ));
    }
    crate::logging::log(&format!(
        "dictation STT = factory fallback (resolved={}) — NOT the local helper",
        cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off")
    ));
    ds_engines::make_stt(cfg, plat)
}

/// Whether the SELECTED STT engine can run LOCALLY right now (Parakeet model resident, or
/// System recognizer authorized) — i.e. whether `build_stt` uses the warm-helper path vs the
/// `ds-engines` factory (which, for BuiltIn with no local model, is now INERT — no silent
/// substitution to ClaudeCode). Shared by `build_stt` AND `Daemon::reload`: a fresh-install
/// model download flips this true WITHOUT changing the engine SELECTION, so reload must
/// rebuild `self.stt` when THIS changes, else dictation stays inert though the model is now
/// present + loaded. PURE-ish (reads model presence / recognizer state), NOT a config-only diff.
pub(crate) fn local_stt_available(cfg: &VoiceConfig) -> bool {
    match cfg.resolved_stt() {
        // Built-in Parakeet: provider-aware (ONNX files vs the ANE shim).
        Some(ds_config::SttEngine::BuiltIn) => parakeet_present_for(cfg),
        // System (Apple's on-device recognizer): authorized + on-device-capable. When false, `build_stt`
        // falls to the INERT SystemStt — NOT the ClaudeNative tap (no silent fallback).
        Some(ds_config::SttEngine::System) => system_stt_available(),
        _ => false,
    }
}

/// Whether a REFUSED dictation start (a Caps tap while [`stt_can_start`] says the engine
/// can't transcribe yet) should surface the refusal cue (`dictation.refused` → the overlay's
/// warning wash). Only the engines with a RUNTIME readiness gate qualify — BuiltIn (model
/// missing / downloading / warm helper loading) and System (recognizer not ready): there the
/// user asked for dictation and got nothing, the fresh-install silent-no-op trap. ClaudeCode
/// is always startable (never refused), and `None` means dictation is deliberately
/// disabled — that tap is the documented pause/resume gesture, not an error. PURE.
pub(crate) fn refusal_cue_on_refused_start(resolved: Option<ds_config::SttEngine>) -> bool {
    matches!(
        resolved,
        Some(ds_config::SttEngine::BuiltIn | ds_config::SttEngine::System)
    )
}

/// §E.4 reload-tick decision (PURE): a TRAILING-EDGE debounce — apply a config change
/// only once `window` has passed with NO NEW trigger, rather than merely `window` since
/// the last reload that actually ran. Every `set_config` write fires TWO triggers for the
/// SAME edit: an immediate IPC `Reload` nudge (the fast path) and a slower filesystem-watch
/// event for the same write (the fallback for a hand-edit, or a missed/late nudge — see
/// `config_watch`). A LEADING-edge cooldown (a fixed gap since the last reload that ran)
/// lets a straggling watch event that arrives just past the window through as if it were
/// an independent, brand new change — which is exactly what fired a second, redundant
/// reload (and a second warm-child restart) ~581 ms after the nudge had already applied
/// the identical edit. Resetting the window on EVERY trigger — not just the first —
/// collapses the whole burst into ONE `Run`, at the cost of landing `window` after the
/// LAST trigger rather than the first (still comfortably sub-second).
///
/// `pending_since` is `None` when nothing is outstanding, else the tick a trigger was
/// first (or most recently) observed. The caller threads the returned `Option<Instant>`
/// into the next tick's `pending_since` — it's the memory `Defer`/`Run` needs instead of
/// re-arming `reload_requested` (a fresh trigger is folded into this state immediately, so
/// nothing is lost even though the poll loop already swapped the flag back to false).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ReloadTick {
    /// Apply the reload now — `window` has elapsed since the most recent trigger.
    Run,
    /// A trigger is outstanding (this tick's, or an earlier tick's) but the quiet window
    /// hasn't elapsed yet.
    Defer,
    /// Nothing outstanding.
    Idle,
}
pub(crate) fn reload_tick(
    triggered_now: bool,
    now: Instant,
    pending_since: Option<Instant>,
    window: Duration,
) -> (ReloadTick, Option<Instant>) {
    // A fresh trigger ALWAYS resets the quiet window, even over an already-pending one —
    // that's what lets a straggling second trigger for the same edit push the reload out
    // instead of racing in as its own separate `Run`.
    let anchor = if triggered_now {
        Some(now)
    } else {
        pending_since
    };
    match anchor {
        None => (ReloadTick::Idle, None),
        Some(t) if now.duration_since(t) >= window => (ReloadTick::Run, None),
        Some(t) => (ReloadTick::Defer, Some(t)),
    }
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
/// warm (`tts_loaded`); `None` (TTS off) never plays. The worker uses this so a
/// not-yet-downloaded / still-loading model never produces silent or garbage playback. PURE.
pub(crate) fn tts_can_play(engine: Option<ds_config::TtsEngine>, tts_loaded: bool) -> bool {
    use ds_config::TtsEngine;
    match engine {
        None => false,
        Some(TtsEngine::System) => true,
        Some(TtsEngine::Kokoro) => tts_loaded,
    }
}

/// What [`TtsManager::restart_if_crashed`](crate::tts::TtsManager::restart_if_crashed) may
/// do for a Kokoro item the [`tts_can_play`] guard is about to drop.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum HealAction {
    /// Leave the child alone (alive and loading, or its last start already failed).
    Nothing,
    /// The child EXITED but still occupies the slot — reap it, then start a fresh one.
    ReapAndStart,
    /// The slot is empty with no recorded start failure — start a fresh child.
    Start,
}

/// GUARD: the self-heal decision for a warm child that isn't serving, from its OBSERVED
/// state. A child that DIED post-READY (crash: AV false-positive, OOM, GPU driver) is the
/// case this exists for — nothing else respawns it: `mark_dead` counts on "the next speak
/// restarts it", but the [`tts_can_play`] guard drops that very speak, wedging BOTH models
/// in "Starting" until an app restart.
/// * present + exited  → [`HealAction::ReapAndStart`] — the crash went unreaped.
/// * absent, no error  → [`HealAction::Start`] — an io error already reaped the crash.
/// * present + alive   → [`HealAction::Nothing`] — it's starting/loading; let it finish.
/// * absent + error    → [`HealAction::Nothing`] — the last START failed (model missing /
///   bad dylib); a retry would fail identically, and the download-completion hook owns
///   that recovery (`downloads`' reload-on-fetch).
///
/// PURE.
pub(crate) fn warm_child_heal_action(
    child_present: bool,
    child_exited: bool,
    start_error: bool,
) -> HealAction {
    match (child_present, child_exited, start_error) {
        (true, true, _) => HealAction::ReapAndStart,
        (true, false, _) => HealAction::Nothing,
        (false, _, false) => HealAction::Start,
        (false, _, true) => HealAction::Nothing,
    }
}

/// GUARD: whether dictation can START for the SELECTED STT engine. `BuiltIn` (Parakeet)
/// records only when its model is resident + warm (`stt_loaded`); `System` (OS recognizer)
/// only when its model is ready (`system_ready`); `ClaudeCode` delegates to Claude Code's own
/// dictation (no local model) → always startable; `None` (dictation off) never dictates. The
/// Caps start-tap uses this so the dictation overlay never opens when STT can't actually
/// transcribe. PURE.
pub(crate) fn stt_can_start(
    engine: Option<ds_config::SttEngine>,
    stt_loaded: bool,
    system_ready: bool,
) -> bool {
    use ds_config::SttEngine;
    match engine {
        None => false,
        Some(SttEngine::BuiltIn) => stt_loaded,
        Some(SttEngine::System) => system_ready,
        Some(SttEngine::ClaudeCode) => true,
    }
}

pub(crate) fn debug_enabled() -> bool {
    std::env::var("DONTSPEAK_DEBUG").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_uses_onnx_runtime_gates_on_the_shim_not_the_raw_provider() {
        use ds_config::Provider;
        // Genuine ANE: `Ane` preference AND the FluidAudio shim present ⇒ Core ML path
        // (NOT ONNX) ⇒ `parakeet_present_for` checks `parakeet_available()`, not the files.
        assert!(!stt_uses_onnx_runtime(Provider::Ane, true));
        // The Intel (and any no-shim) case: `resolved_stt_provider()` still says `Ane`
        // arch-blindly, but with no shim it DOWNGRADES to ONNX-CPU ⇒ needs the model FILES.
        // This is the regression that made dictation fall back to Claude Code on Intel.
        assert!(stt_uses_onnx_runtime(Provider::Ane, false));
        // Explicit ONNX providers are always the ONNX path, shim or not.
        assert!(stt_uses_onnx_runtime(Provider::OrtCpu, false));
        assert!(stt_uses_onnx_runtime(Provider::OrtCpu, true));
        assert!(stt_uses_onnx_runtime(Provider::OrtCuda, false));
        assert!(stt_uses_onnx_runtime(Provider::OrtCuda, true));
    }

    #[test]
    fn kokoro_present_reflects_active_backend() {
        // apple-native: needs the shim (the loader) AND the downloaded Core ML sets — a clean
        // apple-native install with the sets not yet fetched reads MISSING (the download
        // manager then lights "downloading"), never a false "present".
        assert!(kokoro_present_for(true, true, true));
        assert!(!kokoro_present_for(true, true, false));
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
    fn caps_loop_enabled_mirrors_the_toggle() {
        let cfg = |caps_enabled: bool| VoiceConfig {
            caps_enabled,
            ..VoiceConfig::default()
        };
        assert!(caps_loop_enabled(&cfg(true)));
        assert!(!caps_loop_enabled(&cfg(false)));
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
    fn reload_tick_idle_when_nothing_triggered() {
        let now = Instant::now();
        assert_eq!(
            reload_tick(false, now, None, Duration::from_millis(250)),
            (ReloadTick::Idle, None)
        );
        // A stale pending timestamp with no fresh trigger this tick still just waits —
        // covered by the defer/run tests below, not idle (idle is strictly "nothing at all").
    }

    #[test]
    fn reload_tick_defers_then_runs_once_the_quiet_window_elapses() {
        let base = Instant::now();
        let window = Duration::from_millis(250);

        // First trigger: nothing was pending, so it becomes the anchor and defers.
        let (tick, pending) = reload_tick(true, base, None, window);
        assert_eq!(tick, ReloadTick::Defer);
        assert_eq!(pending, Some(base));

        // No new trigger, window not yet elapsed: keep deferring off the SAME anchor.
        let (tick, pending) =
            reload_tick(false, base + Duration::from_millis(100), pending, window);
        assert_eq!(tick, ReloadTick::Defer);
        assert_eq!(pending, Some(base));

        // No new trigger, window fully elapsed: run, and the pending state clears.
        let (tick, pending) = reload_tick(false, base + window, pending, window);
        assert_eq!(tick, ReloadTick::Run);
        assert_eq!(pending, None);
    }

    #[test]
    fn reload_tick_resets_the_window_on_a_second_trigger() {
        // Regression for the real incident: an explicit IPC nudge fires, then a slower
        // filesystem-watch event for the SAME edit lands 581 ms later — well past a
        // 250 ms window, but the trailing-edge reset still collapses both into ONE run
        // instead of letting the straggler race in as its own independent reload.
        let base = Instant::now();
        let window = Duration::from_millis(750);
        let straggler_gap = Duration::from_millis(581);

        // The explicit nudge: first trigger, defers.
        let (tick, pending) = reload_tick(true, base, None, window);
        assert_eq!(tick, ReloadTick::Defer);

        // The straggling watch event for the SAME write, 581ms later: still a fresh
        // trigger, so the anchor moves forward instead of running here.
        let straggler_at = base + straggler_gap;
        let (tick, pending) = reload_tick(true, straggler_at, pending, window);
        assert_eq!(tick, ReloadTick::Defer);
        assert_eq!(
            pending,
            Some(straggler_at),
            "anchor re-armed on the second trigger"
        );

        // Nothing new, window elapsed from the SECOND trigger (not the first): exactly
        // one `Run` for the whole burst.
        let (tick, pending) = reload_tick(false, straggler_at + window, pending, window);
        assert_eq!(tick, ReloadTick::Run);
        assert_eq!(pending, None);

        // Sanity: had the debounce been anchored on the FIRST trigger only (the old
        // leading-edge bug), `base + window` would already be past — window is 750ms and
        // the straggler landed at 581ms, i.e. still inside it — so this single window is
        // what proves the straggler didn't just sail through as an independent reload.
        assert!(straggler_gap < window);
    }

    #[test]
    fn reload_tick_idle_pending_none_does_not_spuriously_run() {
        // No trigger, nothing pending: idle forever, regardless of how much time passes.
        let base = Instant::now();
        let window = Duration::from_millis(250);
        assert_eq!(
            reload_tick(false, base + Duration::from_secs(10), None, window),
            (ReloadTick::Idle, None)
        );
    }

    #[test]
    fn tts_can_play_gates_on_engine_readiness() {
        use ds_config::TtsEngine;
        // None (off) never plays, regardless of the loaded flag.
        assert!(!tts_can_play(None, true));
        assert!(!tts_can_play(None, false));
        // System (macOS `say`) needs no model — always playable.
        assert!(tts_can_play(Some(TtsEngine::System), false));
        assert!(tts_can_play(Some(TtsEngine::System), true));
        // Kokoro plays ONLY when its model is resident + warm — never mid-download/load.
        assert!(!tts_can_play(Some(TtsEngine::Kokoro), false));
        assert!(tts_can_play(Some(TtsEngine::Kokoro), true));
    }

    #[test]
    fn helper_gates_read_the_resolved_engine() {
        use ds_config::{SttEngine, TtsEngine};
        let cfg = |tts: Vec<TtsEngine>, stt: Vec<SttEngine>| VoiceConfig {
            tts_engine_ladder: tts,
            stt_engine_ladder: stt,
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

        // A built_in ladder drives the helper + AEC wherever the on-device stack resolves — every
        // platform EXCEPT Intel macOS with NO onnxruntime dylib, where built_in is unusable and the
        // ladder falls through to off (no helper). With the Homebrew keg / ORT_DYLIB_PATH present,
        // Intel macOS resolves to built_in like everywhere else (see
        // `ds_config::intel_mac_builtin_ort_available`).
        let c2 = cfg(vec![TtsEngine::Kokoro], vec![SttEngine::BuiltIn]);
        let builtin_off = cfg!(all(target_os = "macos", target_arch = "x86_64"))
            && !ds_config::intel_mac_builtin_ort_available();
        if builtin_off {
            assert!(!helper_uses_tts(&c2));
            assert!(!helper_uses_stt(&c2));
            assert!(!helper_needed(&c2));
        } else {
            assert!(helper_uses_tts(&c2));
            assert!(helper_uses_stt(&c2));
            assert!(helper_needed(&c2));
            assert!(full_duplex_wanted(&VoiceConfig {
                full_duplex: true,
                ..c2
            }));
        }
    }

    #[test]
    fn warm_child_heal_restarts_only_a_dead_child() {
        use HealAction::*;
        // Present but EXITED: a post-READY crash still holding the slot — reap + restart
        // (with or without a stale error; a dead child always warrants the restart).
        assert_eq!(warm_child_heal_action(true, true, false), ReapAndStart);
        assert_eq!(warm_child_heal_action(true, true, true), ReapAndStart);
        // Alive: it's starting/loading — never restart from under it.
        assert_eq!(warm_child_heal_action(true, false, false), Nothing);
        assert_eq!(warm_child_heal_action(true, false, true), Nothing);
        // Slot empty, no start failure: an io error already reaped the crash — start.
        assert_eq!(warm_child_heal_action(false, false, false), Start);
        // Slot empty after a FAILED start (model missing / bad dylib): a retry would fail
        // identically — that recovery belongs to the download-completion hook.
        assert_eq!(warm_child_heal_action(false, false, true), Nothing);
    }

    #[test]
    fn stt_can_start_gates_on_engine_availability() {
        use ds_config::SttEngine;
        // None (off) never dictates.
        assert!(!stt_can_start(None, true, true));
        // BuiltIn (Parakeet) records ONLY when its model is resident + warm.
        assert!(!stt_can_start(Some(SttEngine::BuiltIn), false, true));
        assert!(stt_can_start(Some(SttEngine::BuiltIn), true, false));
        // System (OS recognizer) only when its on-device model is ready.
        assert!(!stt_can_start(Some(SttEngine::System), true, false));
        assert!(stt_can_start(Some(SttEngine::System), false, true));
        // Claude Code delegates (no local model) — always startable, ignoring local flags.
        assert!(stt_can_start(Some(SttEngine::ClaudeCode), false, false));
    }

    /// The refusal cue fires ONLY for the engines with a runtime readiness gate: a refused
    /// BuiltIn/System start is the silent-no-op trap (model downloading on a fresh
    /// install), while None (dictation deliberately off — the tap is the pause/resume
    /// gesture) and ClaudeCode (always startable, so never actually refused) stay quiet.
    #[test]
    fn refusal_cue_only_for_runtime_gated_engines() {
        use ds_config::SttEngine;
        assert!(refusal_cue_on_refused_start(Some(SttEngine::BuiltIn)));
        assert!(refusal_cue_on_refused_start(Some(SttEngine::System)));
        assert!(!refusal_cue_on_refused_start(Some(SttEngine::ClaudeCode)));
        assert!(!refusal_cue_on_refused_start(None));
    }
}
