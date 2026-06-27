//! KokoroTts — the DEFAULT TTS engine. Native, in-process Kokoro synthesis
//! (ort + voice-g2p + rodio); NO Python, NO uv, NO speak.py.
//!
//! The actual synth + audio playback runs in the thin `ds-helper` HELPER
//! BIN (see `src/bin/ds_helper.rs`), which this module spawns in the
//! child's OWN process group (`setsid`). That preserves the SACRED
//! single-speaker pidfile contract unchanged: [`spawn`] still returns
//! `(Child, pgid)` and records the pgid in the shared pidfile, so the engine's
//! caps-ON barge-in (`killpg`) and ds-narrate's pidfile-takeover watch loop
//! keep working exactly as before. Only the spawned COMMAND changed — from
//! `uv run ~/kokoro-tts/speak.py <txt> <voice>` to `ds-helper <txt> <voice>
//! <rate>` (native, in its own process group). The helper:
//!   1. lazily ensures the kokoro model + voices + onnxruntime dylib (ds-model),
//!   2. voice-g2p (g2p.rs) → vocab tokenize → batch phonemes at clause marks
//!      (batch.rs `stream_batches`),
//!   3. per batch: ort synth (synth.rs) → trim → rodio streaming playback
//!      (play.rs), synth-N+1-while-N-plays.
//!
//! Degrade-fail-quiet: if the model/voices/onnxruntime aren't present (or audio
//! can't open), the helper exits non-zero and the hook logs it — exactly like
//! the STT "no model" path. Nothing here ever execs uv or speak.py.

use std::process::{Child, Command, Stdio};

use ds_config::Paths;

use crate::{SpeakHandle, Tts};

/// The Kokoro TTS engine (native in-process synth via the ds-helper helper).
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
        let voice = voice_id.unwrap_or("af_sarah");
        let (child, pgid) = spawn(&self.paths, text, voice, rate)?;
        // Fire-and-forget for the trait path: the caller waits by pgid (or via
        // the pidfile) and clears the pidfile. We must NOT block here (speak must
        // return a handle), so we drop the Child handle; the pgid in the pidfile
        // keeps the kill contract.
        drop(child);
        Ok(SpeakHandle { pgid })
    }

    fn kind(&self) -> &'static str {
        "kokoro"
    }
}

/// Locate the `ds-helper` helper binary. Prefer a sibling of the current
/// executable (the normal install + cargo target layout), then fall back to a
/// bare name resolved via PATH. Returns the command string to spawn.
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

/// Spawn the native `ds-helper <txt> <voice> <rate>` helper in the child's
/// OWN process group (`setsid` on unix so pgid == pid), record the pgid in the
/// shared pidfile, and return both the live `Child` (for the caller's wait /
/// takeover loop) and the pgid. Shared by [`KokoroTts::speak`], `ds-speak`,
/// and `ds-narrate` so the spawn logic lives in exactly one place.
///
/// Replaces the Phase-1 `uv run ~/kokoro-tts/speak.py` shell-out: no uv, no
/// speak.py, no `~/kokoro-tts` guard. The helper itself fail-quiets if the model
/// or onnxruntime aren't available yet.
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

    // Windows: don't pop a console window when a windowless GUI host spawns this
    // console exe (the cold fallback path). Stdio is null'd, so nothing is lost.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let child = cmd.spawn()?;
    // SACRED single-speaker post-spawn contract (ARCHITECTURE §0.2) — see
    // ds_proc::record_or_kill.
    let pid = ds_proc::record_or_kill(&paths.pidfile, &child)?;
    Ok((child, pid))
}
