//! Headless `dontspeakd` binary — a thin shim over the engine library. All the
//! logic (caps loop, RPC server, TTS queue, hot-reload) lives in `lib.rs` so the
//! SAME engine can be hosted IN-PROCESS by a native app via the C ABI (one TCC
//! grant on the app), or run standalone here for headless / Linux / CLI use.
// Windows: GUI subsystem so a GUI host spawning the headless engine shows no console.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    dontspeakd::run_headless();
}
