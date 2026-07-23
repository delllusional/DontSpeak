//! Shared client-executable resolution.
//!
//! Presence checks, launch wrappers, usage providers, and the Codex stream supervisor must
//! agree on whether a client is installed and which executable to run. Keep the directory
//! order and client-specific install roots here so those callers cannot drift independently.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::OnceLock;
#[cfg(not(windows))]
use std::time::Duration;

use ds_client::ClientSource;

use crate::paths::Paths;
use crate::voice::VoiceConfig;

/// Explicit inputs for deterministic resolver tests.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientBinarySearch<'a> {
    /// Configured executable name or path. Empty means the client's canonical command.
    configured: &'a str,
    /// Client-specific environment override such as `CODEX_CLI_PATH`.
    override_path: Option<&'a Path>,
    /// Login-shell path captured for GUI-launched processes.
    login_path: Option<&'a OsStr>,
    /// Ordinary process path.
    path: Option<&'a OsStr>,
    /// Windows roaming application-data root.
    app_data: Option<&'a Path>,
    /// Whether machine-global fallback directories may be inspected.
    include_system_dirs: bool,
}

/// Resolve a registry client using its normal configuration and the live environment captured
/// by [`Paths::resolve`]. [`Paths::rooted_at`] remains isolated from the host environment.
pub fn resolve_client_binary(client: ClientSource, paths: &Paths) -> Option<PathBuf> {
    if !client.is_client() {
        return None;
    }
    let configured = if client == ClientSource::Codex {
        VoiceConfig::load(paths).codex_bin
    } else {
        client.as_str().to_string()
    };
    resolve_configured_client_binary(client, paths, &configured)
}

/// Resolve a registry client with a caller-provided executable name or path.
pub fn resolve_configured_client_binary(
    client: ClientSource,
    paths: &Paths,
    configured: &str,
) -> Option<PathBuf> {
    let use_live_environment = paths.path_env.is_some();
    let override_path = use_live_environment
        .then(|| client_binary_override(client))
        .flatten();
    #[cfg(not(windows))]
    let login_path = use_live_environment.then(login_shell_path).flatten();
    #[cfg(windows)]
    let login_path = None;
    #[cfg(windows)]
    let app_data = use_live_environment
        .then(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .flatten();
    #[cfg(not(windows))]
    let app_data: Option<PathBuf> = None;

    resolve_client_binary_in(
        client,
        paths,
        ClientBinarySearch {
            configured,
            override_path: override_path.as_deref(),
            login_path,
            path: paths.path_env.as_deref(),
            app_data: app_data.as_deref(),
            include_system_dirs: use_live_environment,
        },
    )
}

fn client_binary_override(client: ClientSource) -> Option<PathBuf> {
    let variable = match client {
        ClientSource::Codex => "CODEX_CLI_PATH",
        ClientSource::Grok => "GROK_CLI_PATH",
        _ => return None,
    };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Pure resolver core shared by production entry points and table-driven tests.
pub(crate) fn resolve_client_binary_in(
    client: ClientSource,
    paths: &Paths,
    search: ClientBinarySearch<'_>,
) -> Option<PathBuf> {
    if !client.is_client() {
        return None;
    }
    let canonical = client.as_str();
    let configured = search.configured.trim();
    let name = if configured.is_empty() {
        canonical
    } else {
        configured
    };
    let configured_path = Path::new(name);
    if configured_path.is_absolute() || configured_path.components().count() > 1 {
        return configured_path
            .is_file()
            .then(|| configured_path.to_path_buf());
    }
    if let Some(binary) = search.override_path.filter(|candidate| candidate.is_file()) {
        return Some(binary.to_path_buf());
    }

    let mut dirs = Vec::new();
    if let Some(value) = search.login_path {
        dirs.extend(std::env::split_paths(value));
    }
    if let Some(value) = search.path {
        dirs.extend(std::env::split_paths(value));
    }
    dirs.push(paths.home.join(".local/bin"));
    match client {
        ClientSource::Grok => dirs.push(paths.grok_dir.join("bin")),
        ClientSource::Codex => {
            dirs.push(paths.codex_dir.join("packages/standalone/current"));
            #[cfg(target_os = "macos")]
            dirs.extend([
                paths
                    .home
                    .join("Applications/ChatGPT.app/Contents/Resources"),
                paths.home.join("Applications/Codex.app/Contents/Resources"),
            ]);
        }
        _ => {}
    }
    if search.include_system_dirs {
        #[cfg(not(windows))]
        dirs.extend([
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
        ]);
        #[cfg(target_os = "macos")]
        if client == ClientSource::Codex {
            dirs.extend([
                PathBuf::from("/Applications/ChatGPT.app/Contents/Resources"),
                PathBuf::from("/Applications/Codex.app/Contents/Resources"),
            ]);
        }
    }

    #[cfg(windows)]
    {
        let roaming = search
            .app_data
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paths.home.join("AppData/Roaming"));
        dirs.push(roaming.join("npm"));
        if client == ClientSource::Codex {
            let (package, target) = if cfg!(target_arch = "aarch64") {
                ("@openai/codex-win32-arm64", "aarch64-pc-windows-msvc")
            } else {
                ("@openai/codex-win32-x64", "x86_64-pc-windows-msvc")
            };
            dirs.push(
                roaming
                    .join("npm/node_modules/@openai/codex/node_modules")
                    .join(package)
                    .join("vendor")
                    .join(target)
                    .join("bin"),
            );
        }
    }
    #[cfg(not(windows))]
    let _ = search.app_data;

    dirs.into_iter()
        .find_map(|dir| executable_in_dir(&dir, name))
}

#[cfg(windows)]
fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    if Path::new(name).extension().is_some() {
        return dir.join(name).is_file().then(|| dir.join(name));
    }
    ["exe", "cmd", "com", "bat"]
        .into_iter()
        .map(|extension| dir.join(format!("{name}.{extension}")))
        .find(|candidate| candidate.is_file())
}

#[cfg(not(windows))]
fn executable_in_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// Login-shell PATH once (GUI apps); searched before the process PATH.
#[cfg(not(windows))]
fn login_shell_path() -> Option<&'static OsStr> {
    static LOGIN_PATH: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    LOGIN_PATH.get_or_init(capture_login_shell_path).as_deref()
}

#[cfg(not(windows))]
fn capture_login_shell_path() -> Option<std::ffi::OsString> {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const MARKER: &str = "__DONTSPEAK_PATH__";
    let shell = std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/sh".into()
            }
        });
    let is_fish = Path::new(&shell)
        .file_name()
        .is_some_and(|name| name == "fish");
    let print_path = if is_fish {
        "printf '\\n__DONTSPEAK_PATH__%s\\n' (string join : $PATH)"
    } else {
        "printf '\\n__DONTSPEAK_PATH__%s\\n' \"$PATH\""
    };
    let mut child = Command::new(shell)
        .args(["-l", "-c", print_path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MARKER))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::ffi::OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary_name(client: ClientSource) -> String {
        let command = client.as_str();
        if cfg!(windows) {
            format!("{command}.exe")
        } else {
            command.to_string()
        }
    }

    fn isolated_search<'a>(client: ClientSource) -> ClientBinarySearch<'a> {
        ClientBinarySearch {
            configured: client.as_str(),
            override_path: None,
            login_path: None,
            path: None,
            app_data: None,
            include_system_dirs: false,
        }
    }

    #[test]
    fn every_client_uses_the_same_common_search_path() {
        for &client in ClientSource::CLIENTS {
            let root = tempfile::tempdir().unwrap();
            let paths = Paths::rooted_at(root.path());
            let local_bin = root.path().join(".local/bin");
            std::fs::create_dir_all(&local_bin).unwrap();
            let binary = local_bin.join(binary_name(client));
            std::fs::write(&binary, b"fixture").unwrap();

            assert_eq!(
                resolve_client_binary_in(client, &paths, isolated_search(client)),
                Some(binary),
                "{client:?}"
            );
        }
    }

    #[test]
    fn explicit_override_precedes_login_and_process_paths() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(root.path());
        let login_dir = root.path().join("login-bin");
        let process_dir = root.path().join("process-bin");
        std::fs::create_dir_all(&login_dir).unwrap();
        std::fs::create_dir_all(&process_dir).unwrap();
        let filename = binary_name(ClientSource::Codex);
        let explicit = root.path().join(&filename);
        let login = login_dir.join(&filename);
        std::fs::write(&explicit, b"explicit").unwrap();
        std::fs::write(&login, b"login").unwrap();
        std::fs::write(process_dir.join(&filename), b"process").unwrap();
        let login_path = std::env::join_paths([&login_dir]).unwrap();
        let process_path = std::env::join_paths([&process_dir]).unwrap();

        let mut search = isolated_search(ClientSource::Codex);
        search.override_path = Some(&explicit);
        search.login_path = Some(&login_path);
        search.path = Some(&process_path);
        assert_eq!(
            resolve_client_binary_in(ClientSource::Codex, &paths, search),
            Some(explicit.clone())
        );

        search.override_path = None;
        assert_eq!(
            resolve_client_binary_in(ClientSource::Codex, &paths, search),
            Some(login)
        );
    }

    #[test]
    fn client_specific_roots_are_part_of_the_shared_resolver() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(root.path());
        for (client, dir) in [
            (ClientSource::Grok, paths.grok_dir.join("bin")),
            (
                ClientSource::Codex,
                paths.codex_dir.join("packages/standalone/current"),
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            let binary = dir.join(binary_name(client));
            std::fs::write(&binary, b"fixture").unwrap();
            assert_eq!(
                resolve_client_binary_in(client, &paths, isolated_search(client)),
                Some(binary),
                "{client:?}"
            );
        }
    }
}
