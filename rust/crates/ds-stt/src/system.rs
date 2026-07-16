//! System STT — Apple's on-device speech recognition on macOS. Windows/Linux: issue #75.
//! Live recognition runs in the warm helper (`crate::sysspeech::SystemTranscriber`);
//! this `SystemStt` is the INERT in-process placeholder the factory returns when the
//! helper path is unavailable.
//!
//! Deliberately inert (never grabs Caps, never injects) so selecting `stt_engine=system`
//! when it can't run does NOT silently fall back to Claude-native — surfaces "unavailable".

use crate::Stt;

/// Inert in-process System STT placeholder (live path is the warm helper).
#[derive(Default)]
pub struct SystemStt;

impl SystemStt {
    pub fn new() -> Self {
        SystemStt
    }

    /// Usable right now? Real probe on macOS (no prompt); false elsewhere.
    pub fn available() -> bool {
        crate::system_available()
    }
}

impl Stt for SystemStt {
    fn start(&mut self) -> bool {
        // Inert: live recognizer is in the warm helper. false ⇒ Caps does nothing here
        // (no silent claude_native fallback).
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
