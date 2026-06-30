//! Engine lifecycle / orchestration: the headless entry, `engine_run`, the
//! startup wiring, the signal handlers, and `install_bin`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{Paths, VoiceConfig};
use ds_platform::Platform;

use crate::barge::spawn_mic_barge_watcher;
use crate::config_gate::{
    build_stt, config_mtime, debounce_ok, debug_enabled, full_duplex_wanted, helper_needed,
    helper_stt_provider, helper_uses_stt, normalize_long_press, reconcile_helper_models,
    reload_watermark, should_reload_on_mtime,
};
use crate::downloads::{DownloadState, apply_tts_provider, auto_download_missing, set_reload_hook};
use crate::engine::{Engine, PasteBuf, PasteState};
use crate::ipc::spawn_ipc_server;
use crate::listener;
use crate::logging::log;
use crate::stats;
use crate::status::{CapsLog, EngineShared, StatusGate};
use crate::stt_test::TestSession;
use crate::tts::TtsManager;
use crate::ttsq::TtsQueue;

// ── Tunables (match the original Swift daemon) ──────────────────────────────
pub(crate) const POLL_MS: u64 = 30; // caps-state poll interval
/// §E.4 hot-reload debounce: collapse a flurry of triggers (e.g. the GUI's
/// atomic settings.json write AND its explicit SIGHUP nudge that follows) into a
/// single reload. Also hardens against editors that write-twice on save.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
/// How often to re-probe Accessibility trust so a live grant/revoke flips the
/// caps loop without a reload (the dot follows ~this fast).
const AX_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// How often the poll loop re-checks for a missing model to auto-download (retry safety
/// net for a launch-time download that failed / had no network). Slow — the startup +
/// reload hooks handle the common "first activation" case immediately.
const AUTO_DL_RETRY_INTERVAL: Duration = Duration::from_secs(20);
/// Coarse `stat()` BACKSTOP for an out-of-band settings.json edit. The primary trigger is
/// the push-based [`config_watch`](crate::config_watch) filesystem watcher; this slow stat
/// only covers the rare case the watcher can't start or a filesystem drops an event. Kept
/// well under any human re-edit cadence. The SIGHUP / Reload-RPC path is independent (it
/// sets `reload_requested` directly, read every tick).
const MTIME_CHECK_INTERVAL: Duration = Duration::from_secs(3);

/// Run the engine to completion on the CURRENT thread. The host owns the two
/// control flags: `running` (clear it → graceful stop) and `reload_requested`
/// (set it → re-read settings.json). The headless binary drives these from POSIX
/// signals ([`run_headless`]); an in-process host (the SwiftUI/Win/Linux app via
/// the C ABI) drives them from `engine_stop()` / `engine_reload()`. The caps loop,
/// RPC server, and TTS queue all run from here, so whichever process calls this is
/// the one that needs the OS permissions (Accessibility / Input-Monitoring / Mic).
pub fn engine_run(
    running: Arc<AtomicBool>,
    reload_requested: Arc<AtomicBool>,
) -> Result<(), EngineError> {
    let debug = debug_enabled();

    // FATAL startup failures RETURN an error instead of process::exit(): this fn
    // also runs on a background thread INSIDE the host app (the in-process FFI
    // host), where an exit() would kill the whole app. The headless binary maps
    // the Err back to a process exit (run_headless), so its behavior is unchanged.
    let paths = match Paths::resolve() {
        Some(p) => p,
        None => {
            log("FATAL: cannot resolve $HOME");
            return Err(EngineError::HomeUnresolved);
        }
    };

    let plat = match ds_platform::current() {
        Ok(p) => p,
        Err(e) => {
            log(&format!("FATAL: platform init: {e}"));
            return Err(EngineError::PlatformInit(e.to_string()));
        }
    };
    // engine-owns-everything: Accessibility (CGEventPost) is needed ONLY for the
    // Caps-Lock dictation loop — NOT for the RPC host, TTS, STT capture,
    // or config. So a denied preflight is a WARNING, not a fatal exit: the engine
    // stays up as the resident service (no more launchd crash-loop), and the caps
    // loop self-gates on AX trust (re-probed each reload, so granting it later +
    // a reload nudge enables dictation without a restart).
    // One-time PROMPT for the OS permission the caps loop needs (macOS
    // Accessibility). This registers the app in the Accessibility list AND shows the
    // grant dialog on a fresh install, so the user has a row to toggle — without it
    // the silent `preflight` probe below just keeps logging "not trusted" forever and
    // the app never appears in Settings. No-op off macOS / when already trusted.
    plat.request_permissions();
    if let Err(e) = plat.preflight() {
        log(&format!(
            "WARN: {e} — Caps-Lock dictation is OFF until granted; \
             other subsystems (RPC/TTS/STT) run regardless."
        ));
    }

    // §F: read the physical-hold threshold from settings.json. Fail-open:
    // `VoiceConfig::load` already defaults long_press_ms=600 on any error, so a
    // missing / bad settings.json still yields a working engine.
    let cfg = VoiceConfig::load(&paths);
    let long_press_ms = cfg.long_press_ms;

    log(&format!(
        "dontspeakd started (poll={POLL_MS}ms long_press={long_press_ms}ms \
         stt={} debug={debug})",
        cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off")
    ));

    // Make sure both our roots exist before we write settings / the pidfile / bind the RPC
    // socket — unlike ~/.claude they aren't created by another tool. On Windows these are
    // distinct (roaming %APPDATA% config vs local %LOCALAPPDATA% state); on macOS they're
    // the same dir. (Individual writers also create_dir_all their own parents.)
    let _ = std::fs::create_dir_all(&paths.config_dir);
    let _ = std::fs::create_dir_all(&paths.state_dir);

    // Seed the user-editable narration spec on first run (never overwrite the user's edits).
    // The SessionStart hook injects this file's contents into Claude so replies lead with a
    // spoken-line blockquote.
    if !paths.narration_spec.exists()
        && let Err(e) = std::fs::write(&paths.narration_spec, ds_config::DEFAULT_NARRATION_SPEC)
    {
        log(&format!(
            "WARN: cannot write default narration spec {}: {e}",
            paths.narration_spec.display()
        ));
    }

    // Single-instance guard: evict an OLDER engine BEFORE we bind the socket below.
    // launchd's KeepAlive only enforces one launchd-managed daemon — it does NOT
    // cover the engine running in-process inside the GUI host, and Windows/headless
    // have no OS singleton at all. Since `ds_ipc::bind` unlinks + rebinds the
    // socket, a second engine would otherwise STEAL the path from a still-running
    // first one, leaving two engines that both narrate (heard as the same reply
    // spoken twice after a reinstall/upgrade). Ask the old one to exit first; this
    // is cross-platform (SIGTERM → clean shutdown on unix; TerminateProcess on
    // Windows, after which its helper self-exits on stdin EOF). No-op if none/dead.
    if let Some(old) = ds_config::evict_stale_engine(&paths.engine_pid, std::process::id()) {
        log(&format!(
            "evicted stale engine pid {old} before binding the RPC socket"
        ));
    }

    // §E.4: write our own pid so the GUI can SIGHUP us for a no-restart reload +
    // probe our liveness, and so the NEXT engine to start can evict US the same way
    // (see evict_stale_engine above). Tolerate a write failure: the GUI then falls
    // back to launchctl, so the engine keeps running either way.
    if let Err(e) = std::fs::write(&paths.engine_pid, std::process::id().to_string()) {
        log(&format!(
            "WARN: cannot write daemon pidfile {}: {e}",
            paths.engine_pid.display()
        ));
    }

    // `running` / `reload_requested` are owned by the caller (the headless bin
    // wires them to SIGTERM/SIGINT + SIGHUP; the in-app FFI host flips them from
    // engine_stop()/engine_reload()).

    // Live stats for the warm helper (Kokoro TTS + Parakeet STT), fed below.
    let tts_stats = Arc::new(stats::TtsStats::new());
    let stt_stats = Arc::new(stats::SttStats::new());
    // Persisted lifetime seconds (spoken + heard), summed across sessions. Lives next
    // to the other side files in our data dir; loaded now, rewritten after each utterance.
    let lifetime = Arc::new(stats::LifetimeSeconds::load(paths.stats_toml.clone()));
    // Status push gate: every component that flips a `model_status` flag bumps it; the
    // `WaitModelStatus` IPC handler blocks on it. ONE Arc, shared by all of them. Built
    // up front so the TTS manager + queue (below) can be wired to it at construction.
    let status_gate = StatusGate::new();
    let tts = Arc::new(TtsManager::new(
        install_bin("ds-helper"),
        tts_stats.clone(),
        stt_stats.clone(),
        lifetime.clone(),
    ));
    // Wire the gate into the TTS manager so a mute toggle pushes (the muted flag is in
    // `model_status`). Done after construction to keep `new`'s signature test-friendly.
    tts.set_status_gate(status_gate.clone());
    // STT runs through the warm helper (consolidation) — TestSession delegates to it.
    let stt_test = Arc::new(TestSession::new(tts.clone()));
    // Effective caps state (AX-gated), shared with the RPC status handler.
    let caps_active = Arc::new(AtomicBool::new(false));
    // Live dictation flag + recent-events log, the engine → app caps status
    // channel surfaced through `model_status`.
    let stt_active = Arc::new(AtomicBool::new(false));
    let caps_log: CapsLog = Arc::new(Mutex::new(VecDeque::new()));
    // Dictation-preview buffer shared between the engine (writes partials/pending,
    // pastes on confirm) and the IPC status handler (reads it for the `dictation`
    // object the confirm panel renders).
    let paste: PasteState = Arc::new(Mutex::new(PasteBuf::default()));
    // The ONE mic-in-use watcher (CoreAudio listener on macOS, poll thread elsewhere). Its
    // cached state feeds BOTH the TTS worker's focus-hold and the mic-barge watcher, so
    // neither queries the audio device on a timer. Held for the engine's lifetime.
    let mic_watcher = ds_platform::MicWatcher::spawn(|_| {});
    // The single TTS serializer: all speech (replies + narration) flows through
    // this queue onto the warm child, so there is no per-block model reload.
    let ttsq = TtsQueue::start(
        tts.clone(),
        paths.clone(),
        status_gate.clone(),
        mic_watcher.handle(),
    );

    // Background model-download state (polled via model_status by the app's dots).
    let downloads = Arc::new(Mutex::new(DownloadState::default()));

    // The ONE allowed structural tweak: bundle the shared Arc handles threaded
    // through the RPC server and the status aggregator into a single struct, built
    // ONCE here (same Arcs, same clones), so both take `&EngineShared` instead of
    // a long list of loose `Arc`-cloned args.
    let shared = EngineShared {
        tts: tts.clone(),
        caps_active: caps_active.clone(),
        stt_active: stt_active.clone(),
        caps_log: caps_log.clone(),
        paste: paste.clone(),
        downloads: downloads.clone(),
        tts_stats: tts_stats.clone(),
        stt_stats: stt_stats.clone(),
        lifetime: lifetime.clone(),
        gate: status_gate.clone(),
    };

    // engine-owns-everything: host the RPC socket FIRST so ping/get/set/shutdown
    // are answerable immediately — BEFORE warming Kokoro below, whose model load
    // blocks for a few seconds (otherwise a client right after launch times out).
    spawn_ipc_server(
        shared.clone(),
        paths.clone(),
        running.clone(),
        stt_test.clone(),
        ttsq.clone(),
        reload_requested.clone(),
        downloads.clone(),
    );

    // Barge-in TTS the instant the mic goes active (Claude Code's own voice
    // recording is invisible to the engine otherwise), so speech never plays into
    // a live recording.
    spawn_mic_barge_watcher(ttsq.clone(), stt_active.clone(), mic_watcher.handle());

    // Full-duplex AEC env for the warm helper, decided BEFORE the boot start so the
    // child spawns with the right mode (Parakeet STT + Kokoro TTS — see docs/AEC.md).
    tts.set_full_duplex_pref(full_duplex_wanted(&cfg));
    tts.set_stt_provider_pref(helper_stt_provider(&cfg));
    // Preload STT in parallel with the TTS load only when STT is the built-in (Parakeet)
    // engine — `helper_stt_provider` resolves to "ort_cpu" even for Off/ClaudeCode, so it
    // can't gate this.
    tts.set_stt_wanted(helper_uses_stt(&cfg));
    // Warm Kokoro only when TTS is on AND Kokoro is the engine (System uses `say`,
    // which needs no warm model). Blocks on the model load, but the RPC server
    // thread above is already serving.
    tts.set_enabled(helper_needed(&cfg));
    // Make the helper's resident models match the selection at boot (preload the
    // selected engine, free the other) so the UI's "loaded" is right from the start.
    // Apply the persisted execution-provider preference before the warm child
    // starts; on Windows "ort_cuda" downloads the GPU runtime (background) then restarts
    // BOTH engines onto the GPU (the shared `provider` drives Kokoro TTS + Parakeet STT).
    // Wire the warm-child reload hook BEFORE any download can start, so a model fetched here
    // (or on a later reload / IPC request) restarts the child to load it — the shared
    // self-heal that makes a provider switch / fresh install converge without a manual restart.
    set_reload_hook(&downloads, tts.clone(), paths.clone());
    apply_tts_provider(&tts, &downloads, cfg.resolved_tts_provider());
    reconcile_helper_models(&tts, &cfg);
    // Full-auto: fetch any missing model for an enabled engine right away (no manual
    // Download button). Retried on reload + the slow poll tick below if it fails.
    auto_download_missing(&downloads, &cfg);

    // Select the STT engine from config (Phase-1 default == ClaudeNative). The
    // factory degrades to ClaudeNative when the chosen engine is unavailable.
    // §E.4 below hot-reloads this box on SIGHUP or a settings.json mtime change.
    let mut daemon = Engine::with_config(
        plat,
        &cfg,
        paths.pidfile.clone(),
        normalize_long_press(long_press_ms),
    );
    daemon.tts = Some(tts.clone());
    daemon.ttsq = Some(ttsq.clone());
    daemon.caps_active = Some(caps_active.clone());
    daemon.stt_active = Some(stt_active.clone());
    daemon.caps_log = Some(caps_log.clone());
    // Share the SAME push gate the IPC `WaitModelStatus` handler blocks on, so the
    // engine's dictation-change bumps wake the app's overlay push thread.
    daemon.status_gate = Some(status_gate.clone());
    // Share the SAME preview buffer the IPC status handler reads, so partials the
    // helper writes and the `pending` transcript are visible to the confirm panel.
    daemon.paste = paste.clone();
    // Parakeet dictation runs THROUGH the warm helper now (consolidation): rebuild
    // the stt as HelperStt now that daemon.tts is set (with_config built the default).
    daemon.stt = build_stt(
        &cfg,
        daemon.plat.clone(),
        daemon.tts.as_ref(),
        &daemon.paste,
    );
    // Always-listening: build the hands-free listener up front if configured
    // (otherwise the Caps-Lock PTT path runs as before). Hot-reload toggles it
    // via Engine::reload.
    if cfg.listen_mode == ds_config::ListenMode::Always {
        daemon.listener = Some(listener::Listener::new(
            &cfg,
            daemon.plat.clone(),
            ds_model::parakeet_dir().unwrap_or_default(),
            paste.clone(),
            stt_active.clone(),
            daemon.ttsq.clone(),
            Some(status_gate.clone()),
        ));
    }
    caps_active.store(daemon.caps_enabled, Ordering::Relaxed);
    let poll = Duration::from_millis(POLL_MS);

    // Engine is up and serving: push the engineRunning transition so a client that was
    // blocked on `WaitModelStatus` across a restart re-reads a fresh, live snapshot.
    status_gate.bump();

    // §E.4 hot-reload watch state. SIGHUP (reload_requested) is the explicit
    // "reload now" nudge; the mtime-watch makes a plain our config.toml
    // write auto-apply. Seed last_reload one window in the past so the first trigger
    // is honored immediately.
    let mut last_seen = config_mtime(&paths.config_toml);
    let mut last_reload = Instant::now()
        .checked_sub(RELOAD_DEBOUNCE)
        .unwrap_or_else(Instant::now);
    // Re-probe Accessibility periodically so GRANTING it live flips the caps loop
    // on (green dot) with no reload/restart — and revoking flips it off.
    let mut last_ax_probe = Instant::now()
        .checked_sub(AX_PROBE_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_auto_dl = Instant::now();
    let mut last_mtime_check = Instant::now()
        .checked_sub(MTIME_CHECK_INTERVAL)
        .unwrap_or_else(Instant::now);
    // Push-based config watch (FSEvents/inotify/ReadDirectoryChangesW): flips
    // `reload_requested` the instant settings.json changes, so the `stat()` below is only a
    // coarse backstop. Held for the loop's lifetime — dropping the handle stops the watch.
    let _config_watcher = crate::config_watch::spawn(&paths.config_toml, reload_requested.clone());

    while running.load(Ordering::Relaxed) {
        daemon.tick();

        if last_ax_probe.elapsed() >= AX_PROBE_INTERVAL {
            daemon.refresh_caps_gate();
            last_ax_probe = Instant::now();
        }

        // Full-auto download retry safety net: if an enabled engine's model is still
        // missing (a launch-time download failed / had no network), re-kick it without any
        // user action. Cheap + idempotent, but throttled so it's not a per-tick stat storm.
        if last_auto_dl.elapsed() >= AUTO_DL_RETRY_INTERVAL {
            auto_download_missing(&downloads, &daemon.cfg);
            last_auto_dl = Instant::now();
        }

        let hup = reload_requested.swap(false, Ordering::Relaxed);
        // Throttle the settings.json stat to MTIME_CHECK_INTERVAL instead of every 30 ms
        // tick. `current` defaults to `last_seen` on a non-check tick so a SIGHUP/RPC reload
        // (which doesn't stat) leaves `last_seen` unchanged — the next stat tick re-detects
        // any real edit normally.
        let mut current = last_seen;
        let mut mtime_changed = false;
        if last_mtime_check.elapsed() >= MTIME_CHECK_INTERVAL {
            last_mtime_check = Instant::now();
            current = config_mtime(&paths.config_toml);
            mtime_changed = should_reload_on_mtime(last_seen, current);
        }
        if (hup || mtime_changed) && debounce_ok(Instant::now(), last_reload, RELOAD_DEBOUNCE) {
            // VoiceConfig::load is fail-open (bad TOML → defaults), so a reload
            // never bricks the engine on a transient bad edit. (Documented: a
            // hand-edit with a transient bad state would reload to DEFAULTS until
            // the next valid save — matches startup behavior.)
            let new_cfg = VoiceConfig::load(&paths);
            // Switching the STT engine starts a FRESH stats accumulator — the RTF / count
            // shown in the engine's row must reflect ONLY the selected engine, never carry
            // the previous engine's samples (e.g. Parakeet's numbers lingering under System).
            let stt_engine_changed = new_cfg.resolved_stt() != daemon.cfg.resolved_stt();
            daemon.reload(&new_cfg);
            if stt_engine_changed {
                stt_stats.reset();
            }
            apply_tts_provider(&tts, &downloads, new_cfg.resolved_tts_provider());
            // Newly-activated engine (e.g. user just enabled TTS) → auto-fetch its model.
            auto_download_missing(&downloads, &new_cfg);
            // Advance the mtime watermark. On a stat-tick reload `current` is already fresh;
            // on a `hup` reload (push watcher / SIGHUP / RPC, which didn't stat) `current` is
            // stale, so stat ONCE here — otherwise the ≤3 s backstop would re-stat, see the
            // new mtime, and fire a second redundant reload for the same edit. (See the
            // `reload_watermark` unit test.)
            last_seen =
                reload_watermark(mtime_changed, current, || config_mtime(&paths.config_toml));
            last_reload = Instant::now();
        }

        std::thread::sleep(poll);
    }
    daemon.shutdown();
    // Engine is stopping: push the engineRunning→false transition so any client still
    // blocked on `WaitModelStatus` wakes now instead of waiting out its full timeout.
    status_gate.bump();

    // CONC-2: the HOSTED (in-process FFI) path returns here without process-exit,
    // so the OS does NOT reap our children for us — kill + reap the warm
    // ds-helper child explicitly, or engine_stop()/app-quit orphans it (it
    // would keep the mic/model alive after the engine "stopped"). set_enabled(false)
    // runs stop_child(): drop stdin → kill → wait → join the reader. Idempotent and
    // already the toggle-off teardown, so headless exit is unaffected.
    tts.set_enabled(false);
    // CORR-2: the lifetime totals are persisted with a debounce off the reader
    // thread, so a clean stop must flush the unwritten tail (a no-op when nothing
    // is pending) — otherwise the last few utterances of the session are lost.
    lifetime.flush();
    // Still-DETACHED on this return (not joined): the IPC server thread
    // (spawn_ipc_server), the mic-barge watcher (spawn_mic_barge_watcher), and the
    // TtsQueue worker (TtsQueue::start). They hold only Arc clones of the shared
    // state and do no external IO after the socket is removed below; under the
    // headless binary the process exits immediately, and under the hosted FFI the
    // engine is a singleton so a fresh start rebinds cleanly. Joining them would
    // need stop signals threaded through each — deferred as too invasive for this
    // conservative fix.

    // §E.4: remove the engine pidfile ONLY if it still records OUR pid — same
    // don't-clobber-a-newer-instance discipline as ds-narrate::clear_self_pid
    // (a freshly relaunched engine may have already overwritten it).
    if ds_config::read_engine_pid(&paths.engine_pid) == Some(std::process::id() as i32) {
        let _ = std::fs::remove_file(&paths.engine_pid);
    }
    // Tidy the RPC socket on clean exit (a stale file is harmless — serve()
    // unlinks it on the next start — but leaving it makes `ls` lie about state).
    let _ = std::fs::remove_file(&paths.engine_sock);
    Ok(())
}

/// A FATAL engine-startup failure, RETURNED from [`engine_run`] instead of
/// `process::exit()` so a startup failure on the in-process FFI host thread can't
/// take down the whole app. [`run_headless`] maps each variant back to the exit
/// code the standalone binary historically used, preserving headless behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// `$HOME` (and thus the runtime paths) could not be resolved. Was `exit(3)`.
    HomeUnresolved,
    /// Platform init (input/event backend) failed. Was `exit(2)`; carries detail.
    PlatformInit(String),
}

impl EngineError {
    /// The process exit code the headless binary used for this failure, kept so
    /// `run_headless` reproduces the old `process::exit` codes exactly.
    pub fn exit_code(&self) -> i32 {
        match self {
            EngineError::HomeUnresolved => 3,
            EngineError::PlatformInit(_) => 2,
        }
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::HomeUnresolved => write!(f, "cannot resolve $HOME"),
            EngineError::PlatformInit(e) => write!(f, "platform init: {e}"),
        }
    }
}

/// Headless entry: create the control flags, wire them to POSIX signals
/// (SIGTERM/SIGINT → stop, SIGHUP → reload), run the engine, then exit. This is
/// what the `dontspeakd` binary calls; the in-app host instead spawns a thread on
/// [`engine_run`] and flips the flags itself.
pub fn run_headless() {
    let running = Arc::new(AtomicBool::new(true));
    let reload_requested = Arc::new(AtomicBool::new(false));
    install_signal_handlers(running.clone(), reload_requested.clone());
    // A FATAL startup failure now RETURNS (so the in-process host survives); the
    // standalone binary maps it back to the historical exit code so its behavior is
    // unchanged (was exit(3) for $HOME, exit(2) for platform init, exit(0) clean).
    match engine_run(running, reload_requested) {
        Ok(()) => std::process::exit(0),
        Err(e) => std::process::exit(e.exit_code()),
    }
}

/// Resolve a sibling helper binary (e.g. `ds-helper`):
///   1. a sibling of THIS executable with that name (the install layout — all
///      bins land in the same `--bin` dir / `~/.local/bin`),
///   2. bare `<name>` (resolved via `$PATH`).
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

// Process-global signal flags. Each handler only ever does an atomic store,
// which is async-signal-safe. The main loop owns Arc clones; the watcher thread
// publishes these statics into those Arcs.
//   RUN_FLAG    — SIGTERM/SIGINT: graceful stop.
//   RELOAD_FLAG — SIGHUP: §E.4 explicit "reload now" nudge.
#[cfg(unix)]
static RUN_FLAG: AtomicBool = AtomicBool::new(true);
#[cfg(unix)]
static RELOAD_FLAG: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn install_signal_handlers(running: Arc<AtomicBool>, reload_requested: Arc<AtomicBool>) {
    use nix::sys::signal::{SigHandler, Signal, signal};

    // SIGTERM/SIGINT: flip RUN_FLAG. Does nothing but an atomic store (the
    // process-global `static` is reachable from the bare extern "C" handler,
    // which can carry no captured state) — async-signal-safe.
    extern "C" fn handler(_sig: nix::libc::c_int) {
        RUN_FLAG.store(false, Ordering::Relaxed);
    }
    // SIGHUP: flip RELOAD_FLAG. Same async-signal-safe single-store discipline.
    extern "C" fn reload_handler(_sig: nix::libc::c_int) {
        RELOAD_FLAG.store(true, Ordering::Relaxed);
    }

    // nix wraps `sigaction` with correct `SigHandler` typing — no function
    // pointer is laundered through `usize`, so FFI type safety is preserved.
    // A failure here is non-fatal: worst case the engine must be SIGKILLed (or
    // the SIGHUP nudge is lost and the GUI's mtime-watch reload still applies).
    unsafe {
        let h = SigHandler::Handler(handler);
        if let Err(e) = signal(Signal::SIGTERM, h) {
            log(&format!("WARN: cannot install SIGTERM handler: {e}"));
        }
        if let Err(e) = signal(Signal::SIGINT, h) {
            log(&format!("WARN: cannot install SIGINT handler: {e}"));
        }
        let rh = SigHandler::Handler(reload_handler);
        if let Err(e) = signal(Signal::SIGHUP, rh) {
            log(&format!("WARN: cannot install SIGHUP handler: {e}"));
        }
    }

    // The loop reads `running` / `reload_requested`; this watcher propagates the
    // static flips (set from the async-signal-safe handlers) back into the Arcs
    // the loop owns. SIGHUP can fire repeatedly; we coalesce with swap.
    std::thread::spawn(move || {
        while RUN_FLAG.load(Ordering::Relaxed) {
            if RELOAD_FLAG.swap(false, Ordering::Relaxed) {
                reload_requested.store(true, Ordering::Relaxed);
            }
            // Just propagates rare async-signal-safe flag flips (SIGHUP/SIGTERM) into the
            // Arcs — no need to share the 30 ms caps cadence; a stop/reload nudge tolerates
            // this latency, and it keeps this watcher from waking ~33×/s for nothing.
            std::thread::sleep(Duration::from_millis(250));
        }
        running.store(false, Ordering::Relaxed);
    });
}

#[cfg(not(unix))]
fn install_signal_handlers(_running: Arc<AtomicBool>, _reload_requested: Arc<AtomicBool>) {
    // Windows: rely on Ctrl-C default or service-stop; Phase-1 daemon runs as a
    // LaunchAgent equivalent only on macOS. There is no SIGHUP on Windows, so the
    // mtime-watch on settings.json is the ONLY reload mechanism off-unix.
    // TODO(on-target): wire a Windows reload trigger (named event / service
    // control) and a Linux systemctl reload to set `reload_requested`.
}
