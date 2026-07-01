//! SystemTts — the OS's built-in speech synthesizer.
//!
//!   * macOS (COMPILED here): `say -r <wpm> [-v <name>] <text>`, spawned in its own
//!     process group, pgid recorded in the pidfile. `voices()` delegates to the
//!     canonical [`crate::enumerate::system_voices`] (`say -v ?`).
//!     `manage_voices()` opens the Accessibility / Spoken-Content settings pane.
//!   * Windows (cfg, NOT built): PowerShell `System.Speech.Synthesis`
//!     `SelectVoice` + `Rate(-10..10)` mapped from `rate`; `manage_voices`
//!     `ms-settings:speech`.
//!   * Linux (cfg, NOT built): `spd-say -r <-100..100>` else `espeak -s <wpm>`;
//!     `voices()` best-effort empty; no `manage_voices_hint`.

use std::process::Command;

use ds_config::Paths;

use crate::{SpeakHandle, SpeakerVoice, Tts};

/// The system TTS engine.
pub struct SystemTts {
    paths: Paths,
}

impl SystemTts {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// Is a system TTS backend available on THIS build target? macOS always has `say`;
    /// Windows always has PowerShell `System.Speech.Synthesis`. Linux stays unavailable
    /// (its spd-say/espeak path isn't wired into the engine yet).
    pub fn available() -> bool {
        cfg!(any(target_os = "macos", target_os = "windows"))
    }
}

/// Set the spawned child into its own session/process group so the recorded pgid
/// kills the whole tree on barge-in. Shared with `kokoro::spawn`.
#[cfg(unix)]
pub(crate) fn set_new_pgroup(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
        });
    }
}

/// Open the OS's system-voice settings page — the ONE cross-platform seam behind every UI's
/// System-TTS "Manage voices" affordance, so macOS / Windows / Linux all launch the right
/// page from a single call (exposed to the apps as `ds_open_voice_settings`). Returns
/// true if a page was launched.
/// - macOS → System Settings ▸ Accessibility ▸ Spoken Content (where the `say` voices and
///   per-language packs live): the modern anchor, then the legacy one.
/// - Windows → Settings ▸ Time & language ▸ Speech (`ms-settings:speech` — the only Settings
///   deep link Windows exposes for TTS voices; its "Manage voices" adds voices).
/// - Linux → TODO: no portable system-voice settings page yet (spd-say/espeak are CLI).
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
    false // TODO(linux): wire a settings deep link when the system-voice path lands.
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn open_voice_settings() -> bool {
    false
}

/// The DEFAULT system-TTS voice's name as the OS reports it — what the System engine actually
/// speaks with when `tts_system_voice` is empty. Used to NAME "who is speaking" (the greeting)
/// for the OS-default voice. Returns the raw OS name (e.g. Windows `"Microsoft Hazel Desktop"`);
/// the caller tidies it for display. `None` if it can't be resolved.
/// * Windows → the `System.Speech` synthesizer's current voice (the SAME engine our
///   `say_command` speaks through, so the name always matches what's heard).
/// * macOS   → the System Voice from Spoken Content (`SelectedVoiceName`, else a name
///   derived from the `SelectedVoiceID` identifier — see [`default_voice_name`]).
/// * Linux   → TODO (not wired yet — falls back to a name-less greeting).
#[cfg(target_os = "windows")]
pub fn default_voice_name() -> Option<String> {
    use std::os::windows::process::CommandExt;
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
    let out = Command::new("defaults")
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
    None // TODO(linux): resolve the spd-say/espeak default voice name.
}

/// Build a `say` command with `-r <wpm>` (via [`crate::rate_to_wpm`]) and, when
/// non-empty, `-v <voice>`, plus all three null Stdio streams. Does NOT append
/// the text, set a process group, spawn, or touch the pidfile — each call site
/// adds `.arg(text)`, optional pgroup, and spawns/records itself. The single
/// source of the `say` argument vector so all three say spawners (free `spawn`,
/// `SystemTts::speak`, dontspeakd::speak_system) agree on flags + rate math.
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

/// Spawn the macOS `say` speaker as a live `Child` in its own process group,
/// record the pgid, and return `(Child, pgid)` — the spawn-helper counterpart to
/// `kokoro::spawn` so a foreground hook (ds-speak) can keep
/// owning + waiting on the child while still going through engine selection.
/// `voice_id`/`rate` map exactly as in the `Tts::speak` body.
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
    // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
    // ds_proc::record_or_kill.
    let pid = ds_proc::record_or_kill(&paths.pidfile, &child)?;
    Ok((child, pid))
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
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill. The trait path then drops the Child; the
        // caller waits by pgid / pidfile.
        let pid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid: pid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        // Single canonical `say -v ?` enumeration (self-cfg-gated off-host).
        crate::enumerate::system_voices()
    }

    fn can_manage_voices(&self) -> bool {
        true
    }

    fn manage_voices(&self) {
        // Open Accessibility ▸ Spoken Content via the shared cross-platform seam (§B.3).
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

/// Build the Windows system-TTS command: PowerShell driving `System.Speech.Synthesis`
/// with `SelectVoice` (when non-empty), `Rate` mapped from `rate` (0.5..=2.0 → -10..10,
/// 1.0 → 0), and `Speak(text)`. Single quotes are doubled for PowerShell escaping;
/// `CREATE_NO_WINDOW` keeps a console from flashing. Does NOT spawn — each call site spawns
/// and tracks the child itself. The single source of the Windows say invocation, shared by
/// the library `SystemTts` and dontspeakd::speak_system so they agree on rate math + escaping
/// (the macOS counterpart of the same name takes no `text` — it appends it via `.arg`).
#[cfg(target_os = "windows")]
pub fn say_command(voice: Option<&str>, rate: f32, text: &str) -> Command {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    // System.Speech.Synthesis: Rate is -10..10; map 0.5..=2.0 with 1.0 -> 0.
    let r = rate.clamp(0.5, 2.0);
    let ps_rate = ((r - 1.0) * 10.0).round() as i32; // 0.5->-5 .. 2.0->10
    // PowerShell single-quote escaping: double any embedded quote.
    let esc_text = text.replace('\'', "''");
    let select = match voice.filter(|v| !v.trim().is_empty()) {
        Some(v) => format!("$s.SelectVoice('{}');", v.replace('\'', "''")),
        None => String::new(),
    };
    let script = format!(
        "Add-Type -AssemblyName System.Speech; \
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
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill.
        let pid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid: pid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        // Single canonical enumeration entry (self-cfg-gated; empty off-macOS).
        crate::enumerate::system_voices()
    }

    fn can_manage_voices(&self) -> bool {
        true
    }
    fn manage_voices(&self) {
        // Open Time & language ▸ Speech via the shared cross-platform seam.
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
        // Prefer spd-say (-100..100, 0 = normal); fall back to espeak (-s wpm).
        let mut cmd = if which("spd-say") {
            let spd_rate = ((rate.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32;
            let mut c = Command::new("spd-say");
            c.arg("-r").arg(spd_rate.to_string()).arg("-w").arg(text);
            c
        } else {
            let wpm = crate::rate_to_wpm(rate);
            let mut c = Command::new("espeak");
            c.arg("-s").arg(wpm.to_string()).arg(text);
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        set_new_pgroup(&mut cmd);
        let child = cmd.spawn()?;
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill.
        let pid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid: pid })
    }

    fn voices(&self) -> Vec<SpeakerVoice> {
        // Single canonical enumeration entry (self-cfg-gated; empty off-macOS).
        crate::enumerate::system_voices()
    }

    // No system voice installer on Linux (§B.3): no manage_voices / hint.
    fn kind(&self) -> &'static str {
        "system"
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{name_from_voice_identifier, say_command};
    use std::ffi::OsStr;

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
    use super::say_command;

    /// The builder produces a runnable PowerShell `System.Speech` invocation that
    /// SYNTHESIZES on this machine — exit 0 means the OS spoke the text. Audible, so
    /// ignored by default (needs an audio device); run with `--ignored` to hear it.
    #[test]
    #[ignore = "audible; needs an audio device — run with --ignored"]
    fn say_command_speaks_on_this_machine() {
        let status = say_command(None, 1.0, "System voice wired into speak M C P.")
            .spawn()
            .expect("spawn powershell")
            .wait()
            .expect("wait");
        assert!(status.success(), "System.Speech.Speak exited {status:?}");
    }

    /// A specific installed voice name is honored (SelectVoice) and still exits 0.
    #[test]
    #[ignore = "audible; needs an audio device — run with --ignored"]
    fn say_command_with_named_voice_succeeds() {
        let status = say_command(Some("Microsoft Zira Desktop"), 1.0, "Zira here.")
            .spawn()
            .expect("spawn powershell")
            .wait()
            .expect("wait");
        assert!(status.success(), "named-voice Speak exited {status:?}");
    }
}

#[cfg(target_os = "linux")]
fn which(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file()))
        .unwrap_or(false)
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
