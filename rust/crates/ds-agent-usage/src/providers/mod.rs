use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod grok;
pub(crate) mod kimi;
pub(crate) mod qwen;
mod rpc;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Wall-clock budget for credential-bearing usage probes (connect + body).
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_CREDENTIAL_BYTES: u64 = 1024 * 1024;

fn request(method: ds_http::Method, url: &str) -> std::io::Result<ds_http::RequestBuilder> {
    if !url
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage provider URL must use HTTPS",
        ));
    }
    Ok(ds_http::request(
        method,
        url,
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        Some(TOTAL_TIMEOUT),
    ))
}

fn send_json<B: ds_http::body::Body>(
    builder: ds_http::RequestBuilder<B>,
) -> std::io::Result<Value> {
    let response = builder
        .send()
        .map_err(|error| std::io::Error::other(format!("provider request failed: {error}")))?;
    let body = ds_http::read_utf8_limited(response, MAX_JSON_BYTES)?;
    serde_json::from_str(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn read_json_file(path: &Path) -> std::io::Result<Value> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential file exceeds size limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential file exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn number_at(value: &Value, key: &str) -> Option<f64> {
    let raw = value.get(key)?;
    raw.as_f64()
        .or_else(|| raw.as_i64().map(|number| number as f64))
        .or_else(|| raw.as_str()?.trim().parse().ok())
}

fn integer_at(value: &Value, key: &str) -> Option<i64> {
    let raw = value.get(key)?;
    raw.as_i64()
        .or_else(|| raw.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| raw.as_str()?.trim().parse().ok())
}

/// Anthropic fractional seconds (plain Rfc3339 rejects).
fn rfc3339_timestamp(raw: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(date) = time::OffsetDateTime::parse(raw, &Rfc3339) {
        return Some(date.unix_timestamp());
    }
    // Drop fractional seconds before zone: ".707736+00:00" / ".707Z".
    let dot = raw.find('.')?;
    let tail = &raw[dot + 1..];
    let zone_at = tail.find(['Z', '+', '-'])?;
    let cleaned = format!("{}{}", &raw[..dot], &tail[zone_at..]);
    time::OffsetDateTime::parse(&cleaned, &Rfc3339)
        .ok()
        .map(|date| date.unix_timestamp())
}

/// PATH + common GUI-missed install roots. Returned paths exist.
fn resolve_binary(name: &str, paths: &ds_config::Paths) -> Option<PathBuf> {
    let path = std::env::var_os("PATH");
    let override_path = match name {
        "codex" => std::env::var_os("CODEX_CLI_PATH"),
        "grok" => std::env::var_os("GROK_CLI_PATH"),
        _ => None,
    }
    .filter(|value| !value.is_empty())
    .map(PathBuf::from);
    #[cfg(not(windows))]
    let login_path = login_shell_path();
    #[cfg(windows)]
    let login_path = None;
    #[cfg(windows)]
    let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let app_data: Option<PathBuf> = None;
    resolve_binary_in(
        name,
        paths,
        override_path.as_deref(),
        login_path,
        path.as_deref(),
        app_data.as_deref(),
    )
}

fn resolve_binary_in(
    name: &str,
    paths: &ds_config::Paths,
    override_path: Option<&Path>,
    login_path: Option<&OsStr>,
    path: Option<&OsStr>,
    app_data: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(binary) = override_path.filter(|candidate| candidate.is_file()) {
        return Some(binary.to_path_buf());
    }
    let mut dirs = Vec::new();
    if let Some(value) = login_path {
        dirs.extend(std::env::split_paths(value));
    }
    if let Some(value) = path {
        dirs.extend(std::env::split_paths(value));
    }
    dirs.extend([
        paths.home.join(".local/bin"),
        paths.grok_dir.join("bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ]);
    if name == "codex" {
        dirs.push(paths.codex_dir.join("packages/standalone/current"));
        #[cfg(target_os = "macos")]
        dirs.extend([
            paths
                .home
                .join("Applications/ChatGPT.app/Contents/Resources"),
            paths.home.join("Applications/Codex.app/Contents/Resources"),
            PathBuf::from("/Applications/ChatGPT.app/Contents/Resources"),
            PathBuf::from("/Applications/Codex.app/Contents/Resources"),
        ]);
    }

    #[cfg(windows)]
    {
        let roaming = app_data
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paths.home.join("AppData/Roaming"));
        dirs.push(roaming.join("npm"));
        if name == "codex" {
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
    let _ = app_data;

    dirs.into_iter().find_map(|dir| {
        // Windows: skip extensionless npm shebangs (CreateProcess); prefer PE then .cmd.
        #[cfg(windows)]
        let candidates = [
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.com"),
            format!("{name}.bat"),
        ];
        #[cfg(not(windows))]
        let candidates = [name.to_string()];
        candidates
            .into_iter()
            .map(|candidate| dir.join(candidate))
            .find(|candidate| candidate.is_file())
    })
}

/// Login-shell PATH once (GUI apps get a minimal PATH); searched before ambient.
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

    #[test]
    fn provider_requests_require_https() {
        let error = request(ds_http::Method::GET, "http://provider.test/usage")
            .err()
            .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(request(ds_http::Method::GET, "HTTPS://provider.test/usage").is_ok());
    }

    #[test]
    fn binary_resolution_checks_gui_install_roots() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let dir = root.path().join(".grok/bin");
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join(if cfg!(windows) { "grok.exe" } else { "grok" });
        std::fs::write(&binary, b"fixture").unwrap();
        let resolved = resolve_binary_in("grok", &paths, None, None, None, None);
        assert_eq!(resolved, Some(binary));
    }

    #[test]
    fn binary_resolution_prefers_override_then_login_path() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let login_dir = root.path().join("login-bin");
        let ambient_dir = root.path().join("ambient-bin");
        std::fs::create_dir_all(&login_dir).unwrap();
        std::fs::create_dir_all(&ambient_dir).unwrap();
        let filename = if cfg!(windows) { "codex.exe" } else { "codex" };
        let login = login_dir.join(filename);
        let ambient = ambient_dir.join(filename);
        let explicit = root.path().join(filename);
        std::fs::write(&login, b"login").unwrap();
        std::fs::write(&ambient, b"ambient").unwrap();
        std::fs::write(&explicit, b"explicit").unwrap();
        let login_path = std::env::join_paths([&login_dir]).unwrap();
        let ambient_path = std::env::join_paths([&ambient_dir]).unwrap();

        assert_eq!(
            resolve_binary_in(
                "codex",
                &paths,
                Some(&explicit),
                Some(&login_path),
                Some(&ambient_path),
                None,
            ),
            Some(explicit)
        );
        assert_eq!(
            resolve_binary_in(
                "codex",
                &paths,
                None,
                Some(&login_path),
                Some(&ambient_path),
                None,
            ),
            Some(login)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_skips_extensionless_shebang_shims() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let npm = root.path().join("npm");
        let vendor = npm
            .join("node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor")
            .join("x86_64-pc-windows-msvc/bin");
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(npm.join("codex"), b"#!/bin/sh\n").unwrap();
        let real = vendor.join("codex.exe");
        std::fs::write(&real, b"MZ").unwrap();

        let resolved = resolve_binary_in(
            "codex",
            &paths,
            None,
            None,
            Some(npm.as_os_str()),
            Some(root.path()),
        );
        assert_eq!(resolved, Some(real));
    }
}
