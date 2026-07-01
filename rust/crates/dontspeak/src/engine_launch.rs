//! Engine (RPC host) spawn/detach lifecycle. The tools that bridge to the resident
//! engine call [`ensure_engine`] first, since an MCP client may invoke us with no
//! engine running yet.

use std::path::Path;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::time::Duration;

use ds_ipc::Request;

use crate::mcp::log;

/// Detach a spawned host into its own process group so it survives this short-lived
/// MCP shim exiting (and isn't killed by a Ctrl-C to our pgroup). Linux uses
/// `process_group(0)`; the Windows equivalent (CREATE_NEW_PROCESS_GROUP/
/// DETACHED_PROCESS) is still TODO — a plain spawn links and is correct enough for now.
/// macOS launches via `open`, which detaches for us, so it needs no `detach`.
#[cfg(all(unix, not(target_os = "macos")))]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}
#[cfg(not(unix))]
fn detach(_cmd: &mut std::process::Command) {}

/// Ensure the engine (RPC host) is up. The engine has NO headless mode — it only ever
/// runs IN-PROCESS inside the platform's resident host app, so `ensure_engine` launches
/// that app (bringing up the engine + its socket under one process, never a second
/// conflicting one) and waits for the socket. There is exactly one host per platform:
/// macOS DontSpeak.app, Windows the WinUI app (`ds-winui.exe`, P/Invokes ds_core.dll),
/// Linux the GTK app (`ds-gtk`, links ds-core). With no host installed, tools stay
/// unavailable until the user launches it.
pub(crate) fn ensure_engine(sock: &Path) {
    if ds_ipc::request(sock, &Request::Ping).is_ok() {
        return;
    }
    if !launch_host() {
        log("no DontSpeak host app installed; tools fail until it runs");
        return;
    }
    // Wait up to ~5s for the socket (host launch + engine start takes a moment; the
    // engine binds the socket before warming Kokoro, so it answers early).
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if ds_ipc::request(sock, &Request::Ping).is_ok() {
            return;
        }
    }
    log("engine did not become ready in time");
}

/// Launch the resident host app that owns the in-process engine + its socket. Returns
/// whether a launch was ISSUED (not that the engine is ready — the caller then polls the
/// socket). `false` ⇒ no host app installed.
#[cfg(target_os = "macos")]
fn launch_host() -> bool {
    // `-g` background, `-b` by bundle id (LaunchServices finds the installed
    // DontSpeak.app, which is also the login item).
    std::process::Command::new("/usr/bin/open")
        .args(["-g", "-b", "app.dontspeak.org"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn launch_host() -> bool {
    let Some(app) = host_app_bin() else {
        return false;
    };
    let mut cmd = std::process::Command::new(&app);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn().is_ok()
}

/// Locate the resident host-app binary. Windows: `ds-winui.exe` (P/Invokes ds_core.dll,
/// which must sit beside it). Linux: `ds-gtk` (links the ds-core staticlib and hosts the
/// engine in-process — the analogue of DontSpeak.app). Checks, in order: next to this
/// binary (the packaged single-dir install, where every bin lands together) and the
/// `~/.local/bin` layout the installers publish to. `None` ⇒ not installed.
#[cfg(not(target_os = "macos"))]
fn host_app_bin() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const APP: &str = "ds-winui.exe";
    #[cfg(not(target_os = "windows"))]
    const APP: &str = "ds-gtk";

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cur) = std::env::current_exe()
        && let Some(dir) = cur.parent()
    {
        candidates.push(dir.join(APP)); // packaged single-dir install (all bins together)
        #[cfg(target_os = "windows")]
        candidates.push(dir.join("winui").join(APP)); // a winui\ subdir beside us
    }
    if let Some(paths) = ds_config::Paths::resolve() {
        // The `~/.local/bin` install layout: on Linux install-gui.sh installs `ds-gtk`
        // directly here. (Windows: the `winui/` subdir is a legacy dev-deploy fallback —
        // the portable zip now lays every bin together beside this exe, caught above.)
        #[cfg(target_os = "windows")]
        candidates.push(paths.home.join(".local/bin/winui").join(APP));
        #[cfg(not(target_os = "windows"))]
        candidates.push(paths.home.join(".local/bin").join(APP));
    }
    candidates.into_iter().find(|p| p.exists())
}
