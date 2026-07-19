//! SystemTts — OS built-in speech synthesizer.
//!
//!   * macOS (compiled): `say -r <wpm> [-v <name>] <text>`, own process group + pidfile.
//!     `voices()` → [`crate::enumerate::system_voices`]; manage → Spoken Content.
//!   * Windows (cfg): PowerShell `System.Speech`; manage → `ms-settings:speech`.
//!   * Linux (cfg): `spd-say`; voices empty; no manage.

use std::process::Command;

use ds_config::Paths;

use crate::{SpeakHandle, SpeakerVoice, Tts};

/// System TTS engine.
pub struct SystemTts {
    paths: Paths,
}

impl SystemTts {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// Available on this build target? macOS/Windows yes; Linux not wired yet.
    pub fn available() -> bool {
        cfg!(any(target_os = "macos", target_os = "windows"))
    }
}

/// Own session/process group so recorded pgid kills the whole tree on barge-in.
/// Shared with `kokoro::spawn`.
#[cfg(unix)]
pub(crate) fn set_new_pgroup(cmd: &mut Command) {
    ds_proc::set_new_process_group(cmd);
}

/// OS system-voice settings — ONE cross-platform seam for every UI's "Manage voices"
/// (`ds_open_voice_settings`). Returns true if a page was launched.
/// - macOS → Accessibility ▸ Spoken Content (modern then legacy anchor)
/// - Windows → `ms-settings:speech` (only TTS deep link Windows exposes)
/// - Linux → unavailable (issue #74)
#[cfg(target_os = "macos")]
pub fn open_voice_settings() -> bool {
    for uri in [
        "x-apple.systempreferences:com.apple.Accessibility-Settings.extension?SpokenContent",
        "x-apple.systempreferences:com.apple.preference.universalaccess?SpeakableItems",
    ] {
        if Command::new("open")
            .arg(uri)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
pub fn open_voice_settings() -> bool {
    use std::os::windows::process::CommandExt;
    Command::new("cmd")
        .args(["/c", "start", "", "ms-settings:speech"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW — no console flash
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
pub fn open_voice_settings() -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn open_voice_settings() -> bool {
    false
}

/// `say` flags only (`-r`/`-v`, null Stdio). Call sites own text/pgroup/spawn/pidfile.
/// Single source for free `spawn`, `SystemTts::speak`, and dontspeakd::speak_system.
#[cfg(target_os = "macos")]
pub fn say_command(voice: Option<&str>, rate: f32) -> Command {
    use std::process::Stdio;
    let wpm = crate::rate_to_wpm(rate);
    let mut cmd = Command::new("say");
    cmd.arg("-r").arg(wpm.to_string());
    if let Some(v) = voice.filter(|v| !v.trim().is_empty()) {
        cmd.arg("-v").arg(v);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Spawn macOS `say` as live `Child` in own process group; return `(Child, pgid)`.
/// Counterpart to `kokoro::spawn` for foreground hooks that keep the Child.
#[cfg(target_os = "macos")]
pub fn spawn(
    paths: &ds_config::Paths,
    txt: &str,
    voice_id: &str,
    rate: f32,
) -> std::io::Result<(std::process::Child, i32)> {
    let mut cmd = say_command(Some(voice_id), rate);
    cmd.arg(txt);
    set_new_pgroup(&mut cmd);
    let child = cmd.spawn()?;
    // SACRED single-speaker post-spawn (ARCHITECTURE §0.2) — ds_proc::record_or_kill.
    let pgid = ds_proc::record_or_kill(&paths.pidfile, &child)?;
    Ok((child, pgid))
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS (compiled & verified on the build host)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "macos")]
impl Tts for SystemTts {
    fn speak(&self, text: &str, voice_id: Option<&str>, rate: f32) -> std::io::Result<SpeakHandle> {
        let mut cmd = say_command(voice_id, rate);
        cmd.arg(text);
        set_new_pgroup(&mut cmd);

        let child = cmd.spawn()?;
        // SACRED single-speaker post-spawn (ARCHITECTURE §0.2). Trait path drops Child;
        // caller waits by pgid / pidfile.
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        crate::enumerate::system_voices()
    }

    fn can_manage_voices(&self) -> bool {
        true
    }

    fn manage_voices(&self) {
        let _ = open_voice_settings();
    }

    fn manage_voices_hint(&self) -> Option<&str> {
        Some("Spoken Content > System Voice > Manage Voices…")
    }

    fn kind(&self) -> &'static str {
        "system"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows (cfg, NOT built on the macOS host)
// ─────────────────────────────────────────────────────────────────────────────

/// Double every code point PowerShell tokenizes as a single quote — U+0027 plus
/// smart quotes U+2018..=U+201B. Any of them ends a `'…'` literal; doubling only
/// ASCII apostrophe broke ordinary "don’t" and allowed injection (`x’); <cmd>`).
#[cfg(target_os = "windows")]
fn ps_squote_escape(text: &str) -> String {
    const PS_SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if PS_SINGLE_QUOTES.contains(&ch) {
            escaped.push(ch);
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(target_os = "windows")]
pub fn say_command(voice: Option<&str>, rate: f32, text: &str) -> Command {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    // SAPI Rate -10..10 from rate 0.5..=2.0 (1.0 → 0). Slow/fast halves span the
    // same Rate half-range but different `rate` spans, so one slope can't hit both
    // ends (k=10 → +10 at 2.0 but only -5 at 0.5). Piecewise: steeper below 1.0.
    let r = rate.clamp(0.5, 2.0);
    let ps_rate = if r < 1.0 {
        ((r - 1.0) * 20.0).round() as i32 // 0.5->-10 .. 1.0->0
    } else {
        ((r - 1.0) * 10.0).round() as i32 // 1.0->0 .. 2.0->10
    };
    let esc_text = ps_squote_escape(text);
    let select = match voice.filter(|v| !v.trim().is_empty()) {
        Some(v) => format!("$s.SelectVoice('{}');", ps_squote_escape(v)),
        None => String::new(),
    };
    let script = format!(
        "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         {select}$s.Rate = {ps_rate}; $s.Speak('{esc_text}')"
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW — no console flash on speak
    cmd
}

#[cfg(target_os = "windows")]
impl Tts for SystemTts {
    fn speak(&self, text: &str, voice_id: Option<&str>, rate: f32) -> std::io::Result<SpeakHandle> {
        let mut cmd = say_command(voice_id, rate, text);
        let child = cmd.spawn()?;
        // SACRED single-speaker post-spawn (ARCHITECTURE §0.2).
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        crate::enumerate::system_voices()
    }

    fn can_manage_voices(&self) -> bool {
        true
    }
    fn manage_voices(&self) {
        let _ = open_voice_settings();
    }
    fn manage_voices_hint(&self) -> Option<&str> {
        Some("Time & Language > Speech > Manage voices (Add voices)")
    }
    fn kind(&self) -> &'static str {
        "system"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux (cfg, NOT built on the macOS host)
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
impl Tts for SystemTts {
    fn speak(
        &self,
        text: &str,
        _voice_id: Option<&str>,
        rate: f32,
    ) -> std::io::Result<SpeakHandle> {
        use std::process::Stdio;
        // Same piecewise map as Windows SAPI (full -100..100; single slope can't
        // hit -100 at r=0.5).
        let r = rate.clamp(0.5, 2.0);
        let spd_rate = if r < 1.0 {
            ((r - 1.0) * 200.0).round() as i32 // 0.5->-100 .. 1.0->0
        } else {
            ((r - 1.0) * 100.0).round() as i32 // 1.0->0 .. 2.0->100
        };
        let mut cmd = Command::new("spd-say");
        cmd.arg("-r").arg(spd_rate.to_string()).arg("-w").arg(text);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        set_new_pgroup(&mut cmd);
        let child = cmd.spawn()?;
        // SACRED single-speaker post-spawn (ARCHITECTURE §0.2).
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        crate::enumerate::system_voices()
    }

    // No installer on Linux (§B.3).
    fn kind(&self) -> &'static str {
        "system"
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::say_command;
    use std::ffi::OsStr;

    fn args(c: &std::process::Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn say_command_includes_voice_and_rate() {
        let cmd = say_command(Some("Samantha"), 1.0);
        assert_eq!(cmd.get_program(), OsStr::new("say"));
        assert_eq!(args(&cmd), vec!["-r", "175", "-v", "Samantha"]);
    }

    #[test]
    fn say_command_omits_voice_when_empty() {
        assert_eq!(args(&say_command(None, 1.0)), vec!["-r", "175"]);
        assert_eq!(args(&say_command(Some("   "), 1.0)), vec!["-r", "175"]);
    }

    #[test]
    fn say_command_maps_rate_extremes() {
        assert_eq!(args(&say_command(None, 0.5)), vec!["-r", "88"]);
        assert_eq!(args(&say_command(None, 2.0)), vec!["-r", "350"]);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::{ps_squote_escape, say_command};
    use std::ffi::OsStr;

    /// Regression (audit): only the ASCII apostrophe was doubled, so any smart-quote code
    /// point (all tokenized as single quotes by PowerShell) terminated the `'…'` literal —
    /// a parse failure for ordinary "don’t" prose and a script-injection vector for crafted
    /// text. Every variant must reach the generated script doubled.
    #[test]
    fn say_command_escapes_every_powershell_single_quote_variant() {
        assert_eq!(
            ps_squote_escape("a'b\u{2018}c\u{2019}d\u{201A}e\u{201B}f"),
            "a''b\u{2018}\u{2018}c\u{2019}\u{2019}d\u{201A}\u{201A}e\u{201B}\u{201B}f"
        );

        let cmd = say_command(
            Some("O\u{2019}Brien"),
            1.0,
            "don\u{2019}t \u{2018}quote\u{2019}",
        );
        let script = cmd
            .get_args()
            .last()
            .expect("-Command script")
            .to_string_lossy()
            .into_owned();
        for needle in [
            "O\u{2019}\u{2019}Brien",
            "don\u{2019}\u{2019}t",
            "\u{2018}\u{2018}quote\u{2019}\u{2019}",
        ] {
            assert!(script.contains(needle), "missing {needle:?} in {script:?}");
        }
    }
    fn args(c: &std::process::Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn say_command_builds_the_default_voice_script_without_spawning() {
        let cmd = say_command(None, 1.0, "It's wired.");
        assert_eq!(cmd.get_program(), OsStr::new("powershell"));
        let args = args(&cmd);
        assert_eq!(&args[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
        let script = &args[3];
        assert!(script.contains("$s.Rate = 0"));
        assert!(script.contains("$s.Speak('It''s wired.')"));
        assert!(!script.contains("SelectVoice"));
    }

    #[test]
    fn say_command_escapes_the_named_voice_and_maps_rate_extremes() {
        let slow = args(&say_command(Some("Reader's Voice"), 0.5, "hello"));
        assert!(slow[3].contains("$s.SelectVoice('Reader''s Voice')"));
        assert!(slow[3].contains("$s.Rate = -10"));

        let fast = args(&say_command(Some("   "), 2.0, "hello"));
        assert!(!fast[3].contains("SelectVoice"));
        assert!(fast[3].contains("$s.Rate = 10"));
    }
}

// Fallback Tts impl for any other target (keeps the type usable; never built in
// practice). Compiled only off macOS/windows/linux.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl Tts for SystemTts {
    fn speak(
        &self,
        _text: &str,
        _voice_id: Option<&str>,
        _rate: f32,
    ) -> std::io::Result<SpeakHandle> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "system TTS unsupported on this target",
        ))
    }
    fn kind(&self) -> &'static str {
        "system"
    }
}
