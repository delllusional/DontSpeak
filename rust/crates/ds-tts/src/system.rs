//! SystemTts — the OS's built-in speech synthesizer.
//!
//!   * macOS (COMPILED here): `say -r <wpm> [-v <name>] <text>`, spawned in its own
//!     process group, pgid recorded in the pidfile. `voices()` delegates to the
//!     canonical [`crate::enumerate::system_voices`] (`say -v ?`).
//!     `manage_voices()` opens the Accessibility / Spoken-Content settings pane.
//!   * Windows (cfg, NOT built): PowerShell `System.Speech.Synthesis`
//!     `SelectVoice` + `Rate(-10..10)` mapped from `rate`; `manage_voices`
//!     `ms-settings:speech`.
//!   * Linux (cfg, NOT built): `spd-say -r <-100..100>`;
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
    /// (its Speech Dispatcher path isn't wired into the engine yet).
    pub fn available() -> bool {
        cfg!(any(target_os = "macos", target_os = "windows"))
    }
}

/// Set the spawned child into its own session/process group so the recorded pgid
/// kills the whole tree on barge-in. Shared with `kokoro::spawn`.
#[cfg(unix)]
pub(crate) fn set_new_pgroup(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the pre_exec closure runs in the forked child before exec; setsid is
    // async-signal-safe, and the closure captures nothing, allocates nothing, and takes
    // no locks — meeting pre_exec's contract.
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
/// - Linux → unavailable; Speech Dispatcher has no portable settings page (issue #74).
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
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill. The trait path then drops the Child; the
        // caller waits by pgid / pidfile.
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
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
/// Double every code point PowerShell's tokenizer accepts as a single quote — U+0027 plus the
/// smart-quote variants U+2018..=U+201B. Any of them terminates a `'…'` literal, so doubling
/// only the ASCII apostrophe let a curly apostrophe ("don’t", ubiquitous in agent prose, and
/// deliberately preserved by the Markdown frontend) break the script — and a crafted
/// `x’); <cmd>; (‘y` payload escape the string into executable position.
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
    // System.Speech.Synthesis: Rate is -10..10; map 0.5..=2.0 with 1.0 -> 0.
    //
    // The 0.5..1.0 (slow) and 1.0..2.0 (fast) halves span the SAME 10-point half-range
    // of Rate (-10..0 and 0..10 respectively) but cover DIFFERENT-sized `rate` spans
    // (0.5 vs 1.0), so a single `(r - 1.0) * k` slope can't hit both ends: `k = 10`
    // reaches +10 at r=2.0 but only -5 at r=0.5 (half the intended slow-down range).
    // Piecewise-linear with a steeper slope below 1.0 hits the documented floor too.
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
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill.
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
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
        // Same piecewise mapping as the Windows SAPI branch above, and for the same
        // reason: 0.5..1.0 and 1.0..2.0 both need to span the full -100..0 / 0..100
        // half-range despite covering different-sized `rate` spans, so a single
        // slope can't reach -100 at the documented slowest setting (r=0.5).
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
        // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
        // ds_proc::record_or_kill.
        let pgid = ds_proc::record_or_kill(&self.paths.pidfile, &child)?;
        drop(child);
        Ok(SpeakHandle { pgid })
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

    /// A missing configured voice must be a process failure, not silent fallback to the default
    /// voice. Empty text keeps this contract check non-audible.
    #[test]
    fn say_command_with_invalid_voice_fails_without_speaking() {
        let status = say_command(Some("__DontSpeak test voice that cannot exist__"), 1.0, "")
            .spawn()
            .expect("spawn powershell")
            .wait()
            .expect("wait");
        assert!(
            !status.success(),
            "invalid SelectVoice unexpectedly succeeded"
        );
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
