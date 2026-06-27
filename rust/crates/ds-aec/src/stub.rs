//! Non-macOS stub: no native duplex AEC unit yet (Windows/Linux land later — see
//! docs/AEC.md §6/§7). `open()` always fails so the caller degrades to the half-duplex
//! cpal + rodio path. The method surface mirrors the macOS [`DuplexAudio`] so call
//! sites compile cross-platform.

pub struct DuplexAudio {
    _private: (),
}

impl DuplexAudio {
    pub fn open() -> Result<Self, String> {
        Err("duplex AEC not supported on this platform yet".into())
    }

    pub fn capture_rate(&self) -> u32 {
        0
    }

    /// No backend → never owns render (moot; `open()` always fails here).
    pub fn owns_render(&self) -> bool {
        false
    }

    pub fn render_push(&self, _pcm_24k: &[f32]) {}

    pub fn capture_drain(&self) -> Vec<f32> {
        Vec::new()
    }

    pub fn render_pending(&self) -> bool {
        false
    }

    pub fn render_clear(&self) {}

    pub fn barge_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle { _private: () }
    }
}

/// Non-macOS stub capture handle (no VPIO unit exists).
#[derive(Clone)]
pub struct CaptureHandle {
    _private: (),
}

impl CaptureHandle {
    pub fn capture_rate(&self) -> u32 {
        0
    }
    pub fn drain(&self) -> Vec<f32> {
        Vec::new()
    }
}
