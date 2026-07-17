//! Unified activity log — one file, leveled format, lean in-process rotation.
//!
//! Shared by engine + hooks + mcp. Each call opens `O_APPEND` and `write_all`s one line;
//! append-at-EOF is atomic on POSIX and on Windows (`FILE_APPEND_DATA`), so concurrent
//! multi-process writers (engine, CLI hooks/MCP, `ds-helper`) never interleave.
//!
//! Rotation is size-based by RENAME (never truncate — truncate-rewrite concatenated
//! timestamps). At `LOG_MAX_BYTES`: `dontspeak.log` → `.1` → `.2` (oldest dropped). No
//! `newsyslog`/sudo. Concurrent rename at threshold is rare and non-fatal.
//!
//! Wire format: `[<epoch_seconds>] <LEVEL> <source> <message>\n`
//!   e.g. `[1781700000] INFO engine started build=ab12cd`
//! `source` is the subsystem token (`log::Record::target()`); UIs filter on it.
//!
//! Client identity is a different axis: trailing `client=<token>` k=v inside the message
//! (existing k=v idiom), never a fourth positional field:
//!
//! ```text
//! [<epoch>] <LEVEL> <source> <message>                 # client == DontSpeak
//! [<epoch>] <LEVEL> <source> <message> client=codex    # any other client, incl. unknown
//! ```
//!
//! So `parse_unified_line` / `combined_log_json` stay shape-stable; `client=` greps as
//! "external client caused this"; engine lines skip a redundant `client=dontspeak`. See [`log_from`].

use ds_client::ClientSource;
use std::path::{Path, PathBuf};

fn open_append_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Rotate threshold (~5 MiB).
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated files kept (`dontspeak.log.1` .. `.LOG_KEEP_OLD`).
const LOG_KEEP_OLD: usize = 2;

/// Severity token on a unified-log line (INFO/WARN/ERROR/DEBUG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Verbose telemetry; engine gates on `DONTSPEAK_DEBUG` so normal logs stay clean.
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rotated_path(path: &std::path::Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Best-effort size rotation by RENAME. Public so non-`log()` writers (child stderr sinks)
/// share the same bound — see drift-guard test.
pub fn rotate_if_large(path: &Path) {
    let too_big = std::fs::metadata(path)
        .map(|m| m.len() >= LOG_MAX_BYTES)
        .unwrap_or(false);
    if !too_big {
        return;
    }
    // Shift older files up (oldest overwritten), then current → `.1`.
    for i in (1..LOG_KEEP_OLD).rev() {
        let _ = std::fs::rename(rotated_path(path, i), rotated_path(path, i + 1));
    }
    let _ = std::fs::rename(path, rotated_path(path, 1));
}

/// Sibling of the engine log with `file_name` — single placement rule so aux logs never drift.
pub fn aux_log_path(engine_log: &Path, file_name: &str) -> PathBuf {
    engine_log.with_file_name(file_name)
}

/// Open an aux append log: ensure dir, rotate first (bounded long-lived handles), then `O_APPEND`.
/// `None` on any IO error. Use for every non-`log()` sink.
pub fn open_aux_log(log_file: &Path, file_name: &str) -> Option<std::fs::File> {
    let path = aux_log_path(log_file, file_name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_large(&path);
    open_append_private(&path).ok()
}

/// Append one unified-log line. `source` = subsystem token. Fail-quiet (never take down hooks/engine).
/// Attributed to DontSpeak (no `client=`). Use [`log_from`] for a client-caused line.
pub fn log(log_file: &Path, level: LogLevel, source: &str, msg: &str) {
    log_from(log_file, level, source, ClientSource::DontSpeak, msg);
}

/// Client-attributed `log`: non-`DontSpeak` appends ` client=<token>` to the message
/// (positional fields untouched — UI shape stays byte-compatible). Takes the log PATH so
/// tests must pass a tempdir; deliberately no cached client-attributed variant (would tempt
/// real-`$HOME` writes).
pub fn log_from(log_file: &Path, level: LogLevel, source: &str, client: ClientSource, msg: &str) {
    use std::io::Write;
    if let Some(dir) = log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_large(log_file);
    let source = source.replace(['\n', '\r'], " ");
    let msg = msg.replace(['\n', '\r'], " ");
    // Client as trailing k=v; DontSpeak ⇒ no suffix.
    let suffix = match client {
        ClientSource::DontSpeak => String::new(),
        c => format!(" client={}", c.as_str()),
    };
    // One line, one write_all → atomic append.
    let line = format!(
        "[{}] {} {source} {msg}{suffix}\n",
        epoch_secs(),
        level.as_str()
    );
    if let Ok(mut f) = open_append_private(log_file) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Per-OS default log path without depending on `ds-config`.
///
/// Cycle: `VoiceConfig::load` calls this crate's `log()`, so `ds-log` cannot take
/// `ds_config::Paths`. Deliberate duplicate of `ds_config::paths::log_path` / `state_root` /
/// `APP_DIR` (same `directories::BaseDirs` convention); kept in sync by convention. Same
/// pattern as `log_watch::is_relevant` vs `dontspeakd::config_watch`.
///
///   macOS:   `~/Library/Logs/DontSpeak/dontspeak.log`
///   Windows: `%LOCALAPPDATA%\DontSpeak\logs\dontspeak.log`
///   Linux:   `$XDG_STATE_HOME`/`~/.local/state/dontspeak/logs/dontspeak.log`
///
/// `DONTSPEAK_LOG_FILE` overrides outright — needed for integration tests that spawn real
/// binaries (`ds_log::init()`) with no in-process path seam. `HOME`/`XDG_*` work on
/// macOS/Linux; Windows known-folder APIs ignore child `LOCALAPPDATA` env overrides.
fn default_log_file() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DONTSPEAK_LOG_FILE") {
        return Some(PathBuf::from(p));
    }
    let base = directories::BaseDirs::new()?;

    #[cfg(not(target_os = "linux"))]
    const APP_DIR: &str = "DontSpeak";
    #[cfg(target_os = "linux")]
    const APP_DIR: &str = "dontspeak";

    #[cfg(target_os = "macos")]
    {
        Some(
            base.home_dir()
                .join("Library/Logs")
                .join(APP_DIR)
                .join("dontspeak.log"),
        )
    }
    #[cfg(target_os = "windows")]
    {
        Some(
            base.data_local_dir()
                .join(APP_DIR)
                .join("logs")
                .join("dontspeak.log"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let state = base
            .state_dir()
            .unwrap_or_else(|| base.data_dir())
            .join(APP_DIR);
        Some(state.join("logs").join("dontspeak.log"))
    }
}

static CACHED_LOG_FILE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// `log()` with process-cached default path (CLI hooks/MCP, helper). Fail-quiet if `$HOME`
/// unresolved. Callers with a tempdir path must use `log()` — this always hits the real
/// per-OS path and would leak test writes.
///
/// Issue #26: unreachable from unit tests (only via facade sink installed by `init()`, which
/// tests never call). CI "verify tests didn't leak into the real log file" enforces it.
/// No `log_cached_from` by design — client attribution goes through [`log_from`] with a path.
pub(crate) fn log_cached(level: LogLevel, source: &str, msg: &str) {
    match CACHED_LOG_FILE.get_or_init(default_log_file) {
        Some(log_file) => log(log_file, level, source, msg),
        None => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "[{source}] {msg}");
        }
    }
}

/// Last `max_bytes` of a log as UTF-8 (lossy), shared-read while the engine appends.
/// Mid-file start drops the partial first line. Empty if absent/unreadable.
pub fn log_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    if is_symlink(path) {
        return String::new();
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if start > 0 && f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    // Mid-file window ⇒ drop partial first line.
    if start > 0
        && let Some(nl) = s.find('\n')
    {
        s.drain(..=nl);
    }
    s
}

/// Parse `[<epoch>] <LEVEL> <source> <message…>` → `(ts, level, source, message)`.
fn parse_unified_line(line: &str) -> Option<(u64, String, String, String)> {
    let rest = line.strip_prefix('[')?;
    let (ts_str, rest) = rest.split_once(']')?;
    let ts: u64 = ts_str.trim().parse().ok()?;
    let mut it = rest.trim_start().splitn(3, ' ');
    let level = it.next()?.to_string();
    let source = it.next()?.to_string();
    let msg = it.next().unwrap_or("").to_string();
    Some((ts, level, source, msg))
}

/// Erase unified log, rotations, and sibling `*.log` aux files (Logs tab Clear).
/// Remove, not truncate, so next write recreates fresh. Fail-quiet per file; UI must confirm.
pub fn clear_logs(log_file: &Path) {
    clear_logs_at(log_file);
}

fn clear_logs_at(unified_log: &Path) {
    let _ = std::fs::remove_file(unified_log);
    for i in 1..=LOG_KEEP_OLD {
        let _ = std::fs::remove_file(rotated_path(unified_log, i));
    }
    let Some(dir) = unified_log.parent() else {
        return;
    };
    let unified_name = unified_log
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for p in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
        .filter(|p| p.file_name().and_then(|s| s.to_str()) != Some(unified_name))
    {
        let _ = std::fs::remove_file(&p);
    }
}

/// Combined tail of every log in the dir as JSON `[{source, level, text}, …]` (Logs tab).
/// Unified lines keep their own source; aux siblings tag by file stem (`ds-helper.log` →
/// `helper`) at file mtime. Excludes `*.log.N`. `max_bytes` is per file.
pub fn combined_log_json(log_file: &Path, max_bytes: u64) -> String {
    combined_log_json_at(log_file, max_bytes)
}

fn combined_log_json_at(unified_log: &Path, max_bytes: u64) -> String {
    // (ts, source, level, text) — ts only for ordering.
    let mut lines: Vec<(u64, String, String, String)> = Vec::new();

    for l in log_tail(unified_log, max_bytes).lines() {
        if l.is_empty() {
            continue;
        }
        match parse_unified_line(l) {
            Some((ts, level, source, msg)) => lines.push((ts, source, level, msg)),
            None => lines.push((0, "log".to_string(), String::new(), l.to_string())),
        }
    }

    // Sibling `*.log` (not unified; `*.log.N` already filtered by extension).
    if let Some(dir) = unified_log.parent() {
        let unified_name = unified_log
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut aux: Vec<std::path::PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
                .filter(|p| !is_symlink(p))
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n != unified_name)
                        == Some(true)
                })
                .collect();
            aux.sort();
            for p in aux {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("aux");
                let source = stem.strip_prefix("ds-").unwrap_or(stem).to_string();
                let mtime = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                for l in log_tail(&p, max_bytes).lines() {
                    if !l.is_empty() {
                        lines.push((mtime, source.clone(), String::new(), l.to_string()));
                    }
                }
            }
        }
    }

    // Stable by ts; aux blocks land near file mtime.
    lines.sort_by_key(|(ts, ..)| *ts);

    let arr: Vec<serde_json::Value> = lines
        .into_iter()
        .map(|(_, source, level, text)| serde_json::json!({ "source": source, "level": level, "text": text }))
        .collect();
    serde_json::Value::Array(arr).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unified_line_splits_the_wire_format() {
        let (ts, level, source, msg) =
            parse_unified_line("[1781700000] INFO engine started build=ab12cd").unwrap();
        assert_eq!(ts, 1781700000);
        assert_eq!(level, "INFO");
        assert_eq!(source, "engine");
        assert_eq!(msg, "started build=ab12cd");
        assert!(parse_unified_line("not a log line").is_none());
    }

    /// Parsed `(level, source, message)` of the sole line — avoid raw-byte compares that flake
    /// across a second boundary on the `[<epoch>]` prefix.
    fn only_line(path: &Path) -> (String, String, String) {
        let raw = std::fs::read_to_string(path).expect("log written");
        let line = raw.lines().next().expect("one line");
        let (_ts, level, source, msg) = parse_unified_line(line).expect("parses as our format");
        (level, source, msg)
    }

    #[test]
    fn log_from_appends_the_client_as_a_trailing_kv() {
        // Non-DontSpeak → ` client=<token>` at end of message; positional fields untouched.
        let dir = tempfile::tempdir().unwrap();
        for (client, want) in [
            (
                ClientSource::ClaudeCode,
                "greet session=s1 client=claude_code",
            ),
            (ClientSource::Codex, "greet session=s1 client=codex"),
            (ClientSource::QwenCode, "greet session=s1 client=qwen_code"),
            (ClientSource::Grok, "greet session=s1 client=grok"),
            (ClientSource::Unknown, "greet session=s1 client=unknown"),
        ] {
            let p = dir.path().join(format!("{}.log", client.as_str()));
            log_from(&p, LogLevel::Info, "engine", client, "greet session=s1");
            let (level, source, msg) = only_line(&p);
            assert_eq!((level.as_str(), source.as_str()), ("INFO", "engine"));
            assert_eq!(msg, want, "{client:?}");
        }
    }

    #[test]
    fn dontspeak_client_renders_no_suffix_and_matches_plain_log() {
        // Own lines stay compatible with plain `log()` — no `client=dontspeak`.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.log");
        let b = dir.path().join("b.log");
        log(&a, LogLevel::Warn, "config", "bad value key=rate");
        log_from(
            &b,
            LogLevel::Warn,
            "config",
            ClientSource::DontSpeak,
            "bad value key=rate",
        );
        assert_eq!(only_line(&a), only_line(&b));
        assert_eq!(only_line(&a).2, "bad value key=rate", "no client= suffix");
    }

    #[test]
    fn combined_log_merges_unified_and_aux_by_source() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("dontspeak.log");
        std::fs::write(
            &unified,
            b"[1000] INFO engine started\n[1002] WARN config bad value\n",
        )
        .unwrap();
        // Aux sibling + rotated file that must be ignored.
        std::fs::write(
            dir.path().join("ds-helper.log"),
            b"listen-debug: rms=0.02\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("dontspeak.log.1"),
            b"[1] INFO engine old rotated\n",
        )
        .unwrap();

        let json = combined_log_json_at(&unified, 64 * 1024);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = v.as_array().unwrap();
        let sources: Vec<&str> = arr.iter().map(|l| l["source"].as_str().unwrap()).collect();
        assert!(sources.contains(&"engine"), "unified engine line present");
        assert!(sources.contains(&"config"), "unified config line present");
        assert!(
            sources.contains(&"helper"),
            "aux helper line tagged by file name"
        );
        assert!(
            !arr.iter().any(|l| l["text"].as_str() == Some("old rotated")
                || l["text"]
                    .as_str()
                    .map(|t| t.contains("old rotated"))
                    .unwrap_or(false)),
            "rotated *.log.1 is excluded"
        );
    }

    #[test]
    fn log_tail_reads_a_clean_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("dontspeak.log");
        assert_eq!(log_tail(&p, 100), "", "absent file → empty");
        std::fs::write(&p, b"line1\nline2\nline3\n").unwrap();
        assert_eq!(log_tail(&p, 1000), "line1\nline2\nline3\n");
        let tail = log_tail(&p, 11);
        assert!(
            tail.ends_with("line3\n"),
            "ends at the newest line: {tail:?}"
        );
        assert!(
            !tail.contains("ine2"),
            "partial first line dropped: {tail:?}"
        );
    }

    #[test]
    fn clear_logs_removes_the_unified_log_rotated_backups_and_aux_logs() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("dontspeak.log");
        std::fs::write(&unified, b"[1000] INFO engine started\n").unwrap();
        std::fs::write(dir.path().join("dontspeak.log.1"), b"old rotated\n").unwrap();
        std::fs::write(dir.path().join("dontspeak.log.2"), b"older rotated\n").unwrap();
        std::fs::write(dir.path().join("ds-helper.log"), b"helper stderr\n").unwrap();
        // Non-.log sibling must survive.
        std::fs::write(dir.path().join("notes.txt"), b"keep me\n").unwrap();

        clear_logs_at(&unified);

        assert!(!unified.exists(), "unified log removed");
        assert!(
            !dir.path().join("dontspeak.log.1").exists(),
            "rotated .1 removed"
        );
        assert!(
            !dir.path().join("dontspeak.log.2").exists(),
            "rotated .2 removed"
        );
        assert!(
            !dir.path().join("ds-helper.log").exists(),
            "aux log removed"
        );
        assert!(
            dir.path().join("notes.txt").exists(),
            "non-.log sibling untouched"
        );
    }

    #[test]
    fn clear_logs_is_a_noop_when_nothing_exists() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("dontspeak.log");
        clear_logs_at(&unified); // must not panic when the dir/files are absent
    }

    #[test]
    fn aux_log_is_a_sibling_of_the_engine_log() {
        // Drift guard: aux log shares engine log's directory.
        let engine = Path::new("/x/state/logs/dontspeak.log");
        let aux = aux_log_path(engine, "ds-helper.log");
        assert_eq!(aux.parent(), engine.parent(), "shares the engine log's dir");
        assert_eq!(aux.file_name().unwrap(), "ds-helper.log");
    }

    #[test]
    fn rotate_if_large_shifts_by_rename_at_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("aux.log");
        std::fs::write(&p, b"small").unwrap();
        rotate_if_large(&p);
        assert!(
            p.is_file() && !dir.path().join("aux.log.1").exists(),
            "small file untouched"
        );
        std::fs::write(&p, vec![0u8; LOG_MAX_BYTES as usize]).unwrap();
        rotate_if_large(&p);
        assert!(dir.path().join("aux.log.1").is_file(), "rotated to .1");
        assert!(!p.exists(), "active file renamed away");
    }
}
