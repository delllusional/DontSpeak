//! Fake `ds-helper --serve` for `tts::wedge_recovery_tests`. Not packaged; built always
//! (Cargo can't test-only `[[bin]]`). Never production-spawned.
//!
//! READY unless `DONTSPEAK_FAKE_WEDGE_PRE_READY` (pre-READY hang — #59 handshake kill).
//! `listen` → `PARTIAL wedge-ack` then hang; `speak` → DONE (or exit after
//! `DONTSPEAK_FAKE_CLOSE_ON_SPEAK_MS`); `load tts` → TTSLOADED. Parse `op` via JSON
//! (not substring). EOF exits.

use std::io::{BufRead, Write};

fn main() {
    if std::env::var_os("DONTSPEAK_FAKE_WEDGE_PRE_READY").is_some() {
        // Pre-READY wedge (see module doc): alive, silent, stdout open.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let close_on_speak_ms = std::env::var("DONTSPEAK_FAKE_CLOSE_ON_SPEAK_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let _ = writeln!(out, "{}", ds_helper_proto::READY);
    let _ = out.flush();

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let request = serde_json::from_str::<serde_json::Value>(&line).ok();
        let op = request
            .as_ref()
            .and_then(|v| v.get("op"))
            .and_then(|op| op.as_str());
        match op {
            Some("listen") => {
                let _ = writeln!(out, "{}wedge-ack", ds_helper_proto::PARTIAL_PREFIX);
                let _ = out.flush();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            Some("speak") => {
                if let Some(delay_ms) = close_on_speak_ms {
                    let _ = writeln!(out, "{}1", ds_helper_proto::PROGRESS_PREFIX);
                    let _ = out.flush();
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    return;
                }
                let _ = writeln!(out, "{}2", ds_helper_proto::PROGRESS_PREFIX);
                let _ = writeln!(
                    out,
                    "{}synth_ms=1 audio_ms=1 first_ms=1",
                    ds_helper_proto::STATS_PREFIX
                );
                let _ = writeln!(out, "{}", ds_helper_proto::DONE);
                let _ = out.flush();
            }
            Some("load")
                if request
                    .as_ref()
                    .and_then(|v| v.get("engine"))
                    .and_then(|v| v.as_str())
                    == Some("tts") =>
            {
                let _ = writeln!(out, "{}", ds_helper_proto::TTSLOADED);
                let _ = out.flush();
            }
            _ => {} // other fire-and-forget ops and malformed lines are ignored
        }
    }
}
