//! Engine lifecycle / orchestration: the in-process entry, `engine_run`, the
//! startup wiring, the signal handlers, and `install_bin`.

use std::collections::VecDeque;
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
use crate::status::{CapsLog, EngineShared, StatusGate};
use crate::stt_test::TestSession;
use crate::tts::TtsManager;
use crate::ttsq::TtsQueue;

// ── Tunables (match the original Swift daemon) ──────────────────────────────
pub(crate) const POLL_MS: u64 = 30; // caps-state poll interval
/// §E.4 hot-reload quiet window: a TRAILING-EDGE debounce (see
/// [`config_gate::reload_tick`]) that collapses a flurry of triggers — the host's atomic
/// settings.json write AND the explicit `engine_reload()` nudge that follows it, plus
/// whatever an editor's write-twice-on-save adds — into a SINGLE reload, by waiting for
/// this long of silence after the LAST trigger rather than a fixed gap after the last
/// reload that ran. Sized with headroom over a measured 581 ms production gap between the
/// nudge and its own filesystem-watch echo for the same edit.
const RELOAD_QUIET_WINDOW: Duration = Duration::from_millis(750);
/// How often to re-probe Accessibility trust so a live grant/revoke flips the
/// caps loop without a reload (the dot follows ~this fast).
const AX_PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// How often the poll loop re-checks for a missing model to auto-download (retry safety
/// net for a launch-time download that failed / had no network). Slow — the startup +
/// reload hooks handle the common "first activation" case immediately.
const AUTO_DL_RETRY_INTERVAL: Duration = Duration::from_secs(20);
/// How often to nudge the status gate while a background download runs. Progress updates
/// (`DownloadProg.done`) don't themselves bump the status seq, so without this the app's
/// seq-gated status push would show a FROZEN "Downloading N%" that looks stuck. ~2.5 Hz is
/// smooth for the ring/percent without being a repaint storm; only fires while a fetch is active.
const DL_PROGRESS_BUMP_INTERVAL: Duration = Duration::from_millis(400);
/// Coarse `stat()` BACKSTOP for an out-of-band settings.json edit. The primary trigger is
/// the push-based [`config_watch`](crate::config_watch) filesystem watcher; this slow stat
/// only covers the rare case the watcher can't start or a filesystem drops an event. Kept
/// well under any human re-edit cadence. The explicit reload path (`engine_reload()` / the
/// Reload RPC) is independent (it sets `reload_requested` directly, read every tick).
const MTIME_CHECK_INTERVAL: Duration = Duration::from_secs(3);

/// Run the engine to completion on the CURRENT thread. The host owns the two
/// control flags: `running` (clear it → graceful stop) and `reload_requested`
/// (set it → re-read settings.json). The in-process host (the SwiftUI/Win/Linux app
/// via the `ds-core` C ABI) drives them from `engine_stop()` / `engine_reload()` —
/// this is the ONLY way the engine runs (there is no headless binary). The caps loop,
/// RPC server, and TTS queue all run from here, so whichever process calls this is
/// the one that needs the OS permissions (Accessibility / Input-Monitoring / Mic).
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

    // FATAL startup failures RETURN an error instead of process::exit(): this fn
    // runs on a background thread INSIDE the host app (the in-process FFI host),
    // where an exit() would kill the whole app. The host surfaces the Err instead.
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
        log::warn!(
            target: "engine",
            "{e} — Caps-Lock dictation is OFF until granted; \
             other subsystems (RPC/TTS/STT) run regardless."
        );
    }

    // §F: read the physical-hold threshold from settings.json. Fail-open:
    // `VoiceConfig::load` already defaults long_press_ms=600 on any error, so a
    // missing / bad settings.json still yields a working engine.
    let cfg = VoiceConfig::load(&paths);
    let long_press_ms = cfg.long_press_ms;

    // One-time PROMPT for macOS Speech-Recognition authorization when the config
    // resolves to System STT — mirrors `plat.request_permissions()` above (the same
    // "ask once at boot" shape for Accessibility). Without this, a machine that never
    // went through the explicit `set_config(stt_engine=system)` opt-in gate (e.g. the
    // default `stt_engine_ladder` picking System, unset by the user) never calls
    // `system_authorize()` at all: the recognizer sits in `Preparing` (orange) forever
    // and every Caps-Lock tap gets refused, with no path to actually become usable.
    // Re-run on every reload too (see the `Run` arm below) — the ladder can newly
    // resolve to System after boot as well (a hand-edited config.toml, or a higher rung
    // losing availability), and that path never went through `set_config` either.
    authorize_system_stt_if_needed(&cfg, &reload_requested, &running);

    log::info!(
        target: "engine",
        "engine started (poll={POLL_MS}ms long_press={long_press_ms}ms \
         stt={} debug={debug})",
        cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off")
    );

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
        log::warn!(
            target: "engine",
            "cannot write default narration spec {}: {e}",
            paths.narration_spec.display()
        );
    }

    // Converge each AI client's wiring to config.toml's declared `exclude_clients` (absent ⇒ all
    // supported). Runs OFF the boot thread so a slow client-file write never delays engine
    // startup; a steady-state boot writes nothing (the writers short-circuit on an unchanged
    // document). Diagnostics from the writers go to the host's (discarded) stderr — the engine
    // logs only a one-line exit-code summary. LIMITATION: a client installed while the engine is
    // already running (with `exclude_clients` unchanged) is not wired until the next boot.
    {
        let paths = paths.clone(); // Paths: Clone
        std::thread::spawn(move || {
            let code = ds_wire::reconcile(&paths);
            if code != 0 {
                log::warn!(target: "engine", "wire reconcile exited {code}");
            }
        });
    }

    // Single-instance guard: evict an OLDER engine BEFORE we bind the socket below.
    // launchd's KeepAlive only enforces one launchd-managed daemon — it does NOT
    // cover the engine running in-process inside the GUI host, and Windows/Linux
    // have no OS singleton at all. Since `ds_ipc::bind` unlinks + rebinds the
    // socket, a second engine would otherwise STEAL the path from a still-running
    // first one, leaving two engines that both narrate (heard as the same reply
    // spoken twice after a reinstall/upgrade). Ask the old one to exit first; this
    // is cross-platform (SIGTERM → clean shutdown on unix; TerminateProcess on
    // Windows, after which its helper self-exits on stdin EOF). No-op if none/dead.
    if let Some(old) = ds_config::evict_stale_engine(&paths.engine_pid, std::process::id()) {
        log::info!(
            target: "engine",
            "evicted stale engine pid {old} before binding the RPC socket"
        );
    }

    // §E.4: write our own pid so the NEXT engine to start can evict US + probe our
    // liveness (see evict_stale_engine above). Tolerate a write failure: eviction just
    // no-ops, so the engine keeps running either way.
    if let Err(e) = std::fs::write(&paths.engine_pid, std::process::id().to_string()) {
        log::warn!(
            target: "engine",
            "cannot write engine pidfile {}: {e}",
            paths.engine_pid.display()
        );
    }

    // `running` / `reload_requested` are owned by the caller: the in-app FFI host
    // flips them from engine_stop()/engine_reload().

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
    // Dictation-preview buffer shared between the engine (writes partials/finals,
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

    // Codex mid-turn narration: the session ids the hooks report over IPC (GreetSession /
    // MarkActive) land in this registry; the codex_stream supervisor resumes ONLY matching
    // app-server threads. Built before the IPC server so its arms can nudge it.
    let codex_sessions = crate::codex_stream::SessionRegistry::new();

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
        codex_sessions.clone(),
    );

    // Barge-in TTS the instant the mic goes active (Claude Code's own voice
    // recording is invisible to the engine otherwise), so speech never plays into
    // a live recording.
    spawn_mic_barge_watcher(ttsq.clone(), stt_active.clone(), mic_watcher.handle());

    // The Codex app-server SUBSCRIBER (mid-turn narration for `codex --remote` sessions —
    // docs/STREAMING-NARRATION.md). Self-gating: parks while `codex_stream` is off,
    // `~/.codex` is absent, or no session is registered; config is re-read per pass, so
    // no ConfigChange plumbing is needed.
    crate::codex_stream::spawn_supervisor(
        paths.clone(),
        running.clone(),
        codex_sessions,
        mic_watcher.handle(),
        ttsq.clone(),
    );

    // Full-duplex AEC env for the warm helper, decided BEFORE the boot start so the
    // child spawns with the right mode (Parakeet STT + Kokoro TTS — see docs/AEC.md).
    tts.set_full_duplex_pref(full_duplex_wanted(&cfg));
    tts.set_stt_provider_pref(helper_stt_provider(&cfg));
    // Preload STT in parallel with the TTS load only when STT is the built-in (Parakeet)
    // engine — `helper_stt_provider` resolves to "cpu" even for Off/ClaudeCode, so it
    // can't gate this.
    tts.set_stt_wanted(helper_uses_stt(&cfg));
    // Seed the TTS provider preference BEFORE the boot start below: the new model-presence
    // gate in `start_locked` resolves ANE-vs-ONNX from `spawn_prefs.provider`, and without this
    // the FIRST start would gate on `TtsManager::new`'s hardcoded "auto" default instead of the
    // config's actually-resolved provider (`apply_provider_and_autofetch` below only applies it
    // AFTER `set_enabled`). `set_provider` is a no-op beyond storing the preference while
    // stopped — its restart logic only fires once a child is already running.
    tts.set_provider(cfg.resolved_tts_provider().as_str());
    // Warm Kokoro only when TTS is on AND Kokoro is the engine (System uses `say`,
    // which needs no warm model). Blocks on the model load, but the RPC server
    // thread above is already serving.
    tts.set_enabled(helper_needed(&cfg));
    // Make the helper's resident models match the selection at boot (preload the
    // selected engine, free the other) so the UI's "loaded" is right from the start.
    // Apply the persisted execution-provider preference before the warm child
    // starts; on Windows "cuda" downloads the GPU runtime (background) then restarts
    // BOTH engines onto the GPU (the shared `provider` drives Kokoro TTS + Parakeet STT).
    // Wire the warm-child reload hook + the shutdown observer BEFORE any download can start,
    // so a model fetched here (or on a later reload / IPC request) restarts the child to load
    // it — the shared self-heal that makes a provider switch / fresh install converge without
    // a manual restart — and so a detached download's completion hook sees the SAME
    // engine-lifetime `running` flag `ds-core`'s `engine_stop()` clears (a download that
    // finishes after the engine has already been told to stop becomes a no-op instead of
    // respawning the warm child / nudging a reload on an engine the caller believes is fully
    // torn down).
    wire(
        &downloads,
        tts.clone(),
        paths.clone(),
        DownloadFlags {
            reload: reload_requested.clone(),
            running: running.clone(),
        },
    );
    // Full-auto: apply the provider and fetch EVERYTHING missing right away (no manual
    // Download button) — the CUDA runtime first when the provider calls for it, then the
    // model sets, all in parallel (`fetch_plan` pins the order). Retried on reload + the
    // slow poll tick below if it fails.
    apply_provider_and_autofetch(&tts, &downloads, &cfg);
    reconcile_helper_models(&tts, &cfg);

    // Select the STT engine from config. The factory has no silent substitution:
    // an unavailable chosen engine degrades to the same inert placeholder off/None
    // uses, never to a different engine. §E.4 below hot-reloads this box on SIGHUP
    // or a settings.json mtime change.
    let mut daemon = Engine::with_config(
        plat,
        &cfg,
        paths.pidfile.clone(),
        normalize_long_press(long_press_ms),
        Some(&paths),
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
    // helper writes and the landed final transcript are visible to the confirm panel.
    daemon.paste = paste.clone();
    // Parakeet dictation runs THROUGH the warm helper now (consolidation): rebuild
    // the stt as HelperStt now that daemon.tts is set (with_config built the default).
    daemon.stt = build_stt(
        &cfg,
        daemon.plat.clone(),
        daemon.tts.as_ref(),
        &daemon.paste,
        Some(&paths),
    );
    // Track whether that resolved to the LOCAL helper (same predicate build_stt uses:
    // helper present AND model available) so `reload` can detect a later availability flip
    // (fresh-install model download) and rebuild — see `Engine::reload`.
    daemon.stt_is_local = daemon.tts.is_some() && local_stt_available(&cfg);
    // Always-listening: build the hands-free listener up front if configured
    // (otherwise the Caps-Lock PTT path runs as before). Hot-reload toggles it
    // via Engine::reload.
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
    caps_active.store(daemon.caps_enabled, Ordering::Relaxed);
    let poll = Duration::from_millis(POLL_MS);

    // Engine is up and serving: push the engineRunning transition so a client that was
    // blocked on `WaitModelStatus` across a restart re-reads a fresh, live snapshot.
    status_gate.bump();

    // §E.4 hot-reload watch state. SIGHUP (reload_requested) is the explicit
    // "reload now" nudge; the mtime-watch makes a plain our config.toml write
    // auto-apply. `pending_reload_since` is the trailing-edge debounce's own memory of
    // an outstanding, not-yet-applied trigger (see `config_gate::reload_tick`) — `None`
    // until the first trigger arrives, so nothing needs seeding.
    let mut last_seen = config_mtime(&paths.config_toml);
    let mut pending_reload_since: Option<Instant> = None;
    // Re-probe Accessibility periodically so GRANTING it live flips the caps loop
    // on (green dot) with no reload/restart — and revoking flips it off.
    let mut last_ax_probe = Instant::now()
        .checked_sub(AX_PROBE_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_auto_dl = Instant::now();
    let mut last_dl_bump = Instant::now();
    let mut dl_was_active = false; // for the download active→ended edge (push the terminal state)
    let mut last_mtime_check = Instant::now()
        .checked_sub(MTIME_CHECK_INTERVAL)
        .unwrap_or_else(Instant::now);
    // Push-based config watch (FSEvents/inotify/ReadDirectoryChangesW): flips
    // `reload_requested` the instant settings.json changes, so the `stat()` below is only a
    // coarse backstop. Held for the loop's lifetime — dropping the handle stops the watch.
    let _config_watcher = crate::config_watch::spawn(&paths.config_toml, reload_requested.clone());
    // Set when the platform reports its caps-HID monitor stuck (see
    // `ds_platform::CapsKeyMonitor::caps_monitor_stuck`) — checked after the loop
    // exits below to relaunch the whole process once the normal graceful shutdown
    // (ds-helper killed, stats flushed, pidfile cleared) has already completed.
    let mut relaunch_after_shutdown = false;
    // Which resource triggered it (see `Engine::relaunch_reason`), captured at
    // detection time for both the immediate log below and, if the relaunch budget
    // is exhausted, the give-up message at the end of this function — NOT derived
    // from a low-level `eprintln!` in ds_platform::macos::{iohid,led}, which goes
    // to raw process stderr (a GUI-launched app's stderr typically isn't captured
    // anywhere visible) rather than this engine's own persisted `log()`. This is
    // the one line that reliably lands in dontspeak.log, so it names the resource
    // itself instead of pointing at a sibling line that may not exist anywhere.
    let mut relaunch_reason: Option<&'static str> = None;

    while running.load(Ordering::Relaxed) {
        daemon.tick();

        if last_ax_probe.elapsed() >= AX_PROBE_INTERVAL {
            daemon.refresh_caps_gate();
            last_ax_probe = Instant::now();
            // `running.swap` (not `.store`): only claim the relaunch if WE are the
            // one transitioning it true→false. If it already reads false — e.g. an
            // IPC `Request::Shutdown` (ipc.rs) landed this same instant because the
            // user quit the app — a stuck caps monitor must not override an
            // explicit quit with a relaunch; let this shutdown be a real one.
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

        // Full-auto download retry safety net: if an enabled engine's model is still
        // missing (a launch-time download failed / had no network), re-kick it without any
        // user action. Cheap + idempotent, but throttled so it's not a per-tick stat storm.
        //
        // Piggybacks the SAME throttle: a periodic `load stt`/`load tts` self-heal backstop.
        // `reconcile_helper_models` fires-and-forgets a `load`/`unload` per wanted engine —
        // idempotent (a resident model just re-confirms `STTLOADED`/`TTSLOADED`, gated by
        // `ModelSlot::transition`'s change-gating so this can't spam StatusGate) — so
        // re-sending it every interval recovers within one window from a dropped/
        // never-delivered fire-and-forget stdio write: the `load`/`unload` request arriving
        // while the helper is mid-restart, or a pipe write whose effect the sender can't
        // confirm — there's no ack/request-id in the `dontspeakd`<->`ds-helper` wire protocol.
        // NOT compensating for an internal `TtsManager`/`ModelSlot`/`SttResidencySlot` race —
        // those are structurally closed (see `model_slot.rs`/`stt_residency.rs`), not this
        // tick's job.
        if last_auto_dl.elapsed() >= AUTO_DL_RETRY_INTERVAL {
            auto_download_missing(&downloads, &daemon.cfg);
            reconcile_helper_models(&tts, &daemon.cfg);
            last_auto_dl = Instant::now();
        }

        // Live download progress + terminal transition: `DownloadProg.done` advances (and the
        // download later ENDS) without bumping the status seq, so the app's seq-gated push would
        // freeze the "Downloading N%" ring — showing a stuck ring even after the fetch failed or
        // finished. While a download-manager fetch is active (EVERY model fetch runs there,
        // Core ML sets included) nudge the gate so the percent tracks live; and bump ONCE MORE
        // on the active→ended edge so the dot leaves the ring for its real terminal state —
        // green (running) or a RED dot + failure note — instead of a stuck ring. Shared engine
        // loop ⇒ identical on all UIs.
        if last_dl_bump.elapsed() >= DL_PROGRESS_BUMP_INTERVAL {
            let downloading = downloads.lock().unwrap().any_active();
            if downloading || dl_was_active {
                status_gate.bump();
            }
            dl_was_active = downloading;
            last_dl_bump = Instant::now();
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
        let (reload_decision, next_pending) = crate::config_gate::reload_tick(
            hup || mtime_changed,
            Instant::now(),
            pending_reload_since,
            RELOAD_QUIET_WINDOW,
        );
        pending_reload_since = next_pending;
        match reload_decision {
            // A trigger is outstanding but the quiet window hasn't elapsed — nothing else
            // to do: `pending_reload_since` (just updated above) is what carries it to the
            // next tick, no need to re-arm `reload_requested` (a fresh trigger next tick
            // folds straight into that state regardless of the flag's value).
            crate::config_gate::ReloadTick::Defer => {}
            crate::config_gate::ReloadTick::Idle => {}
            crate::config_gate::ReloadTick::Run => {
                // VoiceConfig::load is fail-open (bad TOML → defaults), so a reload
                // never bricks the engine on a transient bad edit. (Documented: a
                // hand-edit with a transient bad state would reload to DEFAULTS until
                // the next valid save — matches startup behavior.)
                let new_cfg = VoiceConfig::load(&paths);
                // Switching the STT engine starts a FRESH stats accumulator — the RTF / count
                // shown in the engine's row must reflect ONLY the selected engine, never carry
                // the previous engine's samples (e.g. Parakeet's numbers lingering under System).
                let stt_engine_changed = new_cfg.resolved_stt() != daemon.cfg.resolved_stt();
                // Re-run the client-wiring reconcile ONLY when the DESIRED set actually changed,
                // so ordinary `set_config` writes (which never touch `exclude_clients`) don't churn
                // client files. `daemon.cfg` is still the OLD config here (reload overwrites it
                // below), so this diffs old→new. Off-thread like the boot trigger.
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
                // Same one-time authorize nudge as boot — a reload (not just startup) can be
                // the FIRST time the config resolves to System (a hand-edited config.toml, or
                // the ladder falling through when a higher rung stops being available), and
                // that path never goes through `set_config`'s explicit opt-in gate either.
                authorize_system_stt_if_needed(&new_cfg, &reload_requested, &running);
                // Newly-activated engine (e.g. user just enabled TTS) → apply the provider and
                // auto-fetch its model(s), CUDA runtime included when the provider calls for it.
                apply_provider_and_autofetch(&tts, &downloads, &new_cfg);
                // Advance the mtime watermark. On a stat-tick reload `current` is already fresh;
                // on a `hup` reload (push watcher / SIGHUP / RPC, which didn't stat) `current` is
                // stale, so stat ONCE here — otherwise the ≤3 s backstop would re-stat, see the
                // new mtime, and fire a second redundant reload for the same edit. (See the
                // `reload_watermark` unit test.)
                last_seen =
                    reload_watermark(mtime_changed, current, || config_mtime(&paths.config_toml));
            }
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
    // already the toggle-off teardown, so engine-stop exit is unaffected.
    tts.set_enabled(false);
    // CORR-2: the lifetime totals are persisted with a debounce off the reader
    // thread, so a clean stop must flush the unwritten tail (a no-op when nothing
    // is pending) — otherwise the last few utterances of the session are lost.
    lifetime.flush();
    // Still-DETACHED on this return (not joined): the IPC server thread
    // (spawn_ipc_server), the mic-barge watcher (spawn_mic_barge_watcher), the
    // TtsQueue worker (TtsQueue::start), and — while a permission dialog is still
    // unanswered — `authorize_system_stt_if_needed`'s thread (it checks `running` before
    // touching `reload_requested`, so it's harmless past this point, just outstanding).
    // They hold only Arc clones of the shared state and do no external IO after the
    // socket is removed below; under the hosted FFI the engine is a singleton so a fresh
    // start rebinds cleanly. Joining them would need stop signals threaded through each —
    // deferred as too invasive for this conservative fix.

    // §E.4: remove the engine pidfile ONLY if it still records OUR pid — same
    // don't-clobber-a-newer-instance discipline as ds-narrate::clear_self_pid
    // (a freshly relaunched engine may have already overwritten it).
    if ds_config::read_engine_pid(&paths.engine_pid) == Some(std::process::id() as i32) {
        let _ = std::fs::remove_file(&paths.engine_pid);
    }
    // Tidy the RPC socket on clean exit (a stale file is harmless — serve()
    // unlinks it on the next start — but leaving it makes `ls` lie about state).
    let _ = std::fs::remove_file(&paths.engine_sock);

    // Self-relaunch, requested above by the caps-HID-stuck check: spawn a fresh
    // instance of the CURRENT executable and exit this process outright. Safe to
    // exit hard here (skipping the host app's normal AppKit quit path) because
    // EVERYTHING that path would have done — kill ds-helper, flush stats, clear the
    // pidfile, remove the socket — has already run above, on this same thread, in
    // the ordinary shutdown sequence every quit takes. `engine_run` normally
    // returns `Ok(())` here instead of exiting so a startup/runtime failure on this
    // in-process FFI host thread can't take the whole app down (see `EngineError`
    // above) — a self-requested relaunch is the one case where taking the whole
    // process down IS the point.
    //
    // Deliberately NOT `dontspeak::engine_launch::launch_host()` (`open -g -b
    // app.dontspeak.org`): that helper is for "nothing is running yet, start the
    // host app" (an MCP shim's cold-start path). Here the OLD instance is still
    // mid-exit when the replacement is spawned — `open -b` resolving the same
    // bundle id could just reactivate the dying instance instead of starting a
    // truly fresh process, which is the one thing this relaunch must guarantee.
    // A direct re-exec of this same binary sidesteps that. (Losing LaunchServices
    // frontmost-activation doesn't matter either: the app runs with
    // `NSApp.setActivationPolicy(.accessory)` — no Dock icon, nothing to "front."
    // Losing argv doesn't matter: nothing here reads `CommandLine.arguments`.)
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
        match relaunched {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                // Every resource the normal quit path would have released is ALREADY
                // gone at this point (ds-helper killed, socket/pidfile removed) — so
                // this is not "stay up as a host," it's staying alive with nothing
                // left running, on the theory that a visibly-dead process (no menu
                // bar icon at all) is worse than a silent one a user can still find
                // and manually relaunch. Expected to be unreachable in practice
                // (current_exe/spawn failing on an already-running binary implies
                // something like the executable vanishing from disk or the process
                // table being exhausted).
                log::info!(
                    target: "engine",
                    "relaunch failed ({e}) — NOT exiting; this instance has already released \
                     its resources (no RPC, no caps monitor) and can't serve as a fallback \
                     host, but exiting with nothing left to bring the app back is worse"
                );
            }
        }
    }
    Ok(())
}

/// Spawn the one-time macOS Speech-Recognition authorization attempt for System STT, IF
/// this resolution actually needs it ([`config_gate::system_stt_needs_authorization`]) —
/// so an already-`Ready` (or off-macOS, where it's never selected) config never pays for
/// a spurious thread +, on macOS <26, a real on-device smoke-transcribe. `system_authorize`
/// BLOCKS on the OS permission flow, so it runs off this thread; on completion it nudges
/// `reload_requested` (the SAME flag the download self-heal in `downloads.rs` uses) so
/// `Engine::reload`'s `local_avail_flipped` check picks up a freshly granted (or freshly
/// denied) recognizer without a restart. Skips the nudge if `running` has already gone
/// false — the engine may have been asked to stop while this thread was blocked on the
/// permission dialog, exactly the race `downloads.rs`'s own `shutdown` observer guards
/// against for its detached completion hooks. Called from BOTH boot (`engine_run`, once)
/// and every config reload (the ladder can newly resolve to System after boot too, via a
/// hand-edited config.toml or a higher rung losing availability) — `system_authorize`
/// otherwise runs ONLY through `set_config`'s explicit `AuthorizeSystemStt` opt-in gate
/// (`dontspeak::tools::call_set_config`), which a config that resolves to System via the
/// ladder alone never goes through.
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

/// Cap on relaunches over the caps-HID-stuck condition within [`CAPS_RELAUNCH_WINDOW`] —
/// a persistent (not per-process) denial must not turn into a silent, unbounded
/// quit/relaunch loop that bounces the menu-bar icon and cold-reloads the STT model
/// every few seconds forever. Small marker file in the state dir (NOT an env var passed
/// to the child): the relaunch re-execs this same binary directly, which does inherit
/// env, but a marker on disk is simplest to reason about and survives regardless of
/// exactly how the child gets started.
const MAX_CAPS_RELAUNCHES: u32 = 3;
/// A relaunch streak older than this doesn't count toward the cap — it's very unlikely
/// to be the same stuck episode, so a fresh count is the right call.
const CAPS_RELAUNCH_WINDOW: Duration = Duration::from_secs(120);

/// Read-modify-write the relaunch-streak marker; returns whether the budget is now
/// exhausted (in which case the marker is left as-is — NOT bumped — so the guard stays
/// tripped rather than resetting on every subsequent stuck detection while dead).
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
        // Outside the window, treat as no prior streak — a much-later, unrelated
        // stuck episode shouldn't inherit an old count.
        .filter(|(_, at)| now.saturating_sub(*at) <= CAPS_RELAUNCH_WINDOW.as_secs())
        .map(|(count, _)| count)
        .unwrap_or(0);
    if prior_count >= MAX_CAPS_RELAUNCHES {
        return true;
    }
    let _ = std::fs::write(&marker, format!("{} {now}", prior_count + 1));
    false
}

/// A FATAL engine-startup failure, RETURNED from [`engine_run`] instead of
/// `process::exit()` so a startup failure on the in-process FFI host thread can't
/// take down the whole app. The host logs/surfaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// `$HOME` (and thus the runtime paths) could not be resolved.
    HomeUnresolved,
    /// Platform init (input/event backend) failed; carries detail.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A tempdir-rooted `Paths` with `state_dir` (where the relaunch-guard marker
    /// lives) actually created — `caps_relaunch_budget_exhausted` silently no-ops
    /// its `fs::write` if the parent dir is missing, which would make every case
    /// here look like "budget never exhausts" for the wrong reason.
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
