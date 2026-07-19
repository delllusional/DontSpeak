//! Engine (RPC host) spawn/detach. Tools that bridge to the resident engine call
//! [`ensure_engine`] first — an MCP client may invoke us with no engine running yet.

use std::path::Path;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::time::Duration;

use ds_ipc::Request;

use crate::mcp::log;

/// Own process group so host survives this MCP shim / Ctrl-C. Linux process_group(0);
/// Windows DETACHED; macOS `open` detaches.
#[cfg(all(unix, not(target_os = "macos")))]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}
#[cfg(target_os = "windows")]
fn detach(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Launch platform host if needed and wait for the engine socket (in-process only).
pub(crate) fn ensure_engine(sock: &Path) -> bool {
    if ds_ipc::request(sock, &Request::Ping).is_ok() {
        return true;
    }
    if !launch_host() {
        log("no DontSpeak host app installed; tools fail until it runs");
        return false;
    }
    // ~5s: host launch + engine start; engine binds the socket before warming Kokoro.
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if ds_ipc::request(sock, &Request::Ping).is_ok() {
            return true;
        }
    }
    log("engine did not become ready in time");
    false
}

/// Launch the resident host. Returns whether a launch was *issued* (caller polls the socket).
/// `false` ⇒ no host app installed.
#[cfg(target_os = "macos")]
fn launch_host() -> bool {
    // `-g` background, `-b` by bundle id (LaunchServices finds DontSpeak.app / login item).
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

/// Locate the host binary. Windows: `ds-winui.exe` (ds_core.dll beside it). Linux: `ds-gtk`.
/// Order: next to this binary (packaged single-dir), then `~/.local/bin` install layout.
#[cfg(not(target_os = "macos"))]
fn host_app_bin() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|cur| cur.parent().map(Path::to_path_buf));
    let home = ds_config::Paths::resolve().map(|paths| paths.home);
    host_app_candidates(exe_dir.as_deref(), home.as_deref())
        .into_iter()
        .find(|p| p.exists())
}

/// Ordered candidate paths from already-resolved `exe_dir` / `home` — pure composition,
/// no filesystem checks. See [`host_app_bin`] for what each candidate is.
#[cfg(not(target_os = "macos"))]
fn host_app_candidates(exe_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    const APP: &str = "ds-winui.exe";
    #[cfg(not(target_os = "windows"))]
    const APP: &str = "ds-gtk";

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join(APP)); // packaged single-dir (all bins together)
        #[cfg(target_os = "windows")]
        candidates.push(dir.join("winui").join(APP)); // winui\ subdir beside us
    }
    if let Some(home) = home {
        // Linux install-gui.sh: `ds-gtk` in ~/.local/bin. Windows portable zip is single-dir
        // (above); winui/ under home is a legacy dev-deploy fallback.
        #[cfg(target_os = "windows")]
        candidates.push(home.join(".local/bin/winui").join(APP));
        #[cfg(not(target_os = "windows"))]
        candidates.push(home.join(".local/bin").join(APP));
    }
    candidates
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    // Pure `host_app_candidates` only — never call host_app_bin / launch_host / ensure_engine
    // (those hit real Paths::resolve and could spawn the installed host).

    #[cfg(target_os = "windows")]
    const APP: &str = "ds-winui.exe";
    #[cfg(not(target_os = "windows"))]
    const APP: &str = "ds-gtk";

    #[test]
    fn both_present_orders_exe_dir_before_home() {
        let exe_dir = Path::new("/tmp/exe-dir");
        let home = Path::new("/tmp/home");
        let candidates = host_app_candidates(Some(exe_dir), Some(home));

        assert_eq!(candidates[0], exe_dir.join(APP));
        #[cfg(target_os = "windows")]
        {
            assert_eq!(candidates[1], exe_dir.join("winui").join(APP));
            assert_eq!(candidates[2], home.join(".local/bin/winui").join(APP));
            assert_eq!(candidates.len(), 3);
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(candidates[1], home.join(".local/bin").join(APP));
            assert_eq!(candidates.len(), 2);
        }
    }

    #[test]
    fn no_exe_dir_still_yields_home_candidates() {
        let home = Path::new("/tmp/home");
        let candidates = host_app_candidates(None, Some(home));

        #[cfg(target_os = "windows")]
        {
            assert_eq!(candidates, vec![home.join(".local/bin/winui").join(APP)]);
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(candidates, vec![home.join(".local/bin").join(APP)]);
        }
    }

    #[test]
    fn no_home_still_yields_exe_dir_candidates() {
        let exe_dir = Path::new("/tmp/exe-dir");
        let candidates = host_app_candidates(Some(exe_dir), None);

        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                candidates,
                vec![exe_dir.join(APP), exe_dir.join("winui").join(APP)]
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(candidates, vec![exe_dir.join(APP)]);
        }
    }

    #[test]
    fn neither_input_yields_no_candidates() {
        assert!(host_app_candidates(None, None).is_empty());
    }
}
