//! codex_stream tests — no real codex, no real `$HOME`, no network: `Paths::rooted_at`
//! tempdirs, a scripted `tungstenite` SERVER on a loopback `TcpListener` playing
//! app-server on every platform, and the pure pieces (coalescer, endpoint resolution, the
//! daemon-start decision, the orphan sweep) driven directly.

use super::*;

#[test]
fn pending_resume_suppresses_duplicate_resolution_work() {
    let now = Instant::now();
    let mut resolve = HashMap::new();
    resolve.insert(
        "session-1".to_string(),
        ResolveState {
            tries: 0,
            next_try: now,
            negative: false,
        },
    );
    let mut pending: HashMap<i64, Pending> = HashMap::new();
    let resumed = HashMap::new();
    assert!(resolution_due(
        "session-1",
        &resolve["session-1"],
        &pending,
        now
    ));
    assert!(resume_wanted(
        "session-1",
        "session-1",
        &resolve,
        &resumed,
        &pending
    ));

    pending.insert(
        1,
        Pending::Resume {
            thread_id: "session-1".to_string(),
            session: "session-1".to_string(),
            sent_at: now,
        },
    );

    assert!(
        !resolution_due("session-1", &resolve["session-1"], &pending, now),
        "a pending resume must not trigger relisting"
    );
    assert!(
        !resume_wanted("session-1", "session-1", &resolve, &resumed, &pending),
        "a pending resume must not be enqueued again from a loaded-list reply"
    );

    // The `request_timeout` sweep in `run_attached` is what actually removes a stale
    // `Pending::Resume` entry (there is no separate in-flight set to fall out of sync) —
    // once it's gone from `pending`, resolution is due again.
    pending.remove(&1);
    assert!(resolution_due(
        "session-1",
        &resolve["session-1"],
        &pending,
        now
    ));
    assert!(resume_wanted(
        "session-1",
        "session-1",
        &resolve,
        &resumed,
        &pending
    ));
}

#[test]
fn resume_in_flight_is_keyed_by_session_not_request_id() {
    let pending: HashMap<i64, Pending> = HashMap::from([
        (
            1,
            Pending::Resume {
                thread_id: "t1".to_string(),
                session: "session-a".to_string(),
                sent_at: Instant::now(),
            },
        ),
        (
            2,
            Pending::LoadedList {
                sent_at: Instant::now(),
            },
        ),
    ]);
    assert!(resume_in_flight("session-a", &pending));
    assert!(!resume_in_flight("session-b", &pending));
}

// ── Pure pieces ───────────────────────────────────────────────────────────────────

#[test]
fn coalescer_flushes_on_newline_age_and_completed() {
    let mut c = Coalescer::new();
    let t0 = Instant::now();
    // No newline yet → buffered, nothing out.
    assert!(c.on_delta("s", "i1", "> Spoken", t0).is_none());
    // Newline → flush: one Delta batch with the WHOLE pending text, seq 0.
    let (sess, batch) = c.on_delta("s", "i1", " line.\nBody", t0).expect("flush");
    assert_eq!(sess, "s");
    assert_eq!(
        batch,
        StreamBatch {
            key: "i1".into(),
            payload: BatchPayload::Delta {
                index: Some(0),
                text: "> Spoken line.\nBody".into(),
            },
            is_final: false,
        }
    );
    // More quiet text: age-flush picks it up (seq advanced to 1), newline not required.
    assert!(c.on_delta("s", "i1", " more", t0).is_none());
    let aged = c.flush_aged(
        t0 + Duration::from_secs(1),
        Duration::from_millis(150),
        None,
    );
    assert_eq!(aged.len(), 1);
    assert_eq!(
        aged[0].1.payload,
        BatchPayload::Delta {
            index: Some(1),
            text: " more".into()
        }
    );
    // Completed: buffer dropped, one final CUMULATIVE batch with the authoritative text.
    let (_, fin) = c.on_completed("s", "i1", "> Spoken line.\n\nBody more.");
    assert!(fin.is_final);
    assert_eq!(
        fin.payload,
        BatchPayload::Cumulative {
            text: "> Spoken line.\n\nBody more.".into()
        }
    );
    // Nothing left to age-flush for that item.
    assert!(
        c.flush_aged(t0 + Duration::from_secs(9), Duration::ZERO, None)
            .is_empty()
    );
}

#[test]
fn coalescer_turn_flush_scopes_to_one_session() {
    let mut c = Coalescer::new();
    let t0 = Instant::now();
    assert!(c.on_delta("s1", "i1", "alpha", t0).is_none());
    assert!(c.on_delta("s2", "i2", "beta", t0).is_none());
    // Scoped flush (turn/completed for s1): only s1's buffer drains, regardless of age.
    let flushed = c.flush_aged(t0, Duration::ZERO, Some("s1"));
    assert_eq!(flushed.len(), 1);
    assert_eq!(flushed[0].0, "s1");
    // s2's buffer is still pending.
    let rest = c.flush_aged(t0 + Duration::from_secs(1), Duration::ZERO, None);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].0, "s2");
}

#[test]
fn coalescer_drop_session_clears_partial_buffers() {
    // Eviction path: a session's thread disappears from the daemon while an item is
    // mid-stream (deltas received, no item/completed). Without drop_session the stale
    // buffer survives and could produce a spurious utterance if the same session is
    // re-resumed on a different thread. drop_session must clear ALL of that session's
    // buffers while leaving other sessions untouched.
    let mut c = Coalescer::new();
    let t0 = Instant::now();
    // Two sessions with partial buffers (no newline, no completion).
    assert!(
        c.on_delta("s1", "i1", "> Evicted session line", t0)
            .is_none()
    );
    assert!(
        c.on_delta("s2", "i2", "> Surviving session line", t0)
            .is_none()
    );
    // Evict s1.
    c.drop_session("s1");
    // s1's buffers are gone: a turn-flush for s1 produces nothing.
    assert!(c.flush_aged(t0, Duration::ZERO, Some("s1")).is_empty());
    // s2's buffer survives intact.
    let rest = c.flush_aged(t0 + Duration::from_secs(1), Duration::ZERO, None);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].0, "s2");
    // Dropping a session that was never in the coalescer is a no-op.
    c.drop_session("unknown");
    assert!(!c.bufs.is_empty());
}

#[cfg(unix)]
#[test]
fn control_socket_path_prefers_env_over_codex_dir() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    // No env → $HOME/.codex/app-server-control/app-server-control.sock.
    let p = control_socket_path(None, &paths);
    assert_eq!(
        p,
        paths
            .codex_dir
            .join("app-server-control/app-server-control.sock")
    );
    // Env set (passed as a VALUE — tests never mutate process env) → wins.
    let env_home = dir.path().join("custom-codex-home");
    let env_os: std::ffi::OsString = env_home.clone().into();
    let p = control_socket_path(Some(env_os.as_os_str()), &paths);
    assert_eq!(
        p,
        env_home.join("app-server-control/app-server-control.sock")
    );
    // Empty env value is ignored (unset-like).
    let empty = std::ffi::OsString::new();
    let p = control_socket_path(Some(empty.as_os_str()), &paths);
    assert!(p.starts_with(&paths.codex_dir));
}

#[test]
fn ws_url_parses_and_endpoint_resolution_prefers_the_override() {
    assert_eq!(
        parse_ws_url("ws://127.0.0.1:4550").as_deref(),
        Some("127.0.0.1:4550")
    );
    assert_eq!(
        parse_ws_url("ws://127.0.0.1:4550/path").as_deref(),
        Some("127.0.0.1:4550")
    );
    assert_eq!(
        parse_ws_url("wss://127.0.0.1:4550"),
        None,
        "no TLS endpoints"
    );
    assert_eq!(parse_ws_url("127.0.0.1:4550"), None, "scheme required");
    assert_eq!(parse_ws_url("ws://"), None);

    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    assert!(
        resolve_endpoint("ws://10.0.0.5:9", None, &paths).is_none(),
        "plaintext non-loopback endpoints must be rejected"
    );
    #[cfg(unix)]
    match resolve_endpoint("", None, &paths) {
        Some(Endpoint::Unix(sock)) => assert!(sock.ends_with("app-server-control.sock")),
        _ => panic!("default must be the unix control socket"),
    }
    #[cfg(windows)]
    match resolve_endpoint("", None, &paths) {
        Some(Endpoint::Tcp(host)) => assert_eq!(host, DEFAULT_WINDOWS_APP_SERVER),
        _ => panic!("default must be the loopback Windows launcher endpoint"),
    }
    // A malformed override resolves to nothing (park; never silently fall back to a
    // socket the user tried to steer away from).
    assert!(resolve_endpoint("http://nope", None, &paths).is_none());
}

#[test]
fn tcp_auto_start_is_loopback_only() {
    assert!(can_auto_start_tcp("127.0.0.1:4500"));
    assert!(can_auto_start_tcp("[::1]:4500"));
    assert!(!can_auto_start_tcp("0.0.0.0:4500"));
    assert!(!can_auto_start_tcp("192.168.1.10:4500"));
    assert!(!can_auto_start_tcp("example.com:4500"));
}

#[test]
fn should_start_unix_server_decision_table() {
    assert!(should_start_unix_server(true, true, true, false));
    assert!(
        !should_start_unix_server(false, true, true, false),
        "not opted in"
    );
    assert!(
        !should_start_unix_server(true, false, true, false),
        "the endpoint connected or failed in a way startup cannot repair"
    );
    assert!(
        !should_start_unix_server(true, true, false, false),
        "no codex binary"
    );
    assert!(
        !should_start_unix_server(true, true, true, true),
        "an owned child is already binding the endpoint"
    );
}

#[test]
fn direct_app_server_command_uses_the_requested_listener() {
    let command = direct_app_server_command(Path::new("codex-bin"), "unix:///tmp/codex.sock");
    assert_eq!(command.get_program(), "codex-bin");
    assert_eq!(
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        ["app-server", "--listen", "unix:///tmp/codex.sock"]
    );
}

#[test]
fn next_backoff_paces_every_failure_path() {
    // Unstable loss / attach failure: doubles toward the ceiling — never a zero delay.
    let mut b = BACKOFF_FLOOR;
    b = next_backoff(false, b);
    assert_eq!(b, BACKOFF_FLOOR * 2);
    for _ in 0..10 {
        b = next_backoff(false, b);
        assert!(b >= BACKOFF_FLOOR, "no zero-sleep path");
    }
    assert_eq!(b, BACKOFF_CEIL, "capped at the ceiling");
    // A STABLE attachment's loss earns the prompt floor retry (still non-zero).
    assert_eq!(next_backoff(true, b), BACKOFF_FLOOR);
}

#[test]
fn launch_waiter_uses_short_retries_during_background_backoff() {
    assert_eq!(
        retry_delay(BACKOFF_CEIL, true),
        LAUNCH_RETRY_DELAY,
        "a synchronous launcher must not inherit the background ceiling"
    );
    assert_eq!(
        retry_delay(BACKOFF_CEIL, false),
        BACKOFF_CEIL,
        "background retries retain their capped backoff"
    );
}

#[test]
fn auto_start_bypasses_the_no_session_bootstrap_park() {
    assert!(should_park_supervisor(true, true, false, false));
    assert!(
        !should_park_supervisor(true, true, false, true),
        "auto-start must create the server before a remote session can fire its first hook"
    );
    assert!(should_park_supervisor(false, true, true, true));
    assert!(should_park_supervisor(true, false, true, true));
}

#[test]
fn resolve_codex_bin_managed_install_honors_codex_home() {
    // The managed-install fallback lives under the CODEX HOME (the same
    // $CODEX_HOME-or-codex_dir resolution the control socket uses), not $HOME/.codex.
    let home = tempfile::tempdir().unwrap();
    let codex_home = tempfile::tempdir().unwrap();
    // A name that exists neither on PATH nor in the system fallback dirs.
    let name = if cfg!(windows) {
        "ds-test-codex-b1n.exe"
    } else {
        "ds-test-codex-b1n"
    };
    let managed = codex_home.path().join("packages/standalone/current");
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::write(managed.join(name), "x").unwrap();
    let mut paths = Paths::rooted_at(home.path());
    paths.codex_dir = codex_home.path().to_path_buf();
    assert_eq!(resolve_codex_bin(name, &paths), Some(managed.join(name)));
    // Not found anywhere → None (the caller warns; nothing is spawned).
    assert_eq!(
        resolve_codex_bin(
            if cfg!(windows) {
                "ds-test-n0t-there.exe"
            } else {
                "ds-test-n0t-there"
            },
            &paths,
        ),
        None
    );
}

#[test]
fn launcher_binary_wins_when_engine_discovery_cannot_find_codex() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(home.path());
    let launcher_bin = home.path().join("launcher-only-codex");
    std::fs::write(&launcher_bin, b"fixture").unwrap();
    let reg = SessionRegistry::new();
    let waiter = reg.clone();
    let expected = launcher_bin.clone();
    let thread =
        std::thread::spawn(move || waiter.ensure_remote(launcher_bin, Duration::from_secs(2)));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !reg.launch_requested() && Instant::now() < deadline {
        std::thread::yield_now();
    }

    assert_eq!(
        resolve_launch_bin(&reg, "codex-not-visible-to-engine", &paths),
        Some(expected)
    );
    reg.launch_failed("test complete");
    assert_eq!(thread.join().unwrap().unwrap_err(), "test complete");
}

#[test]
fn unix_start_kind_uses_owned_server_for_homebrew_and_managed_daemon_for_standalone() {
    let codex_home = tempfile::tempdir().unwrap();
    let standalone = codex_home.path().join("packages/standalone/current/codex");
    std::fs::create_dir_all(standalone.parent().unwrap()).unwrap();
    std::fs::write(&standalone, b"fixture").unwrap();

    assert_eq!(
        unix_start_kind(&standalone, codex_home.path()),
        UnixStartKind::ManagedDaemon
    );

    #[cfg(unix)]
    {
        let configured_symlink = codex_home.path().join("configured-codex");
        std::os::unix::fs::symlink(&standalone, &configured_symlink).unwrap();
        assert_eq!(
            unix_start_kind(&configured_symlink, codex_home.path()),
            UnixStartKind::ManagedDaemon,
            "a configured symlink to the standalone payload remains managed"
        );
    }

    let homebrew = codex_home.path().join("homebrew/bin/codex");
    std::fs::create_dir_all(homebrew.parent().unwrap()).unwrap();
    std::fs::write(&homebrew, b"fixture").unwrap();
    assert_eq!(
        unix_start_kind(&homebrew, codex_home.path()),
        UnixStartKind::OwnedServer
    );
}

#[cfg(unix)]
#[test]
fn owned_unix_app_server_stops_its_process_group_on_drop() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("fake-codex");
    std::fs::write(
        &fixture,
        b"#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();

    let server = start_unix_app_server(&fixture, &dir.path().join("control.sock")).unwrap();
    let pgid = server.child.id() as i32;
    assert!(ds_proc::group_alive(pgid));
    drop(server);
    let deadline = Instant::now() + Duration::from_secs(1);
    while ds_proc::group_alive(pgid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !ds_proc::group_alive(pgid),
        "dropping the owned server must not orphan its process group"
    );
}

#[cfg(windows)]
#[test]
fn resolve_codex_bin_finds_the_native_npm_payload_on_windows() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(home.path());
    let roaming = paths.home.join("AppData/Roaming");
    let package = if cfg!(target_arch = "aarch64") {
        "@openai/codex-win32-arm64"
    } else {
        "@openai/codex-win32-x64"
    };
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-pc-windows-msvc"
    } else {
        "x86_64-pc-windows-msvc"
    };
    let bin = roaming
        .join("npm/node_modules/@openai/codex/node_modules")
        .join(package)
        .join("vendor")
        .join(target)
        .join("bin/codex.exe");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"test").unwrap();

    assert_eq!(
        resolve_codex_bin(ds_config::WiredAgent::Codex.as_str(), &paths),
        Some(bin)
    );
}

#[test]
fn sweep_removes_only_old_narrate_display_files() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    let state_dir = paths.narrate_pid.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&state_dir).unwrap();
    let old_json = state_dir.join("narrate-display-old.json");
    let old_lock = state_dir.join("narrate-display-old.lock");
    let fresh = state_dir.join("narrate-display-fresh.json");
    let foreign = state_dir.join("stats.toml");
    for p in [&old_json, &old_lock, &fresh, &foreign] {
        std::fs::write(p, "x").unwrap();
    }
    // Age the two "old" files past the 7-day bar (mtime is the sweep's signal).
    let ancient = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 3600);
    for p in [&old_json, &old_lock] {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(ancient).unwrap();
    }
    sweep_orphaned_state(&paths);
    assert!(!old_json.exists(), "stale state file swept");
    assert!(!old_lock.exists(), "stale lock swept");
    assert!(fresh.exists(), "fresh state file kept (live session)");
    assert!(foreign.exists(), "non-narrate files never touched");
}

#[test]
fn registry_nudges_wake_snapshot_and_prune() {
    let reg = SessionRegistry::new();
    let (sessions, epoch0) = reg.snapshot();
    assert!(sessions.is_empty());
    reg.nudge("s1");
    reg.nudge(""); // blank ids are ignored
    reg.nudge("   ");
    let (sessions, epoch1) = reg.snapshot();
    assert_eq!(sessions, vec!["s1".to_string()]);
    assert!(epoch1 > epoch0, "a nudge bumps the epoch");
    // wait_change returns immediately when the epoch already moved.
    let woken = reg.wait_change(epoch0, Duration::from_millis(10));
    assert_eq!(woken, epoch1);
    // A re-nudge of the SAME session still bumps (it re-arms negative-cached resolution).
    reg.nudge("s1");
    let (_, epoch2) = reg.snapshot();
    assert!(epoch2 > epoch1);
    // Prune drops entries idle past the TTL.
    reg.prune_older_than(Duration::ZERO);
    assert!(reg.snapshot().0.is_empty());
    // remove() is idempotent.
    reg.remove("s1");
}

#[test]
fn launcher_waits_for_an_attached_endpoint_and_reuses_it() {
    let reg = SessionRegistry::new();
    let waiter = reg.clone();
    let launcher_bin = PathBuf::from("/launcher/codex");
    let expected_bin = launcher_bin.clone();
    let thread =
        std::thread::spawn(move || waiter.ensure_remote(launcher_bin, Duration::from_secs(2)));

    let deadline = Instant::now() + Duration::from_secs(1);
    while !reg.launch_requested() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        reg.launch_requested(),
        "the waiting launcher must wake the supervisor"
    );
    assert_eq!(reg.launcher_bin(), Some(expected_bin));
    reg.launch_ready("ws://127.0.0.1:4500".to_string());
    assert_eq!(thread.join().unwrap().unwrap(), "ws://127.0.0.1:4500");
    assert_eq!(
        reg.ensure_remote(PathBuf::from("/other/codex"), Duration::ZERO)
            .unwrap(),
        "ws://127.0.0.1:4500",
        "a second launcher reuses the initialized observer"
    );
    assert_eq!(
        reg.launcher_bin(),
        Some(PathBuf::from("/other/codex")),
        "the latest resolved binary must remain available for reconnects"
    );
    reg.launch_detached();
    assert!(
        reg.ensure_remote(PathBuf::from("/launcher/codex"), Duration::ZERO)
            .is_err()
    );
}

#[test]
fn launcher_receives_a_supervisor_failure_without_waiting_for_timeout() {
    let reg = SessionRegistry::new();
    let waiter = reg.clone();
    let thread = std::thread::spawn(move || {
        waiter.ensure_remote(PathBuf::from("/launcher/codex"), Duration::from_secs(2))
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !reg.launch_requested() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    reg.launch_failed("codex binary missing");
    assert_eq!(thread.join().unwrap().unwrap_err(), "codex binary missing");
}

#[test]
fn launcher_interrupts_an_in_flight_background_retry_wait() {
    use std::sync::mpsc;

    let reg = SessionRegistry::new();
    let running = Arc::new(AtomicBool::new(true));
    let paced_reg = reg.clone();
    let paced_running = running.clone();
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let pace_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let started = Instant::now();
        pace(&paced_running, &paced_reg, Duration::from_secs(5), false);
        started.elapsed()
    });

    started_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let waiter = reg.clone();
    let launch_thread = std::thread::spawn(move || {
        waiter.ensure_remote(PathBuf::from("/launcher/codex"), Duration::from_secs(2))
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !reg.launch_requested() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(reg.launch_requested(), "launcher did not register its wait");

    let elapsed = pace_thread.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(1),
        "new launch request remained hidden behind retry backoff for {elapsed:?}"
    );
    reg.launch_failed("test complete");
    assert_eq!(launch_thread.join().unwrap().unwrap_err(), "test complete");
}

#[test]
fn session_nudge_does_not_interrupt_retry_backoff() {
    let reg = SessionRegistry::new();
    let running = AtomicBool::new(true);
    let nudger = reg.clone();
    let nudge_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        nudger.nudge("unrelated-session");
    });
    let started = Instant::now();
    pace(&running, &reg, Duration::from_millis(150), false);
    nudge_thread.join().unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "ordinary session activity must not create a hot reconnect loop"
    );
}

#[test]
fn retry_wait_still_observes_shutdown() {
    let reg = SessionRegistry::new();
    let running = Arc::new(AtomicBool::new(true));
    let stopper = running.clone();
    let stop_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        stopper.store(false, Ordering::Relaxed);
    });
    let started = Instant::now();
    pace(&running, &reg, Duration::from_secs(5), false);
    stop_thread.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown remained hidden behind retry backoff"
    );
}

#[cfg(unix)]
#[test]
fn unix_socket_websocket_transport_still_initializes() {
    use serde_json::{Value, json};
    use std::os::unix::net::{UnixListener, UnixStream};
    use tungstenite::Message;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("transport.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        loop {
            if let Message::Text(text) = ws.read().unwrap() {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                if request["method"] == "initialize" {
                    ws.send(Message::text(
                        json!({ "id": request["id"], "result": {} }).to_string(),
                    ))
                    .unwrap();
                } else if request["method"] == "initialized" {
                    break;
                }
            }
        }
    });
    let stream = UnixStream::connect(&socket).unwrap();
    let mut client = WsClient::handshake(stream, "ws://localhost/").unwrap();
    client.initialize(Duration::from_secs(2)).unwrap();
    server.join().unwrap();
}

// ── The scripted app-server (cross-platform loopback WebSocket) ─────────────────────────────────

mod attached {
    use super::*;
    use serde_json::{Value, json};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use tungstenite::{Message, WebSocket};

    /// Reply text the scripted server streams. Digest: two spoken lines.
    const REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.";

    fn short_tunables() -> Tunables {
        Tunables {
            cfg_refresh: Duration::from_millis(100),
            resolve_retry: Duration::from_millis(100),
            resolve_tries: 3,
            relist: Duration::from_millis(250),
            idle_ttl: Duration::from_secs(3600),
            flush_age: Duration::from_millis(50),
            // Keep ordinary scripted requests alive longer than the 150 ms socket read
            // tick. Under CI load, expiring them sooner can discard a valid response
            // before the client thread gets scheduled to read it.
            request_timeout: Duration::from_secs(5),
        }
    }

    fn retry_tunables() -> Tunables {
        Tunables {
            request_timeout: Duration::from_millis(100),
            ..short_tunables()
        }
    }

    /// Read frames until the next JSON TEXT frame, skipping the client's `initialized`
    /// notification (and anything else without an id/method of interest).
    fn next_request(ws: &mut WebSocket<TcpStream>) -> Value {
        loop {
            match ws.read().expect("server read") {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(t.as_str()).expect("json frame");
                    if v["method"] == "initialized" {
                        continue; // the post-initialize notification — not a request
                    }
                    return v;
                }
                _ => continue,
            }
        }
    }

    fn send(ws: &mut WebSocket<TcpStream>, v: Value) {
        ws.send(Message::text(v.to_string())).expect("server send");
    }

    /// Handle the opening `initialize` request (asserting the opt-out list rides along).
    fn serve_initialize(ws: &mut WebSocket<TcpStream>) {
        let req = next_request(ws);
        assert_eq!(req["method"], "initialize", "first request is initialize");
        assert!(
            req["params"]["capabilities"]["optOutNotificationMethods"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "initialize opts out of the noisy delta streams"
        );
        send(ws, json!({ "id": req["id"], "result": {} }));
    }

    /// Serve one loaded-list request with `threads`, then a resume request per expected
    /// thread (responding OK).
    fn serve_list_and_resumes(ws: &mut WebSocket<TcpStream>, threads: &[&str], resumes: usize) {
        let req = next_request(ws);
        assert_eq!(req["method"], "thread/loaded/list");
        send(
            ws,
            json!({ "id": req["id"], "result": { "data": threads } }),
        );
        for _ in 0..resumes {
            let req = next_request(ws);
            assert_eq!(req["method"], "thread/resume");
            let tid = req["params"]["threadId"].as_str().unwrap().to_string();
            send(
                ws,
                json!({ "id": req["id"], "result": { "thread": { "id": tid, "sessionId": tid } } }),
            );
        }
    }

    fn delta(thread: &str, item: &str, text: &str) -> Value {
        json!({ "method": "item/agentMessage/delta", "params": {
            "threadId": thread, "turnId": "turn_1", "itemId": item, "delta": text } })
    }

    fn completed(thread: &str, item: &str, text: &str) -> Value {
        json!({ "method": "item/completed", "params": {
            "threadId": thread, "turnId": "turn_1", "completedAtMs": 1,
            "item": { "type": "agentMessage", "id": item, "text": text } } })
    }

    /// Run `run_attached` against a freshly-connected client, collecting utterances.
    fn attach_once(
        addr: SocketAddr,
        paths: &Paths,
        registry: &SessionRegistry,
        spoken: &std::sync::Arc<Mutex<Vec<(String, String)>>>,
    ) -> Result<Detach, String> {
        attach_once_with(addr, paths, registry, spoken, &short_tunables(), None)
    }

    fn attach_once_with(
        addr: SocketAddr,
        paths: &Paths,
        registry: &SessionRegistry,
        spoken: &std::sync::Arc<Mutex<Vec<(String, String)>>>,
        tunables: &Tunables,
        connected_endpoint: Option<&str>,
    ) -> Result<Detach, String> {
        let stream = TcpStream::connect(addr).expect("client connect");
        let mut ws =
            WsClient::handshake(stream, &format!("ws://{addr}/")).expect("client handshake");
        ws.initialize(Duration::from_secs(5)).expect("initialize");
        let running = AtomicBool::new(true);
        let spoken = spoken.clone();
        let mut speak = move |session: &str, utterance: &NarrationUtterance| {
            spoken
                .lock()
                .unwrap()
                .push((session.to_string(), utterance.text.clone()));
            Ok(())
        };
        run_attached(
            &mut ws,
            paths,
            &running,
            registry,
            &|| false,
            &mut speak,
            tunables,
            connected_endpoint,
        )
    }

    #[test]
    fn attach_streams_narration_and_a_reconnect_replay_never_double_speaks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap(); // gate: codex present
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let addr = listener.local_addr().unwrap();

        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        // Connection 1: resolve + resume sess-1, stream deltas, then the authoritative
        // final text, then hang up mid-session (a daemon restart).
        let server = std::thread::spawn({
            let listener = listener.try_clone().unwrap();
            move || {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                serve_initialize(&mut ws);
                serve_list_and_resumes(&mut ws, &["sess-1"], 1);
                send(&mut ws, delta("sess-1", "item_1", "> First point.\n"));
                send(&mut ws, delta("sess-1", "item_1", "\nDetail.\n"));
                send(&mut ws, completed("sess-1", "item_1", REPLY));
                // Give the client a beat to drain, then drop the connection.
                std::thread::sleep(Duration::from_millis(400));
            }
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        server.join().unwrap();
        assert!(detach.is_err(), "server hangup surfaces as a disconnect");
        assert_eq!(
            *spoken.lock().unwrap(),
            vec![
                ("sess-1".to_string(), "First point.".to_string()),
                ("sess-1".to_string(), "Second point.".to_string()),
            ],
            "both digest lines spoken, in order, exactly once"
        );
        // The witness came for free — Stop stays silent for this session.
        assert!(ds_narrate::witness_exists(&paths, "sess-1"));
        assert!(
            ds_narrate::stop_utterances(Some(REPLY), true, true, false, true).is_empty(),
            "streamed session ⇒ Stop silent"
        );

        // Connection 2 (the reconnect): the server REPLAYS the same completed item. The
        // on-disk high-water mark must keep it silent — the never-double-speak pin.
        registry.nudge("sess-1"); // re-arm resolution (fresh connection state)
        let server = std::thread::spawn({
            let listener = listener.try_clone().unwrap();
            move || {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                serve_initialize(&mut ws);
                serve_list_and_resumes(&mut ws, &["sess-1"], 1);
                send(&mut ws, completed("sess-1", "item_1", REPLY));
                std::thread::sleep(Duration::from_millis(400));
            }
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        server.join().unwrap();
        assert!(detach.is_err());
        assert_eq!(
            spoken.lock().unwrap().len(),
            2,
            "replayed final batch after reconnect re-speaks NOTHING"
        );
    }

    #[test]
    fn loaded_list_timeout_retries_on_the_same_connection() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            let first = next_request(&mut ws);
            assert_eq!(first["method"], "thread/loaded/list");
            // Drop only this response while leaving the WebSocket open.
            let retry = next_request(&mut ws);
            assert_eq!(retry["method"], "thread/loaded/list");
            send(
                &mut ws,
                json!({ "id": retry["id"], "result": { "data": ["sess-1"] } }),
            );
            let resume = next_request(&mut ws);
            assert_eq!(resume["method"], "thread/resume");
            send(
                &mut ws,
                json!({ "id": resume["id"], "result": { "thread": {
                    "id": "sess-1", "sessionId": "sess-1" } } }),
            );
            send(
                &mut ws,
                completed("sess-1", "item-1", "> Recovered.\n\nBody."),
            );
            std::thread::sleep(Duration::from_millis(250));
        });
        let detach = attach_once_with(addr, &paths, &registry, &spoken, &retry_tunables(), None);
        server.join().unwrap();
        assert!(detach.is_err());
        assert_eq!(
            *spoken.lock().unwrap(),
            vec![("sess-1".to_string(), "Recovered.".to_string())]
        );
    }

    #[test]
    fn resume_timeout_retries_on_the_same_connection() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            let list = next_request(&mut ws);
            assert_eq!(list["method"], "thread/loaded/list");
            send(
                &mut ws,
                json!({ "id": list["id"], "result": { "data": ["sess-1"] } }),
            );
            let first_resume = next_request(&mut ws);
            assert_eq!(first_resume["method"], "thread/resume");
            // Drop only this response. The client must clear its in-flight witness,
            // relist, and issue one fresh resume without reconnecting.
            let retry_list = next_request(&mut ws);
            assert_eq!(retry_list["method"], "thread/loaded/list");
            send(
                &mut ws,
                json!({ "id": retry_list["id"], "result": { "data": ["sess-1"] } }),
            );
            let retry_resume = next_request(&mut ws);
            assert_eq!(retry_resume["method"], "thread/resume");
            send(
                &mut ws,
                json!({ "id": retry_resume["id"], "result": { "thread": {
                    "id": "sess-1", "sessionId": "sess-1" } } }),
            );
            send(
                &mut ws,
                completed("sess-1", "item-1", "> Recovered.\n\nBody."),
            );
            std::thread::sleep(Duration::from_millis(250));
        });
        let detach = attach_once_with(addr, &paths, &registry, &spoken, &retry_tunables(), None);
        server.join().unwrap();
        assert!(detach.is_err());
        assert_eq!(
            *spoken.lock().unwrap(),
            vec![("sess-1".to_string(), "Recovered.".to_string())]
        );
    }

    #[test]
    fn foreign_threads_are_never_resumed_or_narrated() {
        // A thread on the daemon whose id matches NO registered session (Codex Desktop /
        // another tool) is never resumed; its notifications are ignored.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let registry = SessionRegistry::new();
        registry.nudge("sess-mine");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            // Only a FOREIGN thread is loaded → no resume must arrive; the next request
            // is another loaded/list retry.
            let req = next_request(&mut ws);
            assert_eq!(req["method"], "thread/loaded/list");
            send(
                &mut ws,
                json!({ "id": req["id"], "result": { "data": ["thr-foreign"] } }),
            );
            // Push a foreign notification anyway — must be ignored (not resumed).
            send(
                &mut ws,
                completed("thr-foreign", "item_9", "> Never spoken."),
            );
            // The client retries the list (unresolved session) rather than resuming.
            let req = next_request(&mut ws);
            assert_eq!(
                req["method"], "thread/loaded/list",
                "no thread/resume for a foreign thread"
            );
            send(
                &mut ws,
                json!({ "id": req["id"], "result": { "data": ["thr-foreign"] } }),
            );
            std::thread::sleep(Duration::from_millis(200));
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        server.join().unwrap();
        assert!(detach.is_err());
        assert!(
            spoken.lock().unwrap().is_empty(),
            "foreign-thread notifications never narrate"
        );
        assert!(
            !ds_narrate::witness_exists(&paths, "thr-foreign"),
            "no witness for a session we never resumed"
        );
    }

    #[test]
    fn thread_unload_evicts_the_session_and_clears_its_state() {
        // Codex has no SessionEnd hook — the supervisor owns cleanup: when a resumed
        // thread disappears from the daemon's loaded list, the session's state/lock/tmp
        // trio is deleted and the session evicted from the registry.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            // Simulate a scheduler delay longer than one socket read tick. Ordinary
            // requests must stay pending instead of enqueueing duplicate protocol work.
            std::thread::sleep(super::super::client::READ_TICK + Duration::from_millis(100));
            serve_list_and_resumes(&mut ws, &["sess-1"], 1);
            send(&mut ws, completed("sess-1", "item_1", "> Hi.\n\nBody."));
            // The periodic relist: NOW the thread is gone (session ended server-side).
            let req = next_request(&mut ws);
            assert_eq!(req["method"], "thread/loaded/list");
            send(
                &mut ws,
                json!({ "id": req["id"], "result": { "data": [] } }),
            );
            std::thread::sleep(Duration::from_millis(300));
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        server.join().unwrap();
        assert!(detach.is_err());
        assert_eq!(
            *spoken.lock().unwrap(),
            vec![("sess-1".to_string(), "Hi.".to_string())],
            "spoken while loaded"
        );
        assert!(
            !ds_narrate::witness_exists(&paths, "sess-1"),
            "eviction cleared the state file trio"
        );
        assert!(
            registry.snapshot().0.is_empty(),
            "evicted from the session registry too"
        );
    }

    #[test]
    fn list_error_response_never_evicts() {
        // A JSON-RPC ERROR reply to thread/loaded/list (`result: None`) must not be
        // conflated with "zero threads loaded": one transient server error mid-turn
        // would otherwise wipe the witness + spoken-offset state of EVERY attached
        // session, and the next batch (or Stop) would re-speak the whole reply.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            serve_list_and_resumes(&mut ws, &["sess-1"], 1);
            send(&mut ws, completed("sess-1", "item_1", "> Hi.\n\nBody."));
            // The periodic relist errors (transient server hiccup) — twice, to show it
            // keeps retrying rather than acting on the failure.
            for _ in 0..2 {
                let req = next_request(&mut ws);
                assert_eq!(req["method"], "thread/loaded/list");
                send(
                    &mut ws,
                    json!({ "id": req["id"], "error": { "code": -32603, "message": "boom" } }),
                );
            }
            std::thread::sleep(Duration::from_millis(300));
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        server.join().unwrap();
        assert!(detach.is_err());
        assert_eq!(
            *spoken.lock().unwrap(),
            vec![("sess-1".to_string(), "Hi.".to_string())],
            "digest spoken exactly once"
        );
        assert!(
            ds_narrate::witness_exists(&paths, "sess-1"),
            "an errored list keeps the witness/state trio"
        );
        assert_eq!(
            registry.snapshot().0,
            vec!["sess-1".to_string()],
            "an errored list keeps the session registered"
        );
    }

    #[test]
    fn run_attached_stands_down_when_gated_off() {
        // codex_dir ABSENT ⇒ the cfg-refresh tick detaches cleanly (Disabled), so a
        // mid-session `~/.codex` removal (or codex_stream=false) parks the supervisor.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path()); // note: no codex_dir created
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();

        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn({
            let listener = listener.try_clone().unwrap();
            move || {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                serve_initialize(&mut ws);
                // Absorb whatever the client sends until it hangs up.
                while ws.read().is_ok() {}
            }
        });
        let detach = attach_once(addr, &paths, &registry, &spoken);
        assert_eq!(detach, Ok(Detach::Disabled));
        drop(listener);
        server.join().unwrap();
    }

    #[test]
    fn endpoint_config_change_detaches_for_immediate_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(&paths.codex_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            &paths.config_toml,
            "codex_app_server_url = \"ws://127.0.0.1:4999\"\n",
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = SessionRegistry::new();
        registry.nudge("sess-1");
        let spoken = std::sync::Arc::new(Mutex::new(Vec::new()));

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            serve_initialize(&mut ws);
            while ws.read().is_ok() {}
        });
        let detach = attach_once_with(
            addr,
            &paths,
            &registry,
            &spoken,
            &short_tunables(),
            Some("tcp:127.0.0.1:4888"),
        );
        assert_eq!(detach, Ok(Detach::Reconfigure));
        server.join().unwrap();
    }
}
