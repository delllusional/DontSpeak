//! The OS-default System-TTS voice's name — extracted from `ds-tts`'s `system.rs`
//! (issue #5) since only [`crate::enumerate`]'s `voice_display_name` (System arm) needs
//! it, and this crate carries no `ds-proc`/synth dependency. The REST of ds-tts's
//! `system.rs` (`SystemTts`, `say_command`, `spawn`, `set_new_pgroup`,
//! `open_voice_settings`) stays in `ds-tts` — those need `ds-proc` (pidfile, process
//! groups) and are the real synth/spawn path, not enumeration.

/// The DEFAULT system-TTS voice's name as the OS reports it — what the System engine actually
/// speaks with when `tts_system_voice` is empty. Used to NAME "who is speaking" (the greeting)
/// for the OS-default voice. Returns the raw OS name (e.g. Windows `"Microsoft Hazel Desktop"`);
/// the caller tidies it for display. `None` if it can't be resolved.
/// * Windows → the `System.Speech` synthesizer's current voice (the SAME engine
///   `ds_tts::system::say_command` speaks through, so the name always matches what's heard).
/// * macOS   → the System Voice from Spoken Content (`SelectedVoiceName`, else a name
///   derived from the `SelectedVoiceID` identifier — see [`default_voice_name`]).
/// * Linux   → TODO (not wired yet — falls back to a name-less greeting).
#[cfg(target_os = "windows")]
pub fn default_voice_name() -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -AssemblyName System.Speech; \
             (New-Object System.Speech.Synthesis.SpeechSynthesizer).Voice.Name",
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// macOS: the voice `say` speaks with by default — NSSpeechSynthesizer's selected voice,
/// stored in the speech prefs (read via `defaults`, so no AppKit link). This is the System
/// Voice set in Spoken Content, i.e. exactly what `say` (no `-v`) uses. Prefers the friendly
/// `SelectedVoiceName`; falls back to a name DERIVED from the `SelectedVoiceID` identifier for
/// selections that recorded only the id (e.g. migrated prefs). `None` if NEITHER is set (the OS
/// then picks an unnamed built-in default — we'd rather greet name-lessly than name a voice we
/// can't confirm is the one actually heard).
#[cfg(target_os = "macos")]
pub fn default_voice_name() -> Option<String> {
    read_voice_pref("SelectedVoiceName").or_else(|| {
        read_voice_pref("SelectedVoiceID").and_then(|id| name_from_voice_identifier(&id))
    })
}

/// Read one key from the macOS speech-voice prefs domain. `None` if the key/domain is absent,
/// the read fails, or the value is empty.
#[cfg(target_os = "macos")]
fn read_voice_pref(key: &str) -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "com.apple.speech.voice.prefs", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Derive a speakable voice name from a macOS voice IDENTIFIER — the trailing dot-segment of
/// the reverse-DNS id: `com.apple.voice.compact.en-US.Samantha` → `"Samantha"`,
/// `com.apple.speech.synthesis.voice.Alex` → `"Alex"`. A legacy all-lowercase segment
/// (`…voice.fred`) is capitalized → `"Fred"`; an already-cased name (`Samantha`, `Ava`) is left
/// as-is; an id with no dots is taken whole. `None` if the trailing segment is empty.
#[cfg(target_os = "macos")]
fn name_from_voice_identifier(id: &str) -> Option<String> {
    let seg = id.trim().rsplit('.').next().unwrap_or("").trim();
    if seg.is_empty() {
        return None;
    }
    // Capitalize a legacy lowercase token; leave already-cased names (Samantha, Ava) untouched.
    if seg.chars().all(|c| c.is_ascii_lowercase()) {
        let mut chars = seg.chars();
        let first = chars.next().unwrap().to_ascii_uppercase();
        Some(std::iter::once(first).chain(chars).collect())
    } else {
        Some(seg.to_string())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn default_voice_name() -> Option<String> {
    None // TODO(linux): resolve the Speech Dispatcher default voice name.
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::name_from_voice_identifier;

    #[test]
    fn voice_id_yields_trailing_name() {
        // Modern reverse-DNS identifiers: the friendly name is the last dot-segment, already cased.
        for (id, want) in [
            ("com.apple.voice.compact.en-US.Samantha", "Samantha"),
            ("com.apple.voice.premium.en-US.Ava", "Ava"),
            ("com.apple.speech.synthesis.voice.Alex", "Alex"),
        ] {
            assert_eq!(
                name_from_voice_identifier(id).as_deref(),
                Some(want),
                "id={id}"
            );
        }
    }

    #[test]
    fn voice_id_capitalizes_legacy_lowercase() {
        // Legacy lowercase tokens are capitalized so the greeting reads naturally.
        assert_eq!(
            name_from_voice_identifier("com.apple.speech.synthesis.voice.fred").as_deref(),
            Some("Fred")
        );
        assert_eq!(
            name_from_voice_identifier("samantha").as_deref(),
            Some("Samantha")
        );
    }

    #[test]
    fn voice_id_handles_bare_and_empty() {
        assert_eq!(
            name_from_voice_identifier("Daniel").as_deref(),
            Some("Daniel")
        ); // no dots
        assert_eq!(name_from_voice_identifier("trailing."), None); // empty trailing segment
        assert_eq!(name_from_voice_identifier(""), None);
        assert_eq!(name_from_voice_identifier("   "), None);
    }
}
