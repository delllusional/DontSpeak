//! Audible earcons — reply-done ding and needs-input cue. The engine resolves an event
//! to a sound FILE and hands the path to the warm helper; nothing here opens audio.
//!
//! Configured sound IS each cue's on/off (no separate enable flag): bundled system-sound
//! NAME or a path inside a platform sound directory; empty = off. Reply ding defaults to
//! the OS chime by name (`"ding"` / `"Tink"` / `"message"`). Bare names resolve through
//! [`system_sounds`] (introspection of the OS sounds folder — never a hardcoded path);
//! unresolved ⇒ fail-quiet off. [`system_sounds`] also feeds the UI picker.

use std::path::{Path, PathBuf};

/// Eyes-free cues. Wire: `reply_done` (Stop hook) / `needs_input` (Notification hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarconEvent {
    ReplyDone,
    NeedsInput,
}

impl EarconEvent {
    /// Wire token over IPC (`"reply_done"` / `"needs_input"`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "reply_done" => Some(Self::ReplyDone),
            "needs_input" => Some(Self::NeedsInput),
            _ => None,
        }
    }

    /// Canonical wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplyDone => "reply_done",
            Self::NeedsInput => "needs_input",
        }
    }

    /// Configured sound (trimmed). Empty = this cue is OFF.
    fn sound_in<'a>(self, reply_sound: &'a str, needs_input_sound: &'a str) -> &'a str {
        match self {
            Self::ReplyDone => reply_sound.trim(),
            Self::NeedsInput => needs_input_sound.trim(),
        }
    }
}

/// Bundled system sound from [`system_sounds`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSound {
    /// File stem (no extension) — what a config sound matches against.
    pub name: String,
    pub path: PathBuf,
    /// Size in bytes (smaller ≈ shorter) — UI-picker sort key.
    pub bytes: u64,
}

/// OS directories for bundled system sounds (convention, not a hardcoded list).
fn sound_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use directories::BaseDirs;
        let mut v = vec![PathBuf::from("/System/Library/Sounds")];
        if let Some(b) = BaseDirs::new() {
            v.push(b.home_dir().join("Library/Sounds"));
        }
        v
    }
    #[cfg(target_os = "windows")]
    {
        let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        vec![PathBuf::from(win).join("Media")]
    }
    #[cfg(target_os = "linux")]
    {
        vec![PathBuf::from("/usr/share/sounds/freedesktop/stereo")]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// Trust boundary: canonicalize and keep the path inside a platform sound directory.
pub fn canonical_sound_path(path: &Path) -> Option<PathBuf> {
    canonical_sound_path_in(path, &sound_dirs())
}

fn canonical_sound_path_in(path: &Path, sound_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    if !path.is_file() {
        return None;
    }
    sound_dirs
        .iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .any(|dir| path.starts_with(dir))
        .then_some(path)
}

/// Platform extension for the dir scan: aiff / wav / oga (rodio/symphonia decodes all).
fn sound_ext() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "aiff"
    }
    #[cfg(target_os = "windows")]
    {
        "wav"
    }
    #[cfg(target_os = "linux")]
    {
        "oga"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        ""
    }
}

/// Enumerate bundled sounds by introspection. Size-then-name sort; de-dup by name
/// (earlier dirs win) — no hardcoded names.
pub fn system_sounds() -> Vec<SystemSound> {
    system_sounds_in(&sound_dirs(), sound_ext())
}

/// Resource-explicit core; tests pass tempdir fixtures so the harness never scans OS dirs.
fn system_sounds_in(sound_dirs: &[PathBuf], ext: &str) -> Vec<SystemSound> {
    let mut out: Vec<SystemSound> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dir in sound_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            if !ext.is_empty() {
                let matches_ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case(ext))
                    .unwrap_or(false);
                if !matches_ext {
                    continue;
                }
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = name.to_string();
            if seen.insert(name.clone()) {
                out.push(SystemSound {
                    name,
                    path,
                    bytes: meta.len(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.bytes.cmp(&b.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Resolve event → sound file, or `None` (fail-quiet). Empty config ⇒ off; absolute path
/// only if inside a platform sound dir; bare name matches [`system_sounds`] case-insensitively.
pub fn resolve_cue(
    reply_sound: &str,
    needs_input_sound: &str,
    event: EarconEvent,
) -> Option<PathBuf> {
    resolve_cue_in(
        reply_sound,
        needs_input_sound,
        event,
        &sound_dirs(),
        sound_ext(),
    )
}

fn resolve_cue_in(
    reply_sound: &str,
    needs_input_sound: &str,
    event: EarconEvent,
    sound_dirs: &[PathBuf],
    ext: &str,
) -> Option<PathBuf> {
    let sound = event.sound_in(reply_sound, needs_input_sound);
    if sound.is_empty() {
        return None;
    }
    let p = PathBuf::from(sound);
    if p.is_absolute() {
        return canonical_sound_path_in(&p, sound_dirs);
    }
    system_sounds_in(sound_dirs, ext)
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(sound))
        .and_then(|s| canonical_sound_path_in(&s.path, sound_dirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_token_roundtrips() {
        for ev in [EarconEvent::ReplyDone, EarconEvent::NeedsInput] {
            assert_eq!(EarconEvent::parse(ev.as_str()), Some(ev));
        }
        assert_eq!(
            EarconEvent::parse("  reply_done "),
            Some(EarconEvent::ReplyDone)
        );
        assert_eq!(EarconEvent::parse("bogus"), None);
    }

    #[test]
    fn empty_sound_is_off() {
        assert_eq!(
            resolve_cue_in("", "   ", EarconEvent::ReplyDone, &[], sound_ext()),
            None
        );
        assert_eq!(
            resolve_cue_in("", "   ", EarconEvent::NeedsInput, &[], sound_ext()),
            None
        );
    }

    #[test]
    fn default_reply_sound_is_the_os_chime() {
        // Pin against VoiceConfig's actual default (not a hardcoded literal here).
        let cfg = ds_config::VoiceConfig::default();
        let expected_name = if cfg!(target_os = "macos") {
            "Tink" // /System/Library/Sounds/Tink.aiff
        } else if cfg!(target_os = "windows") {
            "ding" // C:\Windows\Media\ding.wav
        } else if cfg!(target_os = "linux") {
            "message" // freedesktop message.oga
        } else {
            ""
        };
        assert_eq!(cfg.earcon_reply_sound, expected_name);
        let dir = tempfile::tempdir().unwrap();
        let sounds = dir.path().join("sounds");
        std::fs::create_dir_all(&sounds).unwrap();
        let want = sounds.join(format!("{expected_name}.{}", sound_ext()));
        std::fs::write(&want, b"fixture").unwrap();
        let want = want.canonicalize().ok();
        assert_eq!(
            resolve_cue_in(
                &cfg.earcon_reply_sound,
                &cfg.earcon_needs_input_sound,
                EarconEvent::ReplyDone,
                std::slice::from_ref(&sounds),
                sound_ext(),
            ),
            want
        );
        // Needs-input ships off (empty).
        assert_eq!(cfg.earcon_needs_input_sound, "");
        assert_eq!(
            resolve_cue_in(
                &cfg.earcon_reply_sound,
                &cfg.earcon_needs_input_sound,
                EarconEvent::NeedsInput,
                std::slice::from_ref(&sounds),
                sound_ext(),
            ),
            None
        );
    }

    #[test]
    fn canonical_sound_path_stays_within_allowed_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sounds = dir.path().join("sounds");
        let sibling = dir.path().join("sounds-backup");
        std::fs::create_dir_all(&sounds).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let allowed = sounds.join("ding.wav");
        let outside = sibling.join("ding.wav");
        std::fs::write(&allowed, b"RIFF....").unwrap();
        std::fs::write(&outside, b"RIFF....").unwrap();

        assert_eq!(
            canonical_sound_path_in(&allowed, std::slice::from_ref(&sounds)),
            allowed.canonicalize().ok()
        );
        assert_eq!(canonical_sound_path_in(&outside, &[sounds]), None);
    }

    #[test]
    fn arbitrary_absolute_sound_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let snd = dir.path().join("ding.wav");
        std::fs::write(&snd, b"RIFF....").unwrap();

        assert_eq!(
            resolve_cue_in(
                &snd.to_string_lossy(),
                "",
                EarconEvent::ReplyDone,
                &[],
                sound_ext(),
            ),
            None
        );
    }

    #[test]
    fn system_sounds_are_size_sorted_and_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let fallback = dir.path().join("fallback");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(primary.join("large.wav"), b"12345").unwrap();
        std::fs::write(primary.join("same.wav"), b"123").unwrap();
        std::fs::write(fallback.join("small.wav"), b"1").unwrap();
        std::fs::write(fallback.join("same.wav"), b"x").unwrap();
        std::fs::write(fallback.join("ignored.txt"), b"").unwrap();

        let sounds = system_sounds_in(&[primary.clone(), fallback], "wav");
        assert_eq!(
            sounds.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["small", "same", "large"]
        );
        assert_eq!(
            sounds.iter().find(|s| s.name == "same").unwrap().path,
            primary.join("same.wav"),
            "the first directory wins when names collide"
        );
        for w in sounds.windows(2) {
            assert!(
                (w[0].bytes, &w[0].name) <= (w[1].bytes, &w[1].name),
                "system_sounds must be size-then-name sorted"
            );
            assert_ne!(w[0].name, w[1].name, "names are de-duped");
        }
    }
}
