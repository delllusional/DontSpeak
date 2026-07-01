//! ds-aec — acoustic-echo-cancelled duplex audio (see docs/AEC.md).
//!
//! One platform unit owns BOTH the speaker render and the mic capture, so the OS
//! can subtract the played-back TTS (the far-end reference) from the captured mic
//! signal — letting STT keep listening *while* TTS speaks (full-duplex) instead
//! of the strict half-duplex gate (mic closed during TTS) we fall back to.
//!
//!   * macOS — a single `kAudioUnitSubType_VoiceProcessingIO` AudioUnit. Apple's
//!     built-in voice processing does the AEC; it owns both streams so there is
//!     no delay/clock-drift alignment to do ourselves.
//!   * Windows — a WASAPI capture client opened in the "Communications" category,
//!     which engages the OS capture-side AEC APO. Capture-side only: rodio keeps
//!     rendering TTS (`owns_render() == false`); the OS taps the render endpoint as
//!     the echo reference itself.
//!   * Linux — a PulseAudio/PipeWire `module-echo-cancel` cancelled source, opened by
//!     name through the Pulse simple API (works on both servers). Capture-side only,
//!     like Windows (`owns_render() == false`); rodio keeps rendering and the server's
//!     WebRTC canceller references the render endpoint. (A future in-process WebRTC APM
//!     backend — `owns_render() == true` — is sketched in docs/FULL-DUPLEX-PORT.md §6A.)
//!   * other — an unsupported stub; the caller degrades to the half-duplex path.
//!
//! [`DuplexAudio`] is `!Send` on macOS (the `AudioUnit`, like the cpal capture
//! stream, is `!Send`): open and consume it on ONE thread (the helper's playback
//! thread). Its render/input callbacks run on the CoreAudio realtime thread and
//! talk to that thread through lock-free SPSC rings.

#[cfg(target_os = "macos")]
mod resample;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{CaptureHandle, DuplexAudio};

// The Windows + Linux backends share an identical `Mutex<VecDeque<f32>>`-backed
// `CaptureHandle` and overflow-trim (macOS uses ringbuf, above) — both live in `shared`.
#[cfg(any(windows, target_os = "linux"))]
mod shared;
#[cfg(any(windows, target_os = "linux"))]
pub use shared::CaptureHandle;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::DuplexAudio;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::DuplexAudio;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod stub;
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub use stub::{CaptureHandle, DuplexAudio};
