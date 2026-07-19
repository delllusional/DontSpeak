//! Pure config predicates + reload decisions. Side-effect-light
//! (`reconcile_helper_models` touches the warm helper; rest pure).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ds_config::VoiceConfig;
use ds_platform::Platform;
use ds_stt::Stt;

use crate::engine::PasteState;
use crate::tts;

/// §F physical-hold threshold default.
pub(crate) const DEFAULT_LONG_PRESS_MS: u64 = 600;

/// `0` → default; else honor. Shared by startup and `Engine::reload`.
pub(crate) fn normalize_long_press(ms: u64) -> u64 {
    if ms == 0 { DEFAULT_LONG_PRESS_MS } else { ms }
}

/// Caps-Lock dictation loop gated by `caps`.
pub(crate) fn caps_loop_enabled(cfg: &VoiceConfig) -> bool {
    cfg.caps
}

/// Warm helper needed when either Kokoro TTS or local STT is in use.
pub(crate) fn helper_needed(cfg: &VoiceConfig) -> bool {
    helper_uses_tts(cfg) || helper_uses_stt(cfg)
}

/// Resolved TTS is Kokoro (helper hosts the model).
pub(crate) fn helper_uses_tts(cfg: &VoiceConfig) -> bool {
    cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
}

/// Resolved STT is BuiltIn or System (both run through the warm helper).
pub(crate) fn helper_uses_stt(cfg: &VoiceConfig) -> bool {
    matches!(
        cfg.resolved_stt(),
        Some(ds_config::SttEngine::BuiltIn | ds_config::SttEngine::System)
    )
}

/// macOS + `SMKOKORO_DYLIB_PATH` present. Combine with model-asset gates before advertising.
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

/// ANE Parakeet usable: shim + Core ML sets (same revision markers as the downloader).
pub(crate) fn parakeet_available() -> bool {
    apple_native_shim_available()
        && ds_model::coreml_repo::is_coreml_set_present(&ds_model::coreml_repo::PARAKEET_COREML_SET)
}

/// Provider-aware Parakeet readiness: ONNX files vs ANE shim+Core ML (not raw ONNX-only).
pub(crate) fn parakeet_present_for(cfg: &VoiceConfig) -> bool {
    if stt_uses_onnx_runtime(cfg.resolved_stt_provider(), apple_native_shim_available()) {
        ds_model::is_parakeet_present() // ONNX-CPU/CUDA: needs the downloaded model FILES
    } else {
        parakeet_available() // genuine ANE: shim + its downloaded Core ML sets
    }
}

/// Built-in STT on ONNX (needs model files) vs real ANE. `Provider::Ane` is arch-blind —
/// real only when shim present; else downgrades to ONNX-CPU. Shared by presence + status.
pub(crate) fn stt_uses_onnx_runtime(provider: ds_config::Provider, shim_available: bool) -> bool {
    !(provider == ds_config::Provider::Ane && shim_available)
}

/// Genuine apple-native Kokoro: preference + shim (TTS twin of `!stt_uses_onnx_runtime`).
pub(crate) fn apple_native_tts_active(cfg: &VoiceConfig) -> bool {
    cfg.uses_apple_native_model() && apple_native_shim_available()
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

/// Cheap Kokoro ONNX file presence (no sha256). Shared by status row + spawn gate.
pub(crate) fn kokoro_onnx_files_present() -> bool {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    exists(ds_model::model_path(ds_model::KOKORO_ONNX_FILE))
        && exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE))
        && kokoro_g2p_files_present()
}

/// Cheap shared-frontend presence gate used by both Kokoro synthesis backends.
pub(crate) fn kokoro_g2p_files_present() -> bool {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    exists(ds_model::model_path(ds_model::KOKORO_G2P_ENCODER_FILE))
        && exists(ds_model::model_path(ds_model::KOKORO_G2P_DECODER_FILE))
        && exists(ds_model::onnxruntime_dylib_path())
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

/// Kokoro status "present": ONNX needs files; apple-native needs shim + files.
pub(crate) fn kokoro_present_for(apple_native: bool, shim: bool, files: bool) -> bool {
    if apple_native { shim && files } else { files }
}

/// Full-duplex AEC: opted in + BuiltIn Parakeet + Kokoro TTS (not System STT). docs/AEC.md.
pub(crate) fn full_duplex_wanted(cfg: &VoiceConfig) -> bool {
    // Parakeet-only AEC path; System stays half-duplex (owns its own recognition).
    cfg.full_duplex
        && cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        && helper_uses_tts(cfg)
}

/// Eager load/unload helper models to match config (single residency truth for status/stats).
pub(crate) fn reconcile_helper_models(tts: &Arc<tts::TtsManager>, cfg: &VoiceConfig) {
    if !helper_needed(cfg) || !tts.is_running() {
        return;
    }
    if helper_uses_tts(cfg) {
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

/// Trailing-edge reload debounce: apply a config change
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
/// `say`) needs no model → always ready; `Kokoro` plays only when its model is resident +
/// warm (`tts_loaded`); `None` (TTS off) never plays. The worker uses this so a
/// not-yet-downloaded / still-loading model never produces silent or garbage playback. PURE.
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
/// process-wide `DONTSPEAK_MODEL_DIR` / `SMKOKORO_DYLIB_PATH` / `ORT_DYLIB_PATH` env vars —
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
    fn kokoro_onnx_files_present_needs_synth_frontend_and_dylib() {
        // Hermetic: point `model_dir()` at a FRESH, EMPTY temp dir via `DONTSPEAK_MODEL_DIR`
        // (the same override `ds_config::model_dir()` respects) so this never reads the real
        // ambient OS model cache. `ORT_DYLIB_PATH` is checked BEFORE the `DONTSPEAK_MODEL_DIR`-
        // honoring fallback (`ds_model::onnxruntime_dylib_path()`), so clear it too for a
        // genuinely hermetic claim.
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
        // Empty dir, no dylib override: definitely false. `&&` short-circuits on the first
        // missing file, so this doesn't even need `ORT_DYLIB_PATH` set deliberately.
        assert!(!kokoro_onnx_files_present());

        std::fs::write(tmp.path().join(ds_model::KOKORO_ONNX_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::KOKORO_VOICES_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::KOKORO_G2P_ENCODER_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::KOKORO_G2P_DECODER_FILE), b"dummy").unwrap();
        let dylib = tmp.path().join("dummy-onnxruntime.dylib");
        std::fs::write(&dylib, b"dummy").unwrap();
        // SAFETY: test-only env mutation, still serialized by ENV_LOCK (held for the
        // whole test), restored below.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &dylib);
        }
        assert!(kokoro_onnx_files_present());

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
        assert!(!tts_can_play(Some(TtsEngine::BuiltIn), false));
        assert!(tts_can_play(Some(TtsEngine::BuiltIn), true));
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
