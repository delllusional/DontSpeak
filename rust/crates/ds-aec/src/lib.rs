//! Acoustic-echo-cancelled duplex audio (docs/AEC.md).
//!
//! One platform unit owns speaker + mic so the OS subtracts TTS (far-end) from the mic:
//! full-duplex STT while speaking (vs half-duplex with mic closed during TTS).
//!
//! - **macOS** — one `kAudioUnitSubType_VoiceProcessingIO` unit (AEC built-in; both streams).
//! - **Windows** — WASAPI Communications capture (capture-side AEC APO). `owns_render() == false`;
//!   rodio renders; OS taps the render endpoint as reference.
//! - **Linux** — PulseAudio/PipeWire `module-echo-cancel` source via Pulse simple API.
//!   Capture-side only like Windows. (In-process WebRTC APM is a future option in docs/AEC.md.)
//! - **other** — stub; caller degrades to half-duplex.
//!
//! macOS [`DuplexAudio`] is `!Send` (AudioUnit): open/consume on one helper thread.
//! RT callbacks talk via lock-free SPSC rings.

#[cfg(any(target_os = "macos", windows))]
mod resample;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{CaptureHandle, DuplexAudio, RenderHandle};

// Win+Linux share Mutex+VecDeque CaptureHandle + overflow trim (macOS uses ringbuf).
#[cfg(any(windows, target_os = "linux"))]
mod shared;
#[cfg(any(windows, target_os = "linux"))]
pub use shared::{CaptureHandle, RenderHandle};

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
pub use stub::{CaptureHandle, DuplexAudio, RenderHandle};
