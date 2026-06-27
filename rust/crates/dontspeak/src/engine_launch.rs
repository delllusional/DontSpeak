//! Engine (RPC host) spawn/detach lifecycle. The tools that bridge to the resident
//! engine call [`ensure_engine`] first, since an MCP client may invoke us with no
//! engine running yet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ds_ipc::Request;

use crate::mcp::log;

/// Detach a spawned engine into its own process group so it survives this
/// short-lived MCP shim exiting (and isn't killed by a Ctrl-C to our pgroup).
/// Unix uses `process_group(0)`; the Windows equivalent
/// (CREATE_NEW_PROCESS_GROUP/DETACHED_PROCESS) is still TODO for the Windows
/// port — a plain spawn links and is correct enough for now.
#[cfg(unix)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}
#[cfg(not(unix))]
fn detach(_cmd: &mut std::process::Command) {}

/// Ensure the engine (RPC host) is up. The resident HOST APP hosts the engine
/// IN-PROCESS, so prefer launching it — that brings up the engine + its socket
/// under one process, never a second conflicting one. macOS: DontSpeak.app (one
/// TCC-granted bundle). Windows: the WinUI app (`ds-winui.exe`, which
/// P/Invokes ds_core.dll). Headless (Linux / no app installed): fall back
/// to the standalone `dontspeakd` binary.
pub(crate) fn ensure_engine(sock: &Path) {
    if ds_ipc::request(sock, &Request::Ping).is_ok() {
        return;
    }

    #[cfg(target_os = "macos")]
    // `-g` background, `-b` by bundle id (LaunchServices finds the installed
    // DontSpeak.app, which is also the login item).
    let launched = std::process::Command::new("/usr/bin/open")
        .args(["-g", "-b", "app.dontspeak.org"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    // Windows: launch the WinUI resident host (the macOS-app analogue) when we can
    // find it; it hosts the engine in-process and binds the socket. Falls through to
    // `engine_bin()` (dontspeakd.exe) when no host app is installed (e.g. a dev build
    // with no .NET SDK), so headless still works.
    #[cfg(target_os = "windows")]
    let launched = match host_app_bin() {
        Some(app) => {
            let mut cmd = std::process::Command::new(&app);
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            detach(&mut cmd);
            cmd.spawn().is_ok()
        }
        None => false,
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let launched = false;
    if !launched {
        match engine_bin() {
            Some(bin) => {
                let mut cmd = std::process::Command::new(&bin);
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                detach(&mut cmd);
                let _ = cmd.spawn();
            }
            None => {
                log("no DontSpeak app or engine binary found; tools fail until it runs");
                return;
            }
        }
    }
    // Wait up to ~5s for the socket (app launch + engine start takes a moment; the
    // engine hosts the socket before warming Kokoro, so it answers early).
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if ds_ipc::request(sock, &Request::Ping).is_ok() {
            return;
        }
    }
    log("engine did not become ready in time");
}

/// `~/.local/bin/dontspeakd` (install layout) else a sibling of this binary —
/// the headless engine host (Linux / no GUI). The `EXE_SUFFIX` (`.exe` on
/// Windows, empty elsewhere) is REQUIRED: without it the `exists()` probes look
/// for an extensionless `dontspeakd` and never match `dontspeakd.exe`, so the
/// Windows fallback could never find the binary.
fn engine_bin() -> Option<PathBuf> {
    let exe = format!("dontspeakd{}", std::env::consts::EXE_SUFFIX);
    if let Some(paths) = ds_config::Paths::resolve() {
        let p = paths.home.join(".local/bin").join(&exe);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(cur) = std::env::current_exe()
        && let Some(dir) = cur.parent()
    {
        let p = dir.join(&exe);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Locate the Windows resident host app (`ds-winui.exe`) — the analogue of
/// the macOS DontSpeak.app: it hosts the engine in-process (P/Invokes
/// ds_core.dll, which must sit beside it) and binds the engine socket.
/// Checks, in order: next to this binary (the single-dir `C:\Program Files\Speak
/// MCP` install), a `winui\` subdir beside it, and the `~/.local/bin/winui`
/// layout `install.ps1` publishes to. `None` when no host app is installed.
#[cfg(windows)]
fn host_app_bin() -> Option<PathBuf> {
    const APP: &str = "ds-winui.exe";
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cur) = std::env::current_exe()
        && let Some(dir) = cur.parent()
    {
        candidates.push(dir.join(APP)); // Program Files single-dir install
        candidates.push(dir.join("winui").join(APP)); // a winui\ subdir beside us
    }
    if let Some(paths) = ds_config::Paths::resolve() {
        // install.ps1 publishes the WinUI app to ~/.local/bin/winui.
        candidates.push(paths.home.join(".local/bin/winui").join(APP));
    }
    candidates.into_iter().find(|p| p.exists())
}
