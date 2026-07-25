//! Shared wire-writer IO: bin resolve, fail-closed JSON read, backup→atomic-write.

use ds_config::Paths;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Deployed `dontspeak` for hook/MCP registration.
/// Unix: prefer `~/.local/bin`. Windows: sibling of this exe. macOS `.app` → [`bundle_cli_path`].
/// `paths: None` skips the unix stable path.
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

/// macOS: `Contents/MacOS/<bin>` → `Contents/Helpers/dontspeak`. Pure; `None` elsewhere.
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

/// Missing/empty → `Null`. Malformed → report + `Err` (leave file).
pub(crate) fn read_json_or_bail(tool: &str, cfg: &Path) -> Result<Value, ()> {
    match read_text_or_empty(tool, cfg)? {
        s if s.trim().is_empty() => Ok(Value::Null),
        s => serde_json::from_str::<Value>(&s).map_err(|_| {
            eprintln!(
                "{tool}: existing {} is not valid JSON; leaving it unchanged",
                cfg.display()
            );
        }),
    }
}

/// Missing file → empty. Other read failures leave the file untouched.
pub(crate) fn read_text_or_empty(tool: &str, cfg: &Path) -> Result<String, ()> {
    match std::fs::read_to_string(cfg) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => {
            eprintln!("{tool}: could not read {} ({error})", cfg.display());
            Err(())
        }
    }
}

pub(crate) enum WriteBody<'a> {
    Json(&'a Value),
    Str(&'a str),
}

/// Backup (warn, still write) → atomic write → report. Exit 0/1.
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

    #[test]
    fn text_and_json_reads_fail_closed_except_for_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert_eq!(read_text_or_empty("wire-test", &missing), Ok(String::new()));
        assert_eq!(read_json_or_bail("wire-test", &missing), Ok(Value::Null));

        let unreadable = dir.path().join("config.json");
        std::fs::create_dir(&unreadable).unwrap();
        assert!(read_text_or_empty("wire-test", &unreadable).is_err());
        assert!(read_json_or_bail("wire-test", &unreadable).is_err());
    }
}
