//! Pure config predicates + reload decisions.
//! (`reconcile_helper_models` touches the warm helper; rest pure.)

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ds_config::VoiceConfig;
use ds_platform::Platform;
use ds_stt::Stt;

use crate::engine::PasteState;
use crate::tts;

pub(crate) const DEFAULT_LONG_PRESS_MS: u64 = 600;

/// `0` → default. Shared by startup and reload.
pub(crate) fn normalize_long_press(ms: u64) -> u64 {
    if ms == 0 { DEFAULT_LONG_PRESS_MS } else { ms }
}

pub(crate) fn caps_loop_enabled(cfg: &VoiceConfig) -> bool {
    cfg.caps
}

pub(crate) fn helper_needed(cfg: &VoiceConfig) -> bool {
    helper_uses_tts(cfg) || helper_uses_stt(cfg)
}

/// Built-in TTS via the warm helper.
pub(crate) fn helper_uses_tts(cfg: &VoiceConfig) -> bool {
    cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
}

/// BuiltIn or System STT via warm helper.
pub(crate) fn helper_uses_stt(cfg: &VoiceConfig) -> bool {
    matches!(
        cfg.resolved_stt(),
        Some(ds_config::SttEngine::BuiltIn | ds_config::SttEngine::System)
    )
}

/// macOS + `DONTSPEAK_MLX_DYLIB_PATH`. Pair with model-asset gates before advertising.
#[cfg(target_os = "macos")]
pub(crate) fn mlx_shim_available() -> bool {
    std::env::var_os("DONTSPEAK_MLX_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).exists())
        .unwrap_or(false)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn mlx_shim_available() -> bool {
    false
}

/// Shim + MLX sets (same revision markers as downloader).
pub(crate) fn parakeet_available() -> bool {
    mlx_shim_available()
        && ds_model::mlx_repo::is_mlx_set_present(&ds_model::mlx_repo::PARAKEET_MLX_SET)
}

/// Provider-aware Parakeet readiness: ONNX files versus shim plus MLX assets.
pub(crate) fn parakeet_present_for(cfg: &VoiceConfig) -> bool {
    if stt_uses_onnx_runtime(cfg.resolved_stt_provider(), mlx_shim_available()) {
        ds_model::is_parakeet_present() // ONNX-CPU/CUDA: needs the downloaded model FILES
    } else {
        parakeet_available() // MLX: shim + its downloaded model set
    }
}

/// Built-in STT on ONNX (needs model files) vs native MLX.
/// real only when shim present; else downgrades to ONNX-CPU. Shared by presence + status.
pub(crate) fn stt_uses_onnx_runtime(provider: ds_config::Provider, shim_available: bool) -> bool {
    !(provider == ds_config::Provider::Mlx && shim_available)
}

/// Genuine MLX TTS: supported model and platform plus the loaded shim.
pub(crate) fn mlx_tts_active(cfg: &VoiceConfig) -> bool {
    cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
        && tts_uses_mlx_runtime(cfg.resolved_tts_provider(), mlx_shim_available())
}

fn tts_uses_mlx_runtime(provider: ds_config::Provider, shim_available: bool) -> bool {
    provider == ds_config::Provider::Mlx && shim_available
}

/// System STT usable (probe, no prompt). `build_stt` + status row.
pub(crate) fn system_stt_available() -> bool {
    ds_stt::system_available()
}

/// System selected but not Ready — authorize on boot/reload once per transition
/// (skip every reload when already Ready; Preparing/Unavailable still try).
pub(crate) fn system_stt_needs_authorization(cfg: &VoiceConfig) -> bool {
    cfg.resolved_stt() == Some(ds_config::SttEngine::System)
        && ds_stt::system_state() != ds_stt::SystemState::Ready
}

/// Helper STT token: `"system"` or resolved Parakeet runtime (`DONTSPEAK_STT_PROVIDER`).
pub(crate) fn helper_stt_provider(cfg: &VoiceConfig) -> &'static str {
    match cfg.resolved_stt() {
        Some(ds_config::SttEngine::System) => "system",
        _ => cfg.resolved_stt_provider().as_str(),
    }
}

/// Cheap shared-frontend presence gate used by both Kokoro synthesis backends.
pub(crate) fn kokoro_g2p_files_present() -> bool {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    exists(ds_model::model_path(ds_model::KOKORO_G2P_ENCODER_FILE))
        && exists(ds_model::model_path(ds_model::KOKORO_G2P_DECODER_FILE))
        && exists(ds_model::onnxruntime_dylib_path())
}

/// Cheap selected-model presence gate used by status and helper spawn.
pub(crate) fn tts_model_files_present(cfg: &VoiceConfig) -> bool {
    if mlx_tts_active(cfg) {
        let frontend_present =
            cfg.tts_model != ds_config::TtsModel::Kokoro || kokoro_g2p_files_present();
        frontend_present
            && ds_model::mlx_repo::is_mlx_set_present(ds_model::mlx_repo::tts_mlx_set(
                cfg.tts_model,
            ))
    } else {
        ds_model::tts_model_files_present(cfg.tts_model)
            && ds_model::onnxruntime_dylib_path()
                .map(|path| path.is_file())
                .unwrap_or(false)
    }
}

/// The helper may preload TTS only when the selected model and runtime are present.
pub(crate) fn helper_preloads_tts(cfg: &VoiceConfig) -> bool {
    helper_uses_tts(cfg) && tts_model_files_present(cfg)
}

/// Cheap Parakeet ONNX file presence (status row; absence non-fatal to warm child).
pub(crate) fn parakeet_onnx_files_present() -> bool {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    exists(ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_DECODER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_JOINER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_TOKENS_FILE))
        && exists(ds_model::onnxruntime_dylib_path())
}

/// Full-duplex AEC: opted in + built-in STT/TTS + a model with streaming headroom.
pub(crate) fn full_duplex_wanted(cfg: &VoiceConfig) -> bool {
    // Parakeet-only AEC path; System stays half-duplex (owns its own recognition).
    cfg.full_duplex
        && cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        && cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
        && cfg.tts_model_descriptor().supports_full_duplex
}

/// Eager load/unload helper models to match config (single residency truth for status/stats).
pub(crate) fn reconcile_helper_models(tts: &Arc<tts::TtsManager>, cfg: &VoiceConfig) {
    if !helper_needed(cfg) || !tts.is_running() {
        return;
    }
    if helper_preloads_tts(cfg) {
        tts.load_engine(ds_helper_proto::HelperModel::Tts);
    } else {
        tts.unload_engine(ds_helper_proto::HelperModel::Tts);
    }
    if helper_uses_stt(cfg) {
        tts.load_engine(ds_helper_proto::HelperModel::Stt);
    } else {
        tts.unload_engine(ds_helper_proto::HelperModel::Stt);
    }
}

/// Dictation `Stt`: local helper when available; else `ds-engines` (no silent sub).
pub(crate) fn build_stt<P: Platform + 'static>(
    cfg: &VoiceConfig,
    plat: std::rc::Rc<P>,
    tts: Option<&Arc<tts::TtsManager>>,
    paste: &PasteState,
    paths: Option<&ds_config::Paths>,
) -> Box<dyn Stt> {
    if let Some(tts) = tts
        && local_stt_available(cfg)
    {
        log::info!(
            target: "engine",
            "dictation STT = local helper ({} / {})",
            cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off"),
            cfg.resolved_stt_provider().as_str()
        );
        return Box::new(crate::helper_stt::HelperStt::new(
            tts.clone(),
            paste.clone(),
        ));
    }
    log::info!(
        target: "engine",
        "dictation STT = factory fallback (resolved={}) — NOT the local helper",
        cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off")
    );
    ds_engines::make_stt_at(cfg, plat, &ds_engines::RealAvailability, paths)
}

/// Selected STT can run locally now. Reload must rebuild `stt` when this flips (download
/// without selection change). Not a config-only diff.
pub(crate) fn local_stt_available(cfg: &VoiceConfig) -> bool {
    match cfg.resolved_stt() {
        Some(ds_config::SttEngine::BuiltIn) => parakeet_present_for(cfg),
        // System not ready → inert SystemStt, never ClaudeNative.
        Some(ds_config::SttEngine::System) => system_stt_available(),
        _ => false,
    }
}

/// Surface refusal cue on failed Caps start for BuiltIn/System only (ClaudeCode always
/// startable; Off is pause/resume, not an error).
pub(crate) fn refusal_cue_on_refused_start(resolved: Option<ds_config::SttEngine>) -> bool {
    matches!(
        resolved,
        Some(ds_config::SttEngine::BuiltIn | ds_config::SttEngine::System)
    )
}

/// Trailing-edge reload debounce: quiet for `window` after the *last* trigger (not
/// since last run). Collapses IPC nudge + watch-event bursts for the same write into
/// one `Run`. `pending_since` is the most recent trigger tick (`None` = idle).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum ReloadTick {
    /// Quiet window elapsed since most recent trigger.
    Run,
    /// Trigger outstanding; window not elapsed.
    Defer,
    Idle,
}
pub(crate) fn reload_tick(
    triggered_now: bool,
    now: Instant,
    pending_since: Option<Instant>,
    window: Duration,
) -> (ReloadTick, Option<Instant>) {
    // Fresh trigger resets the quiet window (even over an already-pending one).
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

/// Mtime-watch decision. Returns true iff config.toml should be
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

/// Mtime watermark after a reload (`stat_now` is the only side channel).
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
/// `say`) needs no model → always ready; built-in plays only when its model
/// is resident + warm (`tts_loaded`); `None` (TTS off) never plays. The worker uses this
/// so a not-yet-downloaded / still-loading model never produces silent or garbage
/// playback. PURE.
pub(crate) fn tts_can_play(engine: Option<ds_config::TtsEngine>, tts_loaded: bool) -> bool {
    use ds_config::TtsEngine;
    match engine {
        None => false,
        Some(TtsEngine::System) => true,
        Some(TtsEngine::BuiltIn) => tts_loaded,
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

/// Serializes every `dontspeakd`-crate test (here and in `tts.rs`) that mutates the
/// process-wide `DONTSPEAK_MODEL_DIR` / `DONTSPEAK_MLX_DYLIB_PATH` / `ORT_DYLIB_PATH` env vars —
/// mirrors `ds-model/src/spec.rs`'s own `ENV_LOCK` idiom (`spec.rs:326`). ONE shared lock, not
/// a per-file one, so a `config_gate.rs` test and a `tts.rs` test touching the SAME var can't
/// interleave (`dontspeakd`'s test binary runs multi-threaded by default).
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_uses_onnx_runtime_gates_on_the_shim_not_the_raw_provider() {
        use ds_config::Provider;
        // MLX preference and shim presence select the MLX path
        // (NOT ONNX) ⇒ `parakeet_present_for` checks `parakeet_available()`, not the files.
        assert!(!stt_uses_onnx_runtime(Provider::Mlx, true));
        // The Intel (and any no-shim) case downgrades to ONNX CPU
        // arch-blindly, but with no shim it DOWNGRADES to ONNX-CPU ⇒ needs the model FILES.
        // This is the regression that made dictation fall back to Claude Code on Intel.
        assert!(stt_uses_onnx_runtime(Provider::Mlx, false));
        // Explicit ONNX providers are always the ONNX path, shim or not.
        assert!(stt_uses_onnx_runtime(Provider::OrtCpu, false));
        assert!(stt_uses_onnx_runtime(Provider::OrtCpu, true));
        assert!(stt_uses_onnx_runtime(Provider::OrtCuda, false));
        assert!(stt_uses_onnx_runtime(Provider::OrtCuda, true));
    }

    #[test]
    fn tts_uses_mlx_runtime_gates_on_the_shim_not_the_raw_provider() {
        use ds_config::Provider;

        assert!(tts_uses_mlx_runtime(Provider::Mlx, true));
        assert!(!tts_uses_mlx_runtime(Provider::Mlx, false));
        assert!(!tts_uses_mlx_runtime(Provider::OrtCpu, true));
    }

    #[test]
    fn parakeet_onnx_files_present_needs_all_four_plus_dylib() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_model_dir = std::env::var_os("DONTSPEAK_MODEL_DIR");
        let prev_ort = std::env::var_os("ORT_DYLIB_PATH");

        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: test-only env mutation, serialized by ENV_LOCK (held above), restored
        // below before returning.
        unsafe {
            std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path());
            std::env::remove_var("ORT_DYLIB_PATH");
        }
        assert!(!parakeet_onnx_files_present());

        std::fs::write(tmp.path().join(ds_model::PARAKEET_ENCODER_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::PARAKEET_DECODER_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::PARAKEET_JOINER_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::PARAKEET_TOKENS_FILE), b"dummy").unwrap();
        let dylib = tmp.path().join("dummy-onnxruntime.dylib");
        std::fs::write(&dylib, b"dummy").unwrap();
        // SAFETY: test-only env mutation, still serialized by ENV_LOCK (held for the
        // whole test), restored below.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &dylib);
        }
        assert!(parakeet_onnx_files_present());

        // SAFETY: restore the prior values (or clear them) so later tests see the real
        // env again.
        unsafe {
            match prev_model_dir {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
            match prev_ort {
                Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
                None => std::env::remove_var("ORT_DYLIB_PATH"),
            }
        }
    }

    #[test]
    fn normalize_long_press_uses_default_on_zero() {
        assert_eq!(normalize_long_press(0), DEFAULT_LONG_PRESS_MS);
        assert_eq!(normalize_long_press(750), 750);
        assert_eq!(normalize_long_press(1), 1);
    }

    #[test]
    fn caps_loop_enabled_mirrors_the_toggle() {
        let cfg = |caps: bool| VoiceConfig {
            caps,
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
        // Second trigger re-arms the quiet window so nudge + straggling watch collapse.
        let base = Instant::now();
        let window = Duration::from_millis(750);
        let straggler_gap = Duration::from_millis(581);

        let (tick, pending) = reload_tick(true, base, None, window);
        assert_eq!(tick, ReloadTick::Defer);

        let straggler_at = base + straggler_gap;
        let (tick, pending) = reload_tick(true, straggler_at, pending, window);
        assert_eq!(tick, ReloadTick::Defer);
        assert_eq!(
            pending,
            Some(straggler_at),
            "anchor re-armed on the second trigger"
        );

        let (tick, pending) = reload_tick(false, straggler_at + window, pending, window);
        assert_eq!(tick, ReloadTick::Run);
        assert_eq!(pending, None);

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
        assert!(!tts_can_play(None, true));
        assert!(!tts_can_play(None, false));
        assert!(tts_can_play(Some(TtsEngine::System), false));
        assert!(tts_can_play(Some(TtsEngine::System), true));
        assert!(!tts_can_play(Some(TtsEngine::BuiltIn), false));
        assert!(tts_can_play(Some(TtsEngine::BuiltIn), true));
    }

    #[test]
    fn model_capability_gates_full_duplex() {
        use ds_config::{SttEngine, TtsEngine, TtsModel};
        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        {
            let cfg = VoiceConfig {
                tts_engine: Some(vec![TtsEngine::BuiltIn]),
                tts_model: TtsModel::Chatterbox,
                stt_engine_ladder: vec![SttEngine::BuiltIn],
                stt_engine: None,
                full_duplex: true,
                ..VoiceConfig::default()
            };
            assert!(helper_uses_tts(&cfg));
            assert!(helper_needed(&cfg));
            assert!(!full_duplex_wanted(&cfg));
        }
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

        // Intel macOS resolves built_in from a runtime ORT capability; its two deterministic
        // branches are tested with an injected value in ds-config instead of scanning Homebrew.
        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        {
            let c2 = cfg(vec![TtsEngine::BuiltIn], vec![SttEngine::BuiltIn]);
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
        assert!(!stt_can_start(None, true, true));
        assert!(!stt_can_start(Some(SttEngine::BuiltIn), false, true));
        assert!(stt_can_start(Some(SttEngine::BuiltIn), true, false));
        assert!(!stt_can_start(Some(SttEngine::System), true, false));
        assert!(stt_can_start(Some(SttEngine::System), false, true));
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
