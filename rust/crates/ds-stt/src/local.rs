//! Local on-device STT selector. One type so the warm helper holds whichever
//! `stt_engine` was selected. Parakeet-ONNX (streaming FastConformer over `ort`) and —
//! on macOS — Parakeet Core ML / ANE (FluidAudio). Both SELECTABLE; neither replaces the other.

use std::path::PathBuf;

use crate::parakeet::ParakeetTranscriber;

/// FluidAudio Core ML / ANE shim present (`SMKOKORO_DYLIB_PATH`)? Gates `ane` so a
/// missing shim falls back to ONNX. Engine checks the SAME env
/// (`apple_native_shim_available`); helper inherits it so status row stays honest.
#[cfg(target_os = "macos")]
fn shim_available() -> bool {
    std::env::var_os("SMKOKORO_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).exists())
        .unwrap_or(false)
}

/// Active local transcriber. Same lazy-load surface as [`ParakeetTranscriber`].
pub enum LocalTranscriber {
    /// Streaming FastConformer over `ort` (`cpu`/`cuda` provider). Boxed: avoids clippy
    /// `large_enum_variant` vs the smaller Core ML arm.
    ParakeetOnnx(Box<ParakeetTranscriber>),
    /// macOS FluidAudio Core ML / ANE (`parakeet` engine).
    #[cfg(target_os = "macos")]
    Coreml(crate::coreml::CoremlTranscriber),
    /// macOS `SFSpeechRecognizer` (`system` engine) — DIFFERENT engine, not a Parakeet
    /// runtime; selected by the `"system"` provider token.
    #[cfg(target_os = "macos")]
    System(crate::sysspeech::SystemTranscriber),
}

impl LocalTranscriber {
    /// Backend by RESOLVED provider token. `"system"` → `SFSpeechRecognizer`;
    /// `"ane"` → Core ML when shim present, else ONNX fallback; anything else
    /// (incl. `"cpu"`) → portable ONNX Parakeet. Shim-aware so status row stays honest.
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
        LocalTranscriber::ParakeetOnnx(Box::new(ParakeetTranscriber::for_provider(
            parakeet_dir,
            provider,
        )))
    }

    /// Realized runtime in the SAME token vocabulary Kokoro TTS reports via `PROVIDER`
    /// — STT status maps through the ONE shared `realized_ort_token` path (no drift).
    pub fn provider(&self) -> ds_config::RealizedProvider {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.provider(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Coreml(_) => ds_config::RealizedProvider::CoreMlAne,
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => ds_config::RealizedProvider::System,
        }
    }

    pub fn preload(&mut self) -> Result<(), String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Coreml(c) => c.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.preload(),
        }?;
        // WARM the graph: first real transcribe compiles ONNX / Core ML — throwaway pass
        // now so first dictation doesn't pay it. SKIP System: no graph, rejects synthetic.
        // Best-effort: warmup hiccup must not fail an otherwise-successful load.
        let skip_warmup = match self {
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => true,
            _ => false,
        };
        if !skip_warmup {
            self.transcribe_pcm_16k(&warmup_audio())?;
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

/// ~0.5s @ 16 kHz quiet 440 Hz — warmup for [`LocalTranscriber::preload`].
/// NON-silence: FluidAudio rejects pure silence as invalidAudioData; we need a real
/// forward pass to compile the graph. Transcript discarded.
fn warmup_audio() -> Vec<f32> {
    use std::f32::consts::PI;
    (0..8_000)
        .map(|i| 0.02 * (i as f32 * 2.0 * PI * 440.0 / 16_000.0).sin())
        .collect()
}
