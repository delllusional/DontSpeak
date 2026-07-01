//! System STT — Apple's on-device `SFSpeechRecognizer` on macOS (the deferred Windows
//! `Windows.Media.SpeechRecognition` / Linux paths remain TODO). The REAL recognition
//! runs through the warm helper (mic capture → `crate::sysspeech::SystemTranscriber`),
//! exactly like Parakeet; this `SystemStt` is the INERT in-process placeholder the
//! `ds-engines` factory returns for the helper-less / unavailable case.
//!
//! It is deliberately inert (never grabs Caps, never injects) so that selecting
//! `stt_engine=system` when it can't run does NOT silently fall back to Claude-native
//! dictation — the engine surfaces "unavailable" instead.

use crate::Stt;

/// Inert in-process System STT placeholder (the live path is the warm helper).
#[derive(Default)]
pub struct SystemStt;

impl SystemStt {
    pub fn new() -> Self {
        SystemStt
    }

    /// Is on-device system STT usable right now? Real probe on macOS (no prompt),
    /// false elsewhere. Used by the engine's availability gate.
    pub fn available() -> bool {
        crate::system_available()
    }
}

impl Stt for SystemStt {
    fn start(&mut self) -> bool {
        // Inert: the live recognizer runs in the warm helper, not here. Returning false
        // means a Caps press does nothing in this box (no silent claude_native fallback).
        false
    }
    fn stop(&mut self) {}
    fn is_available(&self) -> bool {
        Self::available()
    }
    fn kind(&self) -> &'static str {
        "system"
    }
}
