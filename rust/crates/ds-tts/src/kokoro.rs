//! KokoroTts — DEFAULT TTS. Native Kokoro via `ds-helper` (no Python/uv/speak.py).
//!
//! Spawns the helper in its OWN process group (`setsid`), preserving the SACRED
//! single-speaker pidfile contract: [`spawn`] returns `(Child, pgid)` for barge-in
//! and narrate's pidfile-takeover watch. Helper: ensure models → G2P → batch →
//! synth → trim → play. Fail-quiet if models/audio missing (like STT "no model").

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
        let voice = voice_id.unwrap_or(ds_config::DEFAULT_KOKORO_VOICE);
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
    let mut cmd = Command::new(helper_command());
    cmd.arg(txt)
        .arg(voice)
        .arg(format!("{rate}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

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
