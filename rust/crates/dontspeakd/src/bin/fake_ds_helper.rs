//! `dontspeakd-fake-helper` — a fake `ds-helper --serve` process, built as an ordinary
//! artifact of this crate (like `ds-aec`'s `ds-aec-probe`: Cargo has no way to scope a
//! `[[bin]]` to `cargo test` only, so this exists in every `cargo build`/`cargo test` of
//! `dontspeakd`, debug and release alike — it is NOT test-gated). Never spawned by
//! production code and never packaged/shipped (every packaging/install script names the
//! real `ds-helper` literally). Exists solely so `tts::wedge_recovery_tests` (issue #34,
//! item 2) has a REAL child process that speaks just enough of the `ds-helper-proto` wire
//! to simulate a genuinely hung native STT finalize call, without the real `ds-helper`'s
//! model/mic/audio dependencies.
//!
//! Protocol: emits READY immediately (unless `DONTSPEAK_FAKE_WEDGE_PRE_READY` is set in
//! the env — then it wedges FIRST: alive, silent, stdout open, never printing READY/ERR
//! or closing the pipe, mirroring a real helper stuck pre-READY in ORT provider init or
//! behind an AV scan of the model file — the issue #59 shape the READY-handshake bound
//! in `tts::start_locked` exists to kill), then per stdin line:
//!   * a `listen` request emits one `PARTIAL wedge-ack` line FIRST — a real, recognized
//!     `ds-helper-proto` token that `listen_cancellable`'s own event loop demuxes and
//!     hands to the caller's `on_partial` callback, which is how the test proves the
//!     request was actually received (not a made-up out-of-band marker) — THEN wedges:
//!     never reads or responds to stdin again, mirroring a real (single-worker-thread)
//!     `ds-helper` blocked inside one hung native call.
//!   * a `speak` request reports progress/stats and replies DONE immediately. When the
//!     first-spawn-only `DONTSPEAK_FAKE_CLOSE_ON_SPEAK_MS` is set, it instead reports one
//!     progress batch, waits that many milliseconds, and exits.
//!   * `load tts` reports TTSLOADED; other fire-and-forget ops are silently ignored, same
//!     as the real protocol's fire-and-forget ops.
//!
//! EOF on stdin exits cleanly. Parses just the `op` field via `serde_json` (the crate
//! already depends on it for this exact protocol) rather than substring-matching the raw
//! line — a substring match would misfire on any request whose `text` field happens to
//! contain the literal word "listen" or "speak".

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
