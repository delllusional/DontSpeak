#![cfg_attr(windows, windows_subsystem = "windows")] // GUI subsystem: no console window (the engine pipes its stdio)
//! Kokoro synth + playback helper.
//!
//! * one-shot: `ds-helper <text> <voice> <rate>` — own process group (pidfile/barge);
//!   fallback when engine down.
//! * `--serve`: load once; NDJSON ops on stdin. `speak` / `stop` (silent); replies
//!   `READY`, `DONE`, `ERR`. Full-duplex listen ends with `LDONE`. Exit on stdin EOF.
//!
//! Fail-quiet if assets/audio missing. macOS: one rodio mixer (gapless). Exit via
//! `_exit` — ort/cpal abort on Drop (macOS 26).

mod duplex;
mod listen;
mod oneshot;
mod prepare;
mod priority;
mod serve;
mod setup;
mod stt_residency;

unsafe extern "C" {
    pub(crate) fn _exit(code: i32) -> !;
}

/// Mirrors the engine's `DONTSPEAK_DEBUG` gate so helper `log::debug!` stays off by default.
/// Fed into `log::set_max_level` once at startup (skips format_args when off).
pub(crate) fn debug_enabled() -> bool {
    std::env::var("DONTSPEAK_DEBUG").as_deref() == Ok("1")
}

fn main() {
    ds_log::init();
    if debug_enabled() {
        log::set_max_level(log::LevelFilter::Debug);
    }

    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_default();

    if first == "--serve" {
        priority::elevate_process();
        priority::elevate_current_thread();
        serve::serve(); // loops until stdin EOF, then _exit
    }

    if first == "--coexist-probe" {
        duplex::coexist_probe(); // dev check: VPIO + a separate cpal capture at once
    }

    if first == "--prefetch" {
        // Installer hook via ds-model (single source of pinned URLs/SHAs).
        // `what` default "all" (not a DownloadTarget) ⇒ models + CUDA in run_prefetch.
        let what = args.next().unwrap_or_else(|| "all".to_string());
        // SAFETY: deliberate `_exit` teardown (ort/cpal abort on Drop); see crate doc.
        unsafe { _exit(setup::run_prefetch(&what)) };
    }

    if first == "--print-manifest" {
        // Still-needed downloads to a file (GUI subsystem has no usable stdout).
        // Lines: `url|file_name|sha` from ds-model.
        let what = args.next().unwrap_or_else(|| "all".to_string());
        let out = args.next().unwrap_or_default();
        // Sole wire-token parse for this hook; unknown (incl. "all") ⇒ empty manifest.
        let items = ds_model::DownloadTarget::parse(&what)
            .map(ds_model::prefetch_items)
            .unwrap_or_default();
        let body = items
            .into_iter()
            .map(|i| format!("{}|{}|{}", i.url, i.file_name, i.sha256))
            .collect::<Vec<_>>()
            .join("\n");
        let code = match std::fs::write(&out, body) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("ds-helper: print-manifest '{what}' failed: {e}");
                1
            }
        };
        // SAFETY: deliberate `_exit` teardown; see crate doc.
        unsafe { _exit(code) };
    }

    if first == "--install-prefetched" {
        // Place/extract from an already-downloaded dir (no network). `<dir> <what>`.
        let dir = args.next().unwrap_or_default();
        let what = args.next().unwrap_or_else(|| "all".to_string());
        ds_model::set_prefetch_source(Some(std::path::PathBuf::from(dir)));
        // SAFETY: deliberate `_exit` teardown; see crate doc.
        unsafe { _exit(setup::run_prefetch(&what)) };
    }

    // One-shot mode: `first` is the text.
    let text = first;
    // Voice is required — there is no fallback voice; the engine always passes the
    // caller's assigned pool voice.
    let voice = args.next().unwrap_or_default();
    if voice.trim().is_empty() {
        eprintln!("usage: ds-helper <text> <voice-id> [rate]");
        // SAFETY: deliberate `_exit` teardown; see crate doc.
        unsafe { _exit(2) };
    }
    let rate: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0_f32);

    if text.trim().is_empty() {
        // SAFETY: deliberate `_exit` teardown; see crate doc.
        unsafe { _exit(0) };
    }

    priority::elevate_process();
    priority::elevate_current_thread();
    let code = match oneshot::run(&text, &voice, rate) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ds-helper: {e}");
            let _ = std::io::Write::flush(&mut std::io::stderr());
            1
        }
    };
    // SAFETY: deliberate `_exit` teardown; see crate doc.
    unsafe { _exit(code) };
}
