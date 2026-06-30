//! Local on-device STT selector. Wraps the local transcribers behind one type so
//! the warm helper can hold whichever the user selected (`stt_engine`) without caring
//! which it is. Parakeet-ONNX (cross-platform streaming FastConformer over `ort`) and — on
//! macOS — Parakeet Core ML / ANE (FluidAudio). Both are SELECTABLE; neither replaces the other.

use std::path::PathBuf;

use crate::parakeet::ParakeetTranscriber;

/// Is the FluidAudio Core ML / ANE shim usable (the app sets `SMKOKORO_DYLIB_PATH` to the
/// bundled dylib)? Gates the `ane` STT backend so a missing shim falls back to ONNX. The
/// engine checks the SAME env (`apple_native_shim_available`), and the helper inherits it,
/// so engine + helper agree on the actual runtime — keeping the status row honest.
#[cfg(target_os = "macos")]
fn shim_available() -> bool {
    std::env::var_os("SMKOKORO_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).exists())
        .unwrap_or(false)
}

/// The active local transcriber. Same lazy-load surface as
/// [`ParakeetTranscriber`] so call sites are backend-agnostic.
pub enum LocalTranscriber {
    /// Cross-platform Parakeet — the streaming FastConformer over `ort` (the `cpu` provider).
    /// Boxed: `ParakeetTranscriber` is much larger than the Core ML variant, so storing
    /// it inline made every `LocalTranscriber` as big as it (clippy `large_enum_variant`).
    /// Box keeps the enum small; the lazy-load surface is unchanged because the box
    /// derefs to the same methods.
    ParakeetOnnx(Box<ParakeetTranscriber>),
    /// macOS-only Parakeet over FluidAudio Core ML / ANE (the `parakeet` engine).
    #[cfg(target_os = "macos")]
    Coreml(crate::coreml::CoremlTranscriber),
    /// macOS-only on-device `SFSpeechRecognizer` (the `system` engine). A DIFFERENT
    /// engine, not a Parakeet runtime — selected by the `"system"` provider token.
    #[cfg(target_os = "macos")]
    System(crate::sysspeech::SystemTranscriber),
}

impl LocalTranscriber {
    /// Pick the backend by RESOLVED provider token. `"system"` → macOS
    /// `SFSpeechRecognizer` (the `system` ENGINE, not a Parakeet runtime); `"ane"` →
    /// FluidAudio Core ML / ANE (macOS only), but only when the shim is actually present,
    /// else it falls back to the ONNX Parakeet so a missing shim degrades gracefully;
    /// anything else (incl. `"cpu"`) → the portable ONNX Parakeet. The engine reports
    /// the same shim-aware runtime, so the status row stays honest.
    pub fn for_provider(provider: &str, parakeet_dir: PathBuf) -> Self {
        #[cfg(target_os = "macos")]
        if provider.eq_ignore_ascii_case("system") {
            return LocalTranscriber::System(crate::sysspeech::SystemTranscriber::new());
        }
        #[cfg(target_os = "macos")]
        if provider.eq_ignore_ascii_case("ane") && shim_available() {
            return LocalTranscriber::Coreml(crate::coreml::CoremlTranscriber::new());
        }
        let _ = provider;
        LocalTranscriber::ParakeetOnnx(Box::new(ParakeetTranscriber::new(parakeet_dir)))
    }

    pub fn preload(&mut self) -> Result<(), String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Coreml(c) => c.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.preload(),
        }?;
        // WARM the inference graph: the FIRST transcribe is what compiles the ONNX / Core
        // ML-ANE graph, so run one throwaway pass NOW (at eager preload) — moving that
        // one-time cost off the user's first dictation. SKIP for System: SFSpeechRecognizer
        // has no graph to warm and rejects synthetic input. Best-effort: a warmup hiccup
        // must not fail an otherwise-successful load.
        let skip_warmup = match self {
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => true,
            _ => false,
        };
        if !skip_warmup {
            let _ = self.transcribe_pcm_16k(&warmup_audio());
        }
        Ok(())
    }

    pub fn unload(&mut self) -> bool {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.unload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Coreml(c) => c.unload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.unload(),
        }
    }

    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Coreml(c) => c.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.transcribe_pcm_16k(pcm),
        }
    }
}

/// ~0.5s @ 16 kHz of a quiet 440 Hz tone — the warmup input for [`LocalTranscriber::preload`].
/// NON-degenerate "audio" so the recognizer accepts it (pure silence is rejected as
/// invalidAudioData by FluidAudio) and actually runs a forward pass, compiling/warming the
/// inference graph. The transcript is discarded.
fn warmup_audio() -> Vec<f32> {
    use std::f32::consts::PI;
    (0..8_000)
        .map(|i| 0.02 * (i as f32 * 2.0 * PI * 440.0 / 16_000.0).sin())
        .collect()
}
