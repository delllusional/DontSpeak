//! Grok interactive session paths under `~/.grok/sessions/`.
//!
//! Live layout (verified): `sessions/<percent-encoded-cwd>/<sessionId>/` holds
//! `updates.jsonl` (ACP event stream) and `chat_history.jsonl` (role lines for Stop).
//! Shared by the CLI Stop adapter and the engine file-tail mid-turn streamer.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::Paths;

/// Percent-encode a cwd the way Grok names session folders
/// (`C:\Users\usr` → `C%3A%5CUsers%5Cusr`).
pub fn encode_grok_session_cwd(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len() * 3);
    for &b in cwd.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `~/.grok/sessions`.
pub fn grok_sessions_root(paths: &Paths) -> PathBuf {
    paths.grok_dir.join("sessions")
}

/// `sessions/<encoded-cwd>/<sessionId>`.
pub fn grok_session_dir(paths: &Paths, encoded_cwd: &str, session: &str) -> PathBuf {
    grok_sessions_root(paths).join(encoded_cwd).join(session)
}

/// Sibling `chat_history.jsonl` under a session directory.
pub fn grok_chat_history_path(session_dir: &Path) -> PathBuf {
    session_dir.join("chat_history.jsonl")
}

/// Sibling `updates.jsonl` under a session directory.
pub fn grok_updates_jsonl_path(session_dir: &Path) -> PathBuf {
    session_dir.join("updates.jsonl")
}

/// Prefer sibling `chat_history.jsonl` when `path` is live Grok `updates.jsonl`
/// (the ACP stream has no `type:assistant` lines).
pub fn prefer_chat_history_transcript(path: PathBuf) -> PathBuf {
    if is_updates_jsonl(&path) {
        let chat = path.with_file_name("chat_history.jsonl");
        if chat.is_file() {
            return chat;
        }
    }
    path
}

/// True when the path's file name is `updates.jsonl` (case-insensitive).
pub fn is_updates_jsonl(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("updates.jsonl"))
}

/// Resolve the session directory for `session`:
/// 1. If `cwd` is non-empty: `sessions/<encode(cwd)>/<session>` when it exists
/// 2. Else (or on miss) scan `sessions/*/<session>/` and pick the newest mtime
pub fn resolve_grok_session_dir(
    paths: &Paths,
    session: &str,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    let root = grok_sessions_root(paths);
    if let Some(cwd) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let dir = grok_session_dir(paths, &encode_grok_session_cwd(cwd), session);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    scan_session_dir_by_mtime(&root, session)
}

/// Resolve `chat_history.jsonl` for a session (cwd hint optional; scan on skew).
pub fn resolve_grok_chat_history(
    paths: &Paths,
    session: &str,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    if let Some(cwd) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let candidate = grok_chat_history_path(&grok_session_dir(
            paths,
            &encode_grok_session_cwd(cwd),
            session,
        ));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    scan_grok_chat_history_by_mtime(paths, session)
}

/// Resolve `updates.jsonl` for a session (engine mid-turn tail).
pub fn resolve_grok_updates_jsonl(
    paths: &Paths,
    session: &str,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    if let Some(cwd) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let candidate = grok_updates_jsonl_path(&grok_session_dir(
            paths,
            &encode_grok_session_cwd(cwd),
            session,
        ));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Scan sessions/*/<session>/updates.jsonl by newest mtime.
    let root = grok_sessions_root(paths);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return None;
    };
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(session).join("updates.jsonl");
        if !candidate.is_file() {
            continue;
        }
        let modified = candidate
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let take = best.as_ref().map(|(_, t)| modified > *t).unwrap_or(true);
        if take {
            best = Some((candidate, modified));
        }
    }
    best.map(|(p, _)| p)
}

fn scan_session_dir_by_mtime(sessions_root: &Path, session: &str) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return None;
    };
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(session);
        if !candidate.is_dir() {
            continue;
        }
        let modified = candidate
            .metadata()
            .and_then(|m| m.modified())
            .or_else(|_| {
                // Fall back to any known sibling file mtime when the dir stamp is missing.
                let chat = grok_chat_history_path(&candidate);
                let updates = grok_updates_jsonl_path(&candidate);
                chat.metadata()
                    .and_then(|m| m.modified())
                    .or_else(|_| updates.metadata().and_then(|m| m.modified()))
            })
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let take = best.as_ref().map(|(_, t)| modified > *t).unwrap_or(true);
        if take {
            best = Some((candidate, modified));
        }
    }
    best.map(|(p, _)| p)
}

/// Scan `sessions/*/<session>/chat_history.jsonl` by newest file mtime
/// (used when the session dir itself is not yet a directory but the file exists).
pub fn scan_grok_chat_history_by_mtime(paths: &Paths, session: &str) -> Option<PathBuf> {
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    let root = grok_sessions_root(paths);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return None;
    };
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(session).join("chat_history.jsonl");
        if !candidate.is_file() {
            continue;
        }
        let modified = candidate
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let take = best.as_ref().map(|(_, t)| modified > *t).unwrap_or(true);
        if take {
            best = Some((candidate, modified));
        }
    }
    best.map(|(p, _)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_live_folder_names() {
        assert_eq!(
            encode_grok_session_cwd(r"C:\Users\usr"),
            "C%3A%5CUsers%5Cusr"
        );
        assert_eq!(encode_grok_session_cwd("/home/u"), "%2Fhome%2Fu");
    }

    #[test]
    fn resolve_prefers_cwd_then_newest_mtime_scan() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let session = "sess-1";
        let cwd = r"C:\Users\usr";

        let older = grok_session_dir(&paths, "other-cwd", session);
        let newer = grok_session_dir(&paths, &encode_grok_session_cwd(cwd), session);
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(grok_chat_history_path(&older), r#"{"type":"user"}"#).unwrap();
        // Ensure newer is strictly later for mtime-based scan.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            grok_chat_history_path(&newer),
            r#"{"type":"assistant","content":"hi"}"#,
        )
        .unwrap();
        std::fs::write(
            grok_updates_jsonl_path(&newer),
            r#"{"method":"session/update"}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_grok_session_dir(&paths, session, Some(cwd)).as_ref(),
            Some(&newer)
        );
        assert_eq!(
            resolve_grok_chat_history(&paths, session, Some(cwd)).unwrap(),
            grok_chat_history_path(&newer)
        );
        assert_eq!(
            resolve_grok_updates_jsonl(&paths, session, None).unwrap(),
            grok_updates_jsonl_path(&newer)
        );
    }

    #[test]
    fn prefer_chat_history_only_when_sibling_exists() {
        let dir = tempfile::tempdir().unwrap();
        let updates = dir.path().join("updates.jsonl");
        let chat = dir.path().join("chat_history.jsonl");
        std::fs::write(&updates, "{}").unwrap();
        assert_eq!(prefer_chat_history_transcript(updates.clone()), updates);
        std::fs::write(&chat, "{}").unwrap();
        assert_eq!(prefer_chat_history_transcript(updates), chat);
    }

    #[test]
    fn empty_session_resolves_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        assert!(resolve_grok_session_dir(&paths, "  ", None).is_none());
        assert!(resolve_grok_updates_jsonl(&paths, "", None).is_none());
    }
}
