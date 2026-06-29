//! Unified activity log — one readable file with lean in-process rotation.
//!
//! One file (`paths.log_file` = ~/Library/Logs/dontspeak.log), one leveled format,
//! shared by every process (engine + hooks + mcp). Each call opens the file
//! `O_APPEND` and writes the whole line in a SINGLE `write_all`; POSIX guarantees
//! an append write lands atomically at EOF, so concurrent writers never interleave.
//!
//! Rotation is IN-PROCESS and size-based, done by RENAME (never truncate — the old
//! truncate-rewrite was the race that concatenated timestamps). When the file
//! reaches `LOG_MAX_BYTES` the writer shifts `dontspeak.log` → `.1` → `.2` (oldest
//! dropped) and a fresh file is recreated on the next append. No `newsyslog`, no
//! sudo. Concurrent rename at the threshold is rare and non-fatal (atomic rename;
//! at worst a couple of lines land in the rotated file).
//!
//! Wire format: `[<epoch_seconds>] <LEVEL> <source> <message>\n`
//!   e.g. `[1781700000] INFO engine started build=ab12cd`
//! `source` is one token: engine, tts, stt, caps, hook, mcp.

use std::path::{Path, PathBuf};

use crate::Paths;

/// Rotate when the active log file reaches this size (~5 MiB).
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// How many rotated files to keep (`dontspeak.log.1` .. `.LOG_KEEP_OLD`).
const LOG_KEEP_OLD: usize = 2;

/// Severity for a unified-log line, rendered as a fixed token (INFO/WARN/ERROR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Verbose per-event telemetry (per-utterance TTS timing, etc.). Written only when the
    /// caller opts in (the engine gates it on `DONTSPEAK_DEBUG`), so normal logs stay clean.
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

/// `path` with a `.<n>` suffix appended (`dontspeak.log` → `dontspeak.log.1`).
fn rotated_path(path: &std::path::Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Best-effort, race-tolerant size rotation by RENAME (never truncate). No-op until the file
/// reaches `LOG_MAX_BYTES`. PUBLIC so non-`log()` writers (e.g. a spawned child's inherited
/// stderr sink) get the SAME rotation as the engine log instead of growing unbounded.
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

/// An auxiliary log file's path — a sibling of the unified engine log (`paths.log_file`) with
/// the given `file_name`. THE single way to place an extra log, so every log shares one per-OS
/// logs dir and none drift to a second location (`with_file_name` keeps the engine log's dir).
pub fn aux_log_path(engine_log: &Path, file_name: &str) -> PathBuf {
    engine_log.with_file_name(file_name)
}

/// Open an auxiliary append log beside the engine log — the ONE entry point for any non-`log()`
/// log sink (e.g. a spawned child's stderr). Ensures the shared logs dir exists, size-rotates
/// the file FIRST (so a long-lived sink that inherits the handle starts bounded), then opens it
/// `O_APPEND`. Returns the open file, or `None` on any IO error. Use this for EVERY new log so
/// location + rotation stay consistent across platforms — see the drift-guard test.
pub fn open_aux_log(paths: &Paths, file_name: &str) -> Option<std::fs::File> {
    let path = aux_log_path(&paths.log_file, file_name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_large(&path);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

/// Append one line to the unified activity log. `source` is the subsystem token
/// (engine, tts, stt, caps, hook, mcp). Fail-quiet: any IO error is a no-op —
/// logging must never take down a hook or the engine.
pub fn log(paths: &Paths, level: LogLevel, source: &str, msg: &str) {
    use std::io::Write;
    if let Some(dir) = paths.log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_large(&paths.log_file);
    // One formatted line, one write_all → atomic append (no interleave).
    let line = format!("[{}] {} {source} {msg}\n", epoch_secs(), level.as_str());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log_file)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The last `max_bytes` of a log file as UTF-8 (lossy), for a read-only in-app log view. Opens
/// SHARED-read so it works while the engine is appending; if the window starts mid-file the
/// (likely partial) first line is dropped so the view begins on a clean line. Empty string if
/// the file is absent/unreadable. Cross-platform — every UI's Logs tab reads through this.
pub fn log_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
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
    // Started mid-file ⇒ the first line is likely partial — drop through the first newline.
    if start > 0
        && let Some(nl) = s.find('\n')
    {
        s.drain(..=nl);
    }
    s
}

/// Parse one unified-log line `[<epoch>] <LEVEL> <source> <message…>` into
/// `(ts, level, source, message)`. `None` if it doesn't match the wire format.
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

/// The COMBINED tail of every log in the logs dir, as a JSON array of
/// `{source, level, text}` in rough chronological order — for the UI's Logs tab. Combines the
/// unified activity log (each line already tagged with its in-process `source`: engine/tts/stt/
/// caps/hook/mcp/config) with each sibling AUXILIARY log (e.g. `ds-helper.log`, the
/// out-of-process warm-synth helper's stderr), tagging aux lines with the file's short name
/// (`ds-helper.log` → `helper`) at the file's mtime. Rotated files (`*.log.N`) are
/// excluded. So the UI gets ALL distinct log types in one list, and can derive the filter set
/// from the distinct `source` values. `max_bytes` caps the tail read PER file.
pub fn combined_log_json(paths: &Paths, max_bytes: u64) -> String {
    combined_log_json_at(&paths.log_file, max_bytes)
}

fn combined_log_json_at(unified_log: &Path, max_bytes: u64) -> String {
    // (ts, source, level, text) — ts only for ordering.
    let mut lines: Vec<(u64, String, String, String)> = Vec::new();

    // The unified log: parse each line's own source/level; keep unparseable lines verbatim.
    for l in log_tail(unified_log, max_bytes).lines() {
        if l.is_empty() {
            continue;
        }
        match parse_unified_line(l) {
            Some((ts, level, source, msg)) => lines.push((ts, source, level, msg)),
            None => lines.push((0, "log".to_string(), String::new(), l.to_string())),
        }
    }

    // Sibling auxiliary logs: every `*.log` in the dir that isn't the unified log itself and
    // isn't a rotated `*.log.N` (those have extension `N`, not `log`, so they're already out).
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

    // Stable sort by ts so the unified lines stay chronological and aux blocks land near their
    // file mtime (stderr without per-line timestamps keeps file order).
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

    #[test]
    fn combined_log_merges_unified_and_aux_by_source() {
        let dir = tempfile::tempdir().unwrap();
        let unified = dir.path().join("dontspeak.log");
        std::fs::write(
            &unified,
            b"[1000] INFO engine started\n[1002] WARN config bad value\n",
        )
        .unwrap();
        // A sibling aux log (raw stderr, no timestamps) + a rotated file that must be ignored.
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
        // Whole file fits the window → returned verbatim.
        assert_eq!(log_tail(&p, 1000), "line1\nline2\nline3\n");
        // Small window → the partial leading line is dropped; the view starts clean.
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
    fn aux_log_is_a_sibling_of_the_engine_log() {
        // DRIFT GUARD: any auxiliary log shares the engine log's directory — only the file name
        // differs. So a new log added via `open_aux_log`/`aux_log_path` can't land elsewhere.
        let engine = Path::new("/x/state/logs/dontspeak.log");
        let aux = aux_log_path(engine, "ds-helper.log");
        assert_eq!(aux.parent(), engine.parent(), "shares the engine log's dir");
        assert_eq!(aux.file_name().unwrap(), "ds-helper.log");
    }

    #[test]
    fn rotate_if_large_shifts_by_rename_at_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("aux.log");
        // Below threshold → no rotation.
        std::fs::write(&p, b"small").unwrap();
        rotate_if_large(&p);
        assert!(
            p.is_file() && !dir.path().join("aux.log.1").exists(),
            "small file untouched"
        );
        // At/over threshold → current renamed to `.1`, active gone (recreated on next open).
        std::fs::write(&p, vec![0u8; LOG_MAX_BYTES as usize]).unwrap();
        rotate_if_large(&p);
        assert!(dir.path().join("aux.log.1").is_file(), "rotated to .1");
        assert!(!p.exists(), "active file renamed away");
    }
}
