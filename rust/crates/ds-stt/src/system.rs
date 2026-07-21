//! System STT — Apple's on-device speech recognition on macOS. Windows/Linux: issue #75.
//! Live path is the warm helper (`sysspeech`); this is the inert in-process placeholder
//! when helper is unavailable. Surfaces "unavailable" (no silent claude_native fallback).

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
        false // helper owns live recognizer
    }
    fn stop(&mut self) {}
    fn kind(&self) -> &'static str {
        "system"
    }
}
