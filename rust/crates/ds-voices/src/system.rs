//! OS-default System-TTS voice name — extracted from `ds-tts`'s `system.rs`
//! (issue #5) so [`crate::enumerate`]`::voice_display_name` (System arm) can use
//! it without a `ds-proc`/synth dependency. Spawn/synth paths stay in `ds-tts`.

/// DEFAULT system-TTS voice name as the OS reports it — what System speaks with
/// when `tts_system_voice` is empty. Used to NAME "who is speaking" for the
/// greeting. Returns the raw OS name (e.g. Windows `"Microsoft Hazel Desktop"`);
/// the caller tidies for display. `None` if unresolved.
/// * Windows → `System.Speech` current voice (same engine `ds_tts::system::say_command`
///   speaks through, so the name matches what's heard).
/// * macOS → Spoken Content System Voice (`SelectedVoiceName`, else derived from
///   `SelectedVoiceID` — see [`default_voice_name`]).
/// * Linux → unresolved; greeting omits the voice name (issue #74).
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

/// macOS: voice `say` speaks with by default — NSSpeechSynthesizer's selected voice
/// in speech prefs (via `defaults`, no AppKit). Prefers friendly `SelectedVoiceName`;
/// falls back to a name DERIVED from `SelectedVoiceID` for selections that recorded
/// only the id (migrated prefs). `None` if NEITHER is set (OS unnamed built-in —
/// greet name-lessly rather than name a voice we can't confirm is heard).
#[cfg(target_os = "macos")]
pub fn default_voice_name() -> Option<String> {
    read_voice_pref("SelectedVoiceName").or_else(|| {
        read_voice_pref("SelectedVoiceID").and_then(|id| name_from_voice_identifier(&id))
    })
}

/// One key from the macOS speech-voice prefs domain. `None` if absent, failed, or empty.
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

/// Speakable name from a macOS voice IDENTIFIER — trailing dot-segment of the
/// reverse-DNS id: `…en-US.Samantha` → `"Samantha"`. Legacy all-lowercase
/// (`…voice.fred`) is capitalized → `"Fred"`; already-cased names left as-is;
/// no dots → whole id. `None` if trailing segment empty.
#[cfg(target_os = "macos")]
fn name_from_voice_identifier(id: &str) -> Option<String> {
    let seg = id.trim().rsplit('.').next().unwrap_or("").trim();
    if seg.is_empty() {
        return None;
    }
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
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::name_from_voice_identifier;

    #[test]
    fn voice_id_yields_trailing_name() {
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
        );
        assert_eq!(name_from_voice_identifier("trailing."), None);
        assert_eq!(name_from_voice_identifier(""), None);
        assert_eq!(name_from_voice_identifier("   "), None);
    }
}
