#![cfg_attr(windows, windows_subsystem = "windows")] // engine pipes stdio; no console
//! Built-in TTS/STT warm helper (registry model + Parakeet) and one-shot synth.
//!
//! - one-shot: `ds-helper <text> <voice> <rate>` — synth + play one utterance, then
//!   exit (manual/dev; the engine always spawns `--serve`)
//! - `--serve`: load once; NDJSON stdin; `READY`/`DONE`/`ERR`; listen ends `LDONE`
//!
//! Fail-quiet if assets/audio missing. Exit via `_exit` (ort/cpal abort on Drop).

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

/// Mirrors engine `DONTSPEAK_DEBUG` (max_level once at startup).
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

    if first == "--synth-check" {
        // Dev check: `--synth-check <model> <voice> <text>` — load + synth one phrase and
        // report amplitude (no audio device). Model from arg, else DONTSPEAK_TTS_MODEL.
        let model = ds_config::TtsModel::parse(&args.next().unwrap_or_default())
            .unwrap_or_else(oneshot::tts_model);
        let voice = args.next().unwrap_or_default();
        let text = args.next().unwrap_or_default();
        let code = match oneshot::synth_check(model, &text, &voice) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("FAIL {}: {error}", model.as_str());
                1
            }
        };
        // SAFETY: deliberate `_exit` teardown; see crate doc.
        unsafe { _exit(code) };
    }

    if first == "--prefetch" {
        // Installer: pinned URLs/SHAs via ds-model. Default "all" ⇒ models + CUDA.
        let what = args.next().unwrap_or_else(|| "all".to_string());
        // SAFETY: deliberate `_exit` (ort/cpal abort on Drop); see crate doc.
        unsafe { _exit(setup::run_prefetch(&what)) };
    }

    if first == "--print-manifest" {
        // GUI subsystem: write `url|file_name|sha` to a file (no usable stdout).
        let what = args.next().unwrap_or_else(|| "all".to_string());
        let out = args.next().unwrap_or_default();
        // Unknown token (incl. "all") ⇒ empty manifest.
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
