//! Audible earcons — the reply "ding" (Claude finished its turn) and a distinct
//! needs-input cue (Claude is waiting on you). The engine resolves an event to a concrete
//! sound FILE and hands the path to the warm helper, which plays it through its existing
//! rodio output; nothing here opens audio.
//!
//! The configured sound IS each cue's on/off — there is no separate enable flag. The value is
//! either a bundled system-sound NAME or a path within a platform sound directory; empty =
//! this cue is OFF. The reply
//! ding defaults to the OS's bundled chime by name — `"ding"` on Windows, `"Tink"` on macOS
//! (the historical chime), `"message"` on Linux — so it rings out of the box on every OS. A
//! bare name resolves THROUGH [`system_sounds`]: matched (case-insensitively) to the real file
//! in the OS's sounds folder (e.g. `"ding"` → `C:\Windows\Media\ding.wav`, `"Tink"` →
//! `/System/Library/Sounds/Tink.aiff`), never a hardcoded path. Anything that doesn't resolve
//! to an existing file is effectively off (fail-quiet, no ding). [`system_sounds`] enumerates
//! the OS's bundled sounds by INTROSPECTION (the per-OS sound dir + extension is the only
//! constant) and also feeds a UI sound picker.

use std::path::{Path, PathBuf};

/// The distinct eyes-free cues. `ReplyDone` = Claude finished its turn (wired to the Stop
/// hook); `NeedsInput` = Claude is waiting on you — a permission prompt or idle (wired to the
/// Notification hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarconEvent {
    ReplyDone,
    NeedsInput,
}

impl EarconEvent {
    /// Parse the wire token the engine receives over IPC (`"reply_done"` / `"needs_input"`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "reply_done" => Some(Self::ReplyDone),
            "needs_input" => Some(Self::NeedsInput),
            _ => None,
        }
    }

    /// The canonical wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplyDone => "reply_done",
            Self::NeedsInput => "needs_input",
        }
    }

    /// The configured sound for this event (trimmed). Empty = this cue is OFF.
    fn sound_in<'a>(self, reply_sound: &'a str, needs_input_sound: &'a str) -> &'a str {
        match self {
            Self::ReplyDone => reply_sound.trim(),
            Self::NeedsInput => needs_input_sound.trim(),
        }
    }
}

/// A bundled system sound discovered by [`system_sounds`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSound {
    /// The file stem (no extension), the name a config sound matches against.
    pub name: String,
    pub path: PathBuf,
    /// File size in bytes (smaller ≈ shorter cue) — the UI-picker sort key.
    pub bytes: u64,
}

/// The OS directories bundled system sounds live in — a well-known OS CONVENTION, not a
/// hardcoded sound list. The files inside are enumerated by [`system_sounds`].
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

/// Canonicalize a cue path and keep it within one of the platform sound directories.
/// This is the trust boundary before a path reaches the warm helper's file-opening protocol.
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

/// The file extension the platform's bundled system sounds carry, so the dir scan finds only
/// playable cues: aiff (macOS), wav (Windows), oga/ogg (Linux). The helper decodes all three
/// via rodio's symphonia decoders.
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

/// Enumerate the OS's bundled system sounds by INTROSPECTION: scan the platform sound dir(s)
/// for files with the platform extension. Sorted by file SIZE then name (smallest first),
/// de-duped by name (earlier dirs win) — so a bare-name sound resolves with NO hardcoded
/// names, and a UI picker can list the shortest cues first.
pub fn system_sounds() -> Vec<SystemSound> {
    system_sounds_in(&sound_dirs(), sound_ext())
}

/// Resource-explicit core of [`system_sounds`]. Production passes the platform sound
/// directories; tests pass tempdir fixtures so the test harness never scans installed or
/// user-owned sounds.
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

/// Resolve an `event` to a concrete sound file to play, or `None` (callers fail-quiet → no
/// ding). The configured sound IS the on/off: empty ⇒ `None` (off); an absolute path ⇒ used
/// only when its canonical target is inside a platform sound directory; a bare NAME (the
/// default is `"ding"`) ⇒ matched case-insensitively against the enumerated system sounds.
/// Anything that doesn't resolve to an allowed existing file is `None` = effectively off.
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
        return None; // no sound set ⇒ this cue is off
    }
    let p = PathBuf::from(sound);
    if p.is_absolute() {
        return canonical_sound_path_in(&p, sound_dirs);
    }
    // A bare name → the matching bundled sound (case-insensitive), else nothing (off).
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
        // An explicitly-empty sound ⇒ the cue is off, regardless of any installed OS sounds.
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
        // The shipped default is the OS's bundled chime by NAME — on out of the box. Pin
        // against ds_config::VoiceConfig's actual default (not just a hardcoded literal here)
        // so a typo in ds-config's default_earcon_reply() would fail THIS test.
        let cfg = ds_config::VoiceConfig::default();
        let expected_name = if cfg!(target_os = "macos") {
            "Tink" // /System/Library/Sounds/Tink.aiff (the historical macOS chime)
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
        // The needs-input cue ships off (empty) — like the historically-unwired earcon.
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
