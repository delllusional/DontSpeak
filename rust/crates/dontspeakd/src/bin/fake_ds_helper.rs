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
//! Protocol: emits READY immediately, then per stdin line:
//!   * a `listen` request emits one `PARTIAL wedge-ack` line FIRST — a real, recognized
//!     `ds-helper-proto` token that `listen_cancellable`'s own event loop demuxes and
//!     hands to the caller's `on_partial` callback, which is how the test proves the
//!     request was actually received (not a made-up out-of-band marker) — THEN wedges:
//!     never reads or responds to stdin again, mirroring a real (single-worker-thread)
//!     `ds-helper` blocked inside one hung native call.
//!   * a `speak` request replies DONE immediately.
//!   * anything else (`lstop`/`stop`/`mute`/`load`/`unload`/…) is silently ignored, same
//!     as the real protocol's fire-and-forget ops.
//!
//! EOF on stdin exits cleanly. Parses just the `op` field via `serde_json` (the crate
//! already depends on it for this exact protocol) rather than substring-matching the raw
//! line — a substring match would misfire on any request whose `text` field happens to
//! contain the literal word "listen" or "speak".

use std::io::{BufRead, Write};

fn main() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", ds_helper_proto::READY);
    let _ = out.flush();

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let op = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|v| v.get("op").and_then(|op| op.as_str()).map(str::to_string));
        match op.as_deref() {
            Some("listen") => {
                let _ = writeln!(out, "{}wedge-ack", ds_helper_proto::PARTIAL_PREFIX);
                let _ = out.flush();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            Some("speak") => {
                let _ = writeln!(out, "{}", ds_helper_proto::DONE);
                let _ = out.flush();
            }
            _ => {} // fire-and-forget ops (`lstop`/`stop`/`mute`/`load`/`unload`/…) and malformed lines: ignored
        }
    }
}
