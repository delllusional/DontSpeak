//! Audible earcons — the reply "ding" (Claude finished its turn) and a distinct
//! needs-input cue (Claude is waiting on you). The engine resolves an event to a concrete
//! sound FILE and hands the path to the warm helper, which plays it through its existing
//! rodio output; nothing here opens audio.
//!
//! The configured sound IS each cue's on/off — there is no separate enable flag. The value is
//! either a bundled system-sound NAME or an absolute PATH; empty = this cue is OFF. The reply
//! ding defaults to the OS's bundled chime by name — `"ding"` on Windows, `"Tink"` on macOS
//! (the historical chime), `"message"` on Linux — so it rings out of the box on every OS. A
//! bare name resolves THROUGH [`system_sounds`]: matched (case-insensitively) to the real file
//! in the OS's sounds folder (e.g. `"ding"` → `C:\Windows\Media\ding.wav`, `"Tink"` →
//! `/System/Library/Sounds/Tink.aiff`), never a hardcoded path. Anything that doesn't resolve
//! to an existing file is effectively off (fail-quiet, no ding). [`system_sounds`] enumerates
//! the OS's bundled sounds by INTROSPECTION (the per-OS sound dir + extension is the only
//! constant) and also feeds a UI sound picker.

use std::path::PathBuf;

use crate::VoiceConfig;

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
    fn sound_in(self, cfg: &VoiceConfig) -> &str {
        match self {
            Self::ReplyDone => cfg.earcon_reply_sound.trim(),
            Self::NeedsInput => cfg.earcon_needs_input_sound.trim(),
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
    let ext = sound_ext();
    let mut out: Vec<SystemSound> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dir in sound_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
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
                out.push(SystemSound { name, path, bytes: meta.len() });
            }
        }
    }
    out.sort_by(|a, b| a.bytes.cmp(&b.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Resolve an `event` to a concrete sound file to play, or `None` (callers fail-quiet → no
/// ding). The configured sound IS the on/off: empty ⇒ `None` (off); an absolute path ⇒ used
/// as-is (must exist); a bare NAME (the default is `"ding"`) ⇒ matched case-insensitively
/// against the enumerated system sounds. Anything that doesn't resolve to an existing file is
/// `None` = effectively off (e.g. `"ding"` resolves on Windows but not on macOS/Linux, which
/// have no `ding.*` bundled sound — set an OS-appropriate name or a path there).
pub fn resolve_cue(cfg: &VoiceConfig, event: EarconEvent) -> Option<PathBuf> {
    let sound = event.sound_in(cfg);
    if sound.is_empty() {
        return None; // no sound set ⇒ this cue is off
    }
    let p = PathBuf::from(sound);
    if p.is_absolute() {
        return p.is_file().then_some(p);
    }
    // A bare name → the matching bundled sound (case-insensitive), else nothing (off).
    system_sounds()
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(sound))
        .map(|s| s.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_token_roundtrips() {
        for ev in [EarconEvent::ReplyDone, EarconEvent::NeedsInput] {
            assert_eq!(EarconEvent::parse(ev.as_str()), Some(ev));
        }
        assert_eq!(EarconEvent::parse("  reply_done "), Some(EarconEvent::ReplyDone));
        assert_eq!(EarconEvent::parse("bogus"), None);
    }

    #[test]
    fn empty_sound_is_off() {
        // An explicitly-empty sound ⇒ the cue is off, regardless of any installed OS sounds.
        let cfg = VoiceConfig {
            earcon_reply_sound: String::new(),
            earcon_needs_input_sound: "   ".into(), // whitespace trims to empty ⇒ off
            ..VoiceConfig::default()
        };
        assert_eq!(resolve_cue(&cfg, EarconEvent::ReplyDone), None);
        assert_eq!(resolve_cue(&cfg, EarconEvent::NeedsInput), None);
    }

    #[test]
    fn default_reply_sound_is_the_os_chime() {
        // The shipped default is the OS's bundled chime by NAME — on out of the box.
        let cfg = VoiceConfig::default();
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
        // The name resolves THROUGH system_sounds to the real file in the OS sounds folder — or
        // None if that sound isn't installed. Assert it matches the introspected lookup so the
        // test is deterministic wherever it runs.
        let want = system_sounds()
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(expected_name))
            .map(|s| s.path);
        assert_eq!(resolve_cue(&cfg, EarconEvent::ReplyDone), want);
        // The needs-input cue ships off (empty) — like the historically-unwired earcon.
        assert_eq!(cfg.earcon_needs_input_sound, "");
        assert_eq!(resolve_cue(&cfg, EarconEvent::NeedsInput), None);
    }

    #[test]
    fn absolute_sound_resolves_only_when_present() {
        // An absolute-path sound is used verbatim when the file exists, and yields None
        // (effectively off) when it doesn't — independent of any OS sounds being installed.
        let dir = tempfile::tempdir().unwrap();
        let snd = dir.path().join("ding.wav");
        std::fs::write(&snd, b"RIFF....").unwrap();

        let cfg = VoiceConfig {
            earcon_reply_sound: snd.to_string_lossy().into_owned(),
            earcon_needs_input_sound: dir.path().join("missing.wav").to_string_lossy().into_owned(),
            ..VoiceConfig::default()
        };
        assert_eq!(resolve_cue(&cfg, EarconEvent::ReplyDone), Some(snd));
        assert_eq!(resolve_cue(&cfg, EarconEvent::NeedsInput), None);
    }

    #[test]
    fn system_sounds_are_size_sorted_and_deduped() {
        // Pure invariant of the ordering (independent of which sounds the host has): sorted by
        // (bytes, name) and unique by name.
        let sounds = system_sounds();
        for w in sounds.windows(2) {
            assert!(
                (w[0].bytes, &w[0].name) <= (w[1].bytes, &w[1].name),
                "system_sounds must be size-then-name sorted"
            );
            assert_ne!(w[0].name, w[1].name, "names are de-duped");
        }
    }
}
