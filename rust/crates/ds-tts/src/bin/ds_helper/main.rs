#![cfg_attr(windows, windows_subsystem = "windows")] // GUI subsystem: no console window (the engine pipes its stdio)
//! ds-helper — the thin native Kokoro synth + playback helper.
//!
//! Two modes:
//!   • one-shot:  `ds-helper <text> <voice> <rate>` — synth + play once, then
//!     exit. Spawned by `ds_tts::kokoro::spawn` (ds-speak / ds-narrate)
//!     in its own process group so the single-speaker pidfile/barge-in contract
//!     holds. The FALLBACK path when the engine is down. Replaces `uv run speak.py`.
//!   • server:    `ds-helper --serve` — load the model ONCE, then read JSON
//!     requests on stdin (one object per line) and synth+play each, so the engine
//!     (and the UI's voice auditioning) is fast after the first load — no per-reply
//!     model reload. This is the WARM path the engine supervises. A new
//!     request OR a `stop` CANCELS the one playing.
//!       Protocol (one JSON object per line):
//!         {"op":"speak","voice":"af_sarah","rate":1.5,"text":"…"}  → play `text`
//!         {"op":"stop"}                                            → cancel playback
//!       Replies: `READY` once loaded; exactly one `DONE` per speak (even
//!       if cancelled); `ERR <msg>` on a fatal load failure. `stop` is silent (it
//!       writes no line) so the response stream stays one-DONE-per-request. In
//!       full-duplex mode the user dictates OVER the reply (a concurrent `listen`
//!       thread, terminated by `LDONE`); stopping the voice is an explicit `stop`
//!       op / Caps long-press, not an implicit talk-over barge.
//!     Exits on stdin EOF.
//!
//! Fail-quiet: missing model/voices/onnxruntime (or no audio) → non-zero exit.
//! In `--serve`, macOS plays through ONE persistent `rodio` sink (per-request
//! `Player`s on a mixer opened once) so sentences are GAPLESS — no per-chunk
//! `afplay` launch. ort's C++ thread-pool AND cpal's CoreAudio backend abort on
//! teardown on macOS 26, so we exit via libc `_exit` to skip ALL destructors.

mod duplex;
mod listen;
mod oneshot;
mod serve;
mod setup;

unsafe extern "C" {
    pub(crate) fn _exit(code: i32) -> !;
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_default();

    if first == "--serve" {
        serve::serve(); // loops until stdin EOF, then _exit
    }

    if first == "--coexist-probe" {
        duplex::coexist_probe(); // dev check: VPIO + a separate cpal capture at once
    }

    if first == "--prefetch" {
        // Installer hook: download model assets and/or the Windows CUDA runtime via
        // ds-model — the SINGLE source of the pinned URLs/SHAs, so the installer never
        // hardcodes or drifts from them. `what` = "models" | "cuda" | "all".
        let what = args.next().unwrap_or_else(|| "all".to_string());
        unsafe { _exit(setup::run_prefetch(&what)) };
    }

    if first == "--print-manifest" {
        // Installer hook: write a component's STILL-NEEDED download list to a file
        // (GUI subsystem ⇒ no stdout for Inno to read). Lines: `url|basename|sha`.
        // URLs come from ds-model, so the installer never hardcodes them.
        let what = args.next().unwrap_or_else(|| "all".to_string());
        let out = args.next().unwrap_or_default();
        let body = ds_model::prefetch_items(&what)
            .into_iter()
            .map(|i| format!("{}|{}|{}", i.url, i.basename, i.sha256))
            .collect::<Vec<_>>()
            .join("\n");
        let code = match std::fs::write(&out, body) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("ds-helper: print-manifest '{what}' failed: {e}");
                1
            }
        };
        unsafe { _exit(code) };
    }

    if first == "--install-prefetched" {
        // Installer hook: verify + place/extract a component from a dir the installer
        // already downloaded into (no network). `<dir> <what>`. Reuses the normal
        // ensure_* paths via the prefetch source set below.
        let dir = args.next().unwrap_or_default();
        let what = args.next().unwrap_or_else(|| "all".to_string());
        ds_model::set_prefetch_source(Some(std::path::PathBuf::from(dir)));
        unsafe { _exit(setup::run_prefetch(&what)) };
    }

    // One-shot mode: `first` is the text.
    let text = first;
    let voice = {
        let v = args.next().unwrap_or_default();
        if v.trim().is_empty() {
            "af_sarah".to_string()
        } else {
            v
        }
    };
    let rate: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0_f32);

    if text.trim().is_empty() {
        unsafe { _exit(0) };
    }

    let code = match oneshot::run(&text, &voice, rate) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ds-helper: {e}");
            let _ = std::io::Write::flush(&mut std::io::stderr());
            1
        }
    };
    unsafe { _exit(code) };
}
