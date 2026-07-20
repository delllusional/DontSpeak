//! Default TTS via `ds-helper` in own process group. [`spawn`] → `(Child, pgid)` for
//! barge/pidfile. Fail-quiet if models/audio missing.

use std::process::{Child, Command, Stdio};

use ds_config::Paths;

use crate::{SpeakHandle, Tts};

/// Kokoro TTS via the ds-helper bin.
pub struct KokoroTts {
    paths: Paths,
}

impl KokoroTts {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }
}

impl Tts for KokoroTts {
    fn speak(&self, text: &str, voice_id: Option<&str>, rate: f32) -> std::io::Result<SpeakHandle> {
        // No fallback voice exists — Kokoro callers always name the assigned pool voice.
        let voice = voice_id.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "kokoro needs a voice id")
        })?;
        let (child, pgid) = spawn(&self.paths, text, voice, rate)?;
        // Trait path: return handle immediately; caller waits by pgid/pidfile.
        drop(child);
        Ok(SpeakHandle { pgid })
    }

    fn kind(&self) -> &'static str {
        "kokoro"
    }
}

/// `ds-helper`: sibling of current exe, else PATH.
fn helper_command() -> std::ffi::OsString {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("ds-helper");
        if sibling.is_file() {
            return sibling.into_os_string();
        }
    }
    std::ffi::OsString::from("ds-helper")
}

/// Spawn `ds-helper <txt> <voice> <rate>` in own process group; record pgid.
/// Shared by speak / ds-speak / ds-narrate (single spawn site).
pub fn spawn(paths: &Paths, txt: &str, voice: &str, rate: f32) -> std::io::Result<(Child, i32)> {
    // Mirror the warm helper's set-or-remove env contract (dontspeakd child_env): set
    // the config-resolved provider, clear the serve-mode vars — a one-shot helper must
    // not inherit STT/duplex/preload behavior from an ambient environment.
    let cfg = ds_config::VoiceConfig::load(paths);
    let mut cmd = Command::new(helper_command());
    cmd.arg(txt)
        .arg(voice)
        .arg(format!("{rate}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("DONTSPEAK_TTS_MODEL", "kokoro")
        .env("DONTSPEAK_PROVIDER", cfg.tts_provider_token())
        .env_remove("DONTSPEAK_STT_PROVIDER")
        .env_remove("DONTSPEAK_FULL_DUPLEX")
        .env_remove("DONTSPEAK_STT_PRELOAD")
        .env_remove("DONTSPEAK_TTS_PRELOAD");

    #[cfg(unix)]
    crate::system::set_new_pgroup(&mut cmd);

    // Windowless GUI host must not flash a console on cold fallback.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let child = cmd.spawn()?;
    // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
    // ds_proc::record_or_kill.
    let pgid = ds_proc::record_or_kill(&paths.pidfile, &child)?;
    Ok((child, pgid))
}
