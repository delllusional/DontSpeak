//! Engine lifecycle: `engine_run`, startup wiring, signals, `install_bin`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{Paths, VoiceConfig};
use ds_platform::Platform;

use crate::barge::spawn_mic_barge_watcher;
use crate::config_gate::{
    build_stt, config_mtime, debug_enabled, full_duplex_wanted, helper_needed, helper_stt_provider,
    helper_uses_stt, local_stt_available, normalize_long_press, reconcile_helper_models,
    reload_watermark, should_reload_on_mtime, system_stt_needs_authorization,
};
use crate::downloads::{
    DownloadFlags, DownloadState, apply_provider_and_autofetch, auto_download_missing, wire,
};
use crate::engine::{Engine, PasteBuf, PasteState};
use crate::ipc::spawn_ipc_server;
use crate::listener;
use crate::stats;
use crate::status::{EngineShared, StatusGate};
use crate::stt_test::TestSession;
use crate::tts::TtsManager;
use crate::ttsq::TtsQueue;

pub(crate) const POLL_MS: u64 = 30; // caps-state poll
/// Trailing-edge reload debounce after last trigger.
const RELOAD_QUIET_WINDOW: Duration = Duration::from_millis(750);
/// Re-probe AX so live grant/revoke flips caps without reload.
const AX_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// Auto-download poll cadence; per-target failure policy applies backoff and permanent latches.
const AUTO_DL_RETRY_INTERVAL: Duration = Duration::from_secs(20);
/// Status-gate nudge while downloading (~2.5 Hz; progress alone doesn't bump seq).
const DL_PROGRESS_BUMP_INTERVAL: Duration = Duration::from_millis(400);
/// Coarse mtime backstop if config_watch drops events.
const MTIME_CHECK_INTERVAL: Duration = Duration::from_secs(3);

/// Engine entry on this thread. Host drives `running` / `reload_requested` (C ABI).
pub fn engine_run(
    running: Arc<AtomicBool>,
    reload_requested: Arc<AtomicBool>,
) -> Result<(), EngineError> {
    let debug = debug_enabled();
    ds_log::init();
    log::set_max_level(if debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });

    // Return Err, never process::exit (in-process host thread).
    let paths = match Paths::resolve() {
        Some(p) => p,
        None => {
            log::error!(target: "engine", "cannot resolve $HOME");
            return Err(EngineError::HomeUnresolved);
        }
    };

    let plat = match ds_platform::current() {
        Ok(p) => p,
        Err(e) => {
            log::error!(target: "engine", "platform init: {e}");
            return Err(EngineError::PlatformInit(e.to_string()));
        }
    };
    // AX needed only for Caps loop — denied is warn, not fatal (engine stays up).
    // request_permissions: one-time prompt so the app appears in Settings.
    plat.request_permissions();
    if let Err(e) = plat.preflight() {
        log::warn!(
            target: "engine",
            "{e} — Caps-Lock dictation is OFF until granted; \
             other subsystems (RPC/TTS/STT) run regardless."
        );
    }

    // Fail-open load (defaults on bad TOML).
    let cfg = VoiceConfig::load(&paths);
    let long_press_ms = cfg.long_press_ms;

    // System STT speech-auth prompt at boot (and reload) — ladder can resolve to System
    // without set_config's explicit opt-in.
    authorize_system_stt_if_needed(&cfg, &reload_requested, &running);

    log::info!(
        target: "engine",
        "engine started (poll={POLL_MS}ms long_press={long_press_ms}ms \
         stt={} debug={debug})",
        cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off")
    );
    // DONTSPEAK_BUILD_ID — log-only, not on wire.
    log::debug!(
        target: "engine",
        "build_id={}",
        env!("DONTSPEAK_BUILD_ID")
    );

    let _ = std::fs::create_dir_all(&paths.config_dir);
    let _ = std::fs::create_dir_all(&paths.state_dir);

    // Wire reconcile off-thread (exclude_clients). Limitation: clients installed
    // mid-run with unchanged exclude_clients wait until next boot.
    {
        let paths = paths.clone();
        std::thread::spawn(move || {
            let code = ds_wire::reconcile(&paths);
            if code != 0 {
                log::warn!(target: "engine", "wire reconcile exited {code}");
            }
        });
    }

    // Evict older engine before bind — IPC rebind would steal socket and double-narrate.
    if let Some(old) = ds_config::evict_stale_engine(&paths.engine_pid, std::process::id()) {
        log::info!(
            target: "engine",
            "evicted stale engine pid {old} before binding the RPC socket"
        );
    }

    // Own pid for next instance's eviction. Write failure → eviction no-ops.
    if let Err(e) = std::fs::write(&paths.engine_pid, std::process::id().to_string()) {
        log::warn!(
            target: "engine",
            "cannot write engine pidfile {}: {e}",
            paths.engine_pid.display()
        );
    }

    let tts_stats = Arc::new(stats::TtsStats::new());
    let stt_stats = Arc::new(stats::SttStats::new());
    let lifetime = Arc::new(stats::LifetimeSeconds::load(paths.stats_toml.clone()));
    let status_gate = StatusGate::new();
    let tts = Arc::new(TtsManager::new(
        install_bin("ds-helper"),
        paths.log_file.clone(),
        tts_stats.clone(),
        stt_stats.clone(),
        lifetime.clone(),
    ));
    tts.set_status_gate(status_gate.clone());
    let stt_test = Arc::new(TestSession::new(tts.clone()));
    let caps_active = Arc::new(AtomicBool::new(false));
    let stt_active = Arc::new(AtomicBool::new(false));
    let paste: PasteState = Arc::new(Mutex::new(PasteBuf::default()));
    // One mic watcher for TTS focus-hold + barge (cached; no per-timer device query).
    let mic_watcher = ds_platform::MicWatcher::spawn(|_| {});
    // Seed spawn prefs before queue/IPC — else boot heal can start STT-only helper
    // (constructor defaults; no playback sink until reload).
    tts.set_full_duplex_pref(full_duplex_wanted(&cfg));
    tts.set_stt_provider_pref(helper_stt_provider(&cfg));
    // STT preload only for built_in (provider token is "cpu" even for Off/ClaudeCode).
    tts.set_stt_wanted(helper_uses_stt(&cfg));
    tts.set_tts_wanted(crate::config_gate::helper_preloads_tts(&cfg));
    tts.set_tts_selection(cfg.tts_model);
    tts.set_provider(cfg.resolved_tts_provider().as_str());
    let ttsq = TtsQueue::start(
        tts.clone(),
        paths.clone(),
        status_gate.clone(),
        mic_watcher.handle(),
    );

    let downloads = Arc::new(Mutex::new(DownloadState::default()));

    let shared = EngineShared {
        tts: tts.clone(),
        caps_active: caps_active.clone(),
        stt_active: stt_active.clone(),
        paste: paste.clone(),
        downloads: downloads.clone(),
        tts_stats: tts_stats.clone(),
        stt_stats: stt_stats.clone(),
        lifetime: lifetime.clone(),
        gate: status_gate.clone(),
        dictation_presenters: Arc::new(
            crate::dictation_presenter::DictationPresenterRegistry::default(),
        ),
    };

    // Session registries before IPC (hooks nudge; supervisors filter).
    let codex_sessions = crate::codex_stream::SessionRegistry::new();
    let grok_sessions = crate::grok_stream::SessionRegistry::new();

    // RPC first — answer before Kokoro warm blocks.
    spawn_ipc_server(
        shared.clone(),
        paths.clone(),
        stt_test.clone(),
        ttsq.clone(),
        reload_requested.clone(),
        codex_sessions.clone(),
        grok_sessions.clone(),
    );

    spawn_mic_barge_watcher(ttsq.clone(), stt_active.clone(), mic_watcher.handle());

    // Mid-turn supervisors (self-gating; re-read config each pass).
    crate::codex_stream::spawn_supervisor(
        paths.clone(),
        running.clone(),
        codex_sessions,
        mic_watcher.handle(),
        ttsq.clone(),
    );

    crate::grok_stream::spawn_supervisor(
        paths.clone(),
        running.clone(),
        grok_sessions,
        mic_watcher.handle(),
        ttsq.clone(),
    );

    // Warm helper when needed (RPC already serving).
    tts.set_enabled(helper_needed(&cfg));
    // Wire reload/shutdown hooks before any download (completion must no-op after stop).
    wire(
        &downloads,
        tts.clone(),
        paths.clone(),
        DownloadFlags {
            reload: reload_requested.clone(),
            running: running.clone(),
        },
    );
    apply_provider_and_autofetch(&tts, &downloads, &cfg);
    reconcile_helper_models(&tts, &cfg);

    // No silent STT substitution — unavailable → inert placeholder.
    let mut daemon = Engine::with_config(
        plat,
        &cfg,
        normalize_long_press(long_press_ms),
        Some(&paths),
    );
    daemon.tts = Some(tts.clone());
    daemon.ttsq = Some(ttsq.clone());
    daemon.caps_active = Some(caps_active.clone());
    daemon.stt_active = Some(stt_active.clone());
    daemon.status_gate = Some(status_gate.clone());
    daemon.paste = paste.clone();
    // Rebuild STT now that tts is set (HelperStt path).
    daemon.stt = build_stt(
        &cfg,
        daemon.plat.clone(),
        daemon.tts.as_ref(),
        &daemon.paste,
        Some(&paths),
    );
    // For reload to detect local availability flip after first model download.
    daemon.stt_is_local = daemon.tts.is_some() && local_stt_available(&cfg);
    if cfg.listen_mode == ds_config::ListenMode::Always {
        daemon.listener = Some(listener::Listener::new(
            &cfg,
            daemon.plat.clone(),
            ds_model::parakeet_dir().unwrap_or_default(),
            listener::ListenerShared {
                paste: paste.clone(),
                stt_active: stt_active.clone(),
                ttsq: daemon.ttsq.clone(),
                gate: Some(status_gate.clone()),
            },
        ));
    }
    caps_active.store(daemon.caps, Ordering::Relaxed);
    let poll = Duration::from_millis(POLL_MS);

    status_gate.bump();

    let mut last_seen = config_mtime(&paths.config_toml);
    let mut pending_reload_since: Option<Instant> = None;
    let mut last_ax_probe = Instant::now()
        .checked_sub(AX_PROBE_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_auto_dl = Instant::now();
    let mut last_dl_bump = Instant::now();
    let mut dl_was_active = false; // active→ended edge for terminal state push
    let mut last_mtime_check = Instant::now()
        .checked_sub(MTIME_CHECK_INTERVAL)
        .unwrap_or_else(Instant::now);
    // Push watch; mtime below is coarse backstop.
    let _config_watcher = crate::config_watch::spawn(&paths.config_toml, reload_requested.clone());
    // Caps-HID stuck → relaunch after graceful shutdown.
    let mut relaunch_after_shutdown = false;
    // Capture reason at detection for dontspeak.log (GUI stderr may be discarded).
    let mut relaunch_reason: Option<&'static str> = None;

    while running.load(Ordering::Relaxed) {
        daemon.tick();

        if last_ax_probe.elapsed() >= AX_PROBE_INTERVAL {
            daemon.refresh_caps_gate();
            last_ax_probe = Instant::now();
            // swap: don't relaunch over an explicit quit that already cleared running.
            if daemon.needs_relaunch() && running.swap(false, Ordering::Relaxed) {
                relaunch_reason = daemon.relaunch_reason();
                log::info!(
                    target: "engine",
                    "{} is stuck denied despite an already-trusted Accessibility grant — \
                     relaunching to pick it up fresh",
                    relaunch_reason.unwrap_or("a caps-related HID resource")
                );
                relaunch_after_shutdown = true;
            }
        }

        // Throttled: auto-download retry + fire-and-forget load/unload self-heal
        // (no wire ack; recovers dropped stdio writes, not closed races).
        if last_auto_dl.elapsed() >= AUTO_DL_RETRY_INTERVAL {
            auto_download_missing(&downloads, &daemon.cfg);
            reconcile_helper_models(&tts, &daemon.cfg);
            last_auto_dl = Instant::now();
        }

        // Progress doesn't bump seq — nudge while active + once on active→ended.
        if last_dl_bump.elapsed() >= DL_PROGRESS_BUMP_INTERVAL {
            let downloading = downloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .any_active();
            if downloading || dl_was_active {
                status_gate.bump();
            }
            dl_was_active = downloading;
            last_dl_bump = Instant::now();
        }

        let hup = reload_requested.swap(false, Ordering::Relaxed);
        // Throttle mtime; non-check ticks keep last_seen so hup doesn't mask edits.
        let mut current = last_seen;
        let mut mtime_changed = false;
        if last_mtime_check.elapsed() >= MTIME_CHECK_INTERVAL {
            last_mtime_check = Instant::now();
            current = config_mtime(&paths.config_toml);
            mtime_changed = should_reload_on_mtime(last_seen, current);
        }
        let (reload_decision, next_pending) = crate::config_gate::reload_tick(
            hup || mtime_changed,
            Instant::now(),
            pending_reload_since,
            RELOAD_QUIET_WINDOW,
        );
        pending_reload_since = next_pending;
        match reload_decision {
            crate::config_gate::ReloadTick::Defer => {}
            crate::config_gate::ReloadTick::Idle => {}
            crate::config_gate::ReloadTick::Run => {
                // Fail-open load (bad TOML → defaults).
                let new_cfg = VoiceConfig::load(&paths);
                let stt_engine_changed = new_cfg.resolved_stt() != daemon.cfg.resolved_stt();
                // Wire reconcile only when exclude set changes (avoid set_config churn).
                if new_cfg.excluded_clients() != daemon.cfg.excluded_clients() {
                    let paths = paths.clone();
                    std::thread::spawn(move || {
                        let _ = ds_wire::reconcile(&paths);
                    });
                }
                daemon.reload(&new_cfg);
                if stt_engine_changed {
                    stt_stats.reset();
                }
                authorize_system_stt_if_needed(&new_cfg, &reload_requested, &running);
                apply_provider_and_autofetch(&tts, &downloads, &new_cfg);
                // Refresh watermark on hup so mtime backstop doesn't double-reload.
                last_seen =
                    reload_watermark(mtime_changed, current, || config_mtime(&paths.config_toml));
            }
        }

        std::thread::sleep(poll);
    }
    daemon.shutdown();
    status_gate.bump();

    // Hosted path: OS won't reap children — stop helper explicitly.
    tts.set_enabled(false);
    lifetime.flush();
    // Detached threads (IPC, barge, ttsq, optional auth) not joined — Arc-only, rebind OK.

    // Remove pid only if still ours (don't clobber a newer instance).
    if ds_config::read_engine_pid(&paths.engine_pid).map(|pid| pid as u32)
        == Some(std::process::id())
    {
        let _ = std::fs::remove_file(&paths.engine_pid);
    }
    let _ = std::fs::remove_file(&paths.engine_sock);

    // Caps-HID stuck: re-exec this binary (not open -b — could reactivate dying instance).
    if relaunch_after_shutdown {
        if caps_relaunch_budget_exhausted(&paths) {
            log::info!(
                target: "engine",
                "caps HID relaunch: already relaunched {MAX_CAPS_RELAUNCHES} times in the \
                 last {}s over {} — giving up rather than loop forever; it stays broken \
                 until a manual restart",
                CAPS_RELAUNCH_WINDOW.as_secs(),
                relaunch_reason.unwrap_or("a caps-related HID resource"),
            );
            return Ok(());
        }
        let relaunched = std::env::current_exe()
            .and_then(|exe| std::process::Command::new(&exe).spawn().map(|_| ()));
        if let Err(e) = relaunched {
            // Resources already released — stay alive so user can find/relaunch manually.
            log::info!(
                target: "engine",
                "relaunch failed ({e}) — NOT exiting; this instance has already released \
                 its resources (no RPC, no caps monitor) and can't serve as a fallback \
                 host, but exiting with nothing left to bring the app back is worse"
            );
        }
    }
    Ok(())
}

/// Off-thread System STT speech-auth when needed. Nudges reload on completion unless
/// `running` already false (same shutdown race as download completion hooks).
fn authorize_system_stt_if_needed(
    cfg: &VoiceConfig,
    reload_requested: &Arc<AtomicBool>,
    running: &Arc<AtomicBool>,
) {
    if !system_stt_needs_authorization(cfg) {
        return;
    }
    let reload_requested = reload_requested.clone();
    let running = running.clone();
    std::thread::spawn(move || {
        if let Err(e) = ds_stt::system_authorize() {
            log::warn!(
                target: "engine",
                "system STT authorization failed: {e} — dictation stays on the \
                 Claude Code fallback until granted"
            );
        }
        if running.load(Ordering::Relaxed) {
            reload_requested.store(true, Ordering::Relaxed);
        }
    });
}

/// Caps-HID relaunch cap within window (disk marker; avoid unbounded quit loop).
const MAX_CAPS_RELAUNCHES: u32 = 3;
/// Streak older than this doesn't count (fresh stuck episode).
const CAPS_RELAUNCH_WINDOW: Duration = Duration::from_secs(120);

/// RMW relaunch-streak marker. Exhausted → true without bumping (stay tripped).
fn caps_relaunch_budget_exhausted(paths: &Paths) -> bool {
    let marker = paths.state_dir.join("caps-relaunch-guard");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let prior_count = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|s| {
            let (count, at) = s.trim().split_once(' ')?;
            Some((count.parse::<u32>().ok()?, at.parse::<u64>().ok()?))
        })
        .filter(|(_, at)| now.saturating_sub(*at) <= CAPS_RELAUNCH_WINDOW.as_secs())
        .map(|(count, _)| count)
        .unwrap_or(0);
    if prior_count >= MAX_CAPS_RELAUNCHES {
        return true;
    }
    let _ = std::fs::write(&marker, format!("{} {now}", prior_count + 1));
    false
}

/// Fatal startup failure returned from [`engine_run`] (never process::exit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    HomeUnresolved,
    PlatformInit(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::HomeUnresolved => write!(f, "cannot resolve $HOME"),
            EngineError::PlatformInit(e) => write!(f, "platform init: {e}"),
        }
    }
}

/// Sibling of this exe, else bare name on `$PATH`.
fn install_bin(name: &str) -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(name);
        if sibling.exists() {
            return sibling;
        }
    }
    std::path::PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Paths with state_dir created (budget write no-ops if parent missing).
    fn test_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        (dir, paths)
    }

    #[test]
    fn caps_relaunch_budget_missing_marker_starts_a_fresh_streak() {
        let (_dir, paths) = test_paths();
        let marker = paths.state_dir.join("caps-relaunch-guard");
        assert!(!marker.exists());

        assert!(!caps_relaunch_budget_exhausted(&paths));

        let written = std::fs::read_to_string(&marker).unwrap();
        let (count, _at) = written.trim().split_once(' ').expect("count + timestamp");
        assert_eq!(count, "1", "first-ever detection writes count=1");
    }

    #[test]
    fn caps_relaunch_budget_trips_once_the_cap_is_reached() {
        let (_dir, paths) = test_paths();
        // The first MAX_CAPS_RELAUNCHES detections all stay within budget (counts 1..=3
        // get written), each returning false.
        for n in 0..MAX_CAPS_RELAUNCHES {
            assert!(
                !caps_relaunch_budget_exhausted(&paths),
                "detection {n} should still be within budget"
            );
        }
        // The NEXT detection reads a prior count already at the cap ⇒ exhausted.
        assert!(caps_relaunch_budget_exhausted(&paths));
    }

    #[test]
    fn caps_relaunch_budget_marker_older_than_the_window_is_a_fresh_streak() {
        let (_dir, paths) = test_paths();
        let marker = paths.state_dir.join("caps-relaunch-guard");
        // A marker already AT the cap, but timestamped well outside CAPS_RELAUNCH_WINDOW —
        // this must NOT inherit the old count; a much-later stuck episode starts fresh.
        let stale_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(CAPS_RELAUNCH_WINDOW.as_secs() + 30);
        std::fs::write(&marker, format!("{MAX_CAPS_RELAUNCHES} {stale_at}")).unwrap();

        assert!(!caps_relaunch_budget_exhausted(&paths));
    }

    #[test]
    fn engine_error_display_matches_the_logged_reason() {
        assert_eq!(
            EngineError::HomeUnresolved.to_string(),
            "cannot resolve $HOME"
        );
        assert_eq!(
            EngineError::PlatformInit("x".into()).to_string(),
            "platform init: x"
        );
    }
}
