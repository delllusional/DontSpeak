//! Unsupported platforms: `open()` always fails → caller degrades to half-duplex.
//! Method surface mirrors macOS [`DuplexAudio`] so call sites compile cross-platform.

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

    pub fn render_buffered(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    pub fn render_clear(&self) {}

    pub fn set_muted(&self, _on: bool) {}

    pub fn barge_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    pub fn capture_handle(&self) -> CaptureHandle {
        CaptureHandle { _private: () }
    }

    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle { _private: () }
    }
}

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

#[derive(Clone)]
pub struct RenderHandle {
    _private: (),
}

impl RenderHandle {
    pub fn push(&self, _pcm_24k: &[f32]) {}
    pub fn buffered(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
    pub fn set_muted(&self, _on: bool) {}
}
