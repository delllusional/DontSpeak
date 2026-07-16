//! Shared wire-writer IO: bin resolve, never-clobber JSON read, backup→atomic-write tail.

use ds_config::Paths;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Deployed `dontspeak` path for hook/MCP registration.
///
/// Unix prefers `~/.local/bin/dontspeak` (wired configs survive rebuilds; dev `target/` wires
/// the deployed bin). Windows never does — portable zip is beside this exe; a stale
/// `~/.local/bin` must not shadow it. Else sibling of this exe. `paths: None` skips the unix
/// stable path. macOS `.app` uses [`bundle_cli_path`] so reconcile doesn't churn every launch.
pub(crate) fn resolve_dontspeak_bin_at(paths: Option<&Paths>) -> Option<String> {
    let file = format!("dontspeak{}", std::env::consts::EXE_SUFFIX);
    #[cfg(unix)]
    if let Some(p) = paths {
        let cand = p.home.join(".local/bin").join(&file);
        if cand.exists() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    #[cfg(not(unix))]
    let _ = paths;
    let exe = std::env::current_exe().ok()?;
    if let Some(cli) = bundle_cli_path(&exe) {
        return Some(cli.to_string_lossy().into_owned());
    }
    Some(exe.parent()?.join(&file).to_string_lossy().into_owned())
}

/// macOS bundle: `Contents/MacOS/<bin>` → `Contents/Helpers/dontspeak`. Pure; `None` elsewhere.
fn bundle_cli_path(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    Some(contents.join("Helpers").join("dontspeak"))
}

/// Missing/empty → `Null` (shapers treat as `{}`). Malformed → report + `Err` (never clobber).
pub(crate) fn read_json_or_bail(tool: &str, cfg: &Path) -> Result<Value, ()> {
    match std::fs::read_to_string(cfg) {
        Err(_) => Ok(Value::Null),
        Ok(s) if s.trim().is_empty() => Ok(Value::Null),
        Ok(s) => serde_json::from_str::<Value>(&s).map_err(|_| {
            eprintln!(
                "{tool}: existing {} is not valid JSON; leaving it unchanged",
                cfg.display()
            );
        }),
    }
}

pub(crate) enum WriteBody<'a> {
    Json(&'a Value),
    Str(&'a str),
}

/// Backup (warn on failure, still write) → atomic write → report. Exit 0/1.
pub(crate) fn backup_then_write(
    tool: &str,
    cfg: &Path,
    ext: &str,
    body: &WriteBody,
    action: &str,
) -> i32 {
    if let Err(e) = ds_config::backup_before_write(cfg, ext) {
        eprintln!(
            "{tool}: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
            cfg.display()
        );
    }
    let written = match body {
        WriteBody::Json(v) => ds_config::atomic_write_json(cfg, v),
        WriteBody::Str(s) => ds_config::atomic_write_str(cfg, s),
    };
    match written {
        Ok(()) => {
            eprintln!("{tool}: {action} {}", cfg.display());
            0
        }
        Err(e) => {
            eprintln!("{tool}: write failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_cli_path_resolves_helpers_dontspeak() {
        let exe = Path::new("/Applications/DontSpeak.app/Contents/MacOS/DontSpeak");
        assert_eq!(
            bundle_cli_path(exe),
            Some(PathBuf::from(
                "/Applications/DontSpeak.app/Contents/Helpers/dontspeak"
            ))
        );
        assert_eq!(
            bundle_cli_path(Path::new("/home/alex/.local/bin/DontSpeak")),
            None
        );
        assert_eq!(bundle_cli_path(Path::new("/tmp/MacOS/DontSpeak")), None);
    }
}
