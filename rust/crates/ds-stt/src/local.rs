//! Local on-device STT selector — one type for the warm helper's chosen `stt_engine`.
//! Parakeet over `ort`; on macOS also MLX Audio / Fluid / System.

use std::path::PathBuf;

use crate::parakeet::ParakeetTranscriber;

/// Active local transcriber. Same lazy-load surface as [`ParakeetTranscriber`].
pub enum LocalTranscriber {
    /// Parakeet over `ort` (`cpu`/`cuda`). Boxed vs smaller native arms (`large_enum_variant`).
    ParakeetOnnx(Box<ParakeetTranscriber>),
    /// macOS MLX Audio (`mlx`).
    #[cfg(target_os = "macos")]
    Mlx(crate::mlx::MlxTranscriber),
    /// FluidAudio Core ML / ANE (`fluid`); aarch64 only — no Intel dylib (#211).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Fluid(crate::fluid::FluidTranscriber),
    /// `SFSpeechRecognizer` (`system`) — separate engine, not a Parakeet runtime.
    #[cfg(target_os = "macos")]
    System(crate::sysspeech::SystemTranscriber),
}

/// Pure shim-vs-fallthrough selection (dlopen-free; unit-testable without ANE).
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Onnx,
    Mlx,
    /// aarch64 only (#211).
    #[cfg(target_arch = "aarch64")]
    Fluid,
    System,
}

/// `"system"` / `"fluid"`(+dylib, aarch64) / `"mlx"`(+dylib) → native; else ONNX.
/// `mlx`/`fluid` are separate dylib probes (#241).
#[cfg(target_os = "macos")]
fn select_backend(provider: &str, mlx: bool, fluid: bool) -> Backend {
    if provider.eq_ignore_ascii_case("system") {
        return Backend::System;
    }
    // aarch64-only Fluid arm: Intel has no dylib; fall through to ONNX (#211).
    #[cfg(target_arch = "aarch64")]
    if provider.eq_ignore_ascii_case("fluid") && fluid {
        return Backend::Fluid;
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = fluid;
    if provider.eq_ignore_ascii_case("mlx") && mlx {
        return Backend::Mlx;
    }
    Backend::Onnx
}

impl LocalTranscriber {
    /// Backend for the resolved provider token; shim-aware for an honest status row.
    pub fn for_provider(provider: &str, parakeet_dir: PathBuf) -> Self {
        #[cfg(target_os = "macos")]
        {
            use ds_model::shim::{Shim, available};
            match select_backend(provider, available(Shim::Mlx), available(Shim::Fluid)) {
                Backend::System => {
                    return LocalTranscriber::System(crate::sysspeech::SystemTranscriber::new());
                }
                #[cfg(target_arch = "aarch64")]
                Backend::Fluid => {
                    return LocalTranscriber::Fluid(crate::fluid::FluidTranscriber::new());
                }
                Backend::Mlx => {
                    return LocalTranscriber::Mlx(crate::mlx::MlxTranscriber::new());
                }
                Backend::Onnx => {}
            }
        }
        let _ = provider;
        LocalTranscriber::ParakeetOnnx(Box::new(ParakeetTranscriber::for_provider(
            parakeet_dir,
            provider,
        )))
    }

    /// Realized runtime in the shared Kokoro `PROVIDER` token vocabulary.
    pub fn provider(&self) -> ds_config::RealizedProvider {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.provider(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(_) => ds_config::RealizedProvider::Mlx,
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(_) => ds_config::RealizedProvider::Fluid,
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => ds_config::RealizedProvider::System,
        }
    }

    pub fn preload(&mut self) -> Result<(), String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(c) => c.preload(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.preload(),
        }?;
        // Graph warm (ONNX/MLX/Core ML compile). System has no graph. Best-effort.
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
            LocalTranscriber::Mlx(c) => c.unload(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.unload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.unload(),
        }
    }

    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(c) => c.transcribe_pcm_16k(pcm),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.transcribe_pcm_16k(pcm),
        }
    }
}

/// ~0.5s @ 16 kHz quiet 440 Hz — non-silence so warmup runs a real forward pass.
fn warmup_audio() -> Vec<f32> {
    use std::f32::consts::PI;
    (0..8_000)
        .map(|i| 0.02 * (i as f32 * 2.0 * PI * 440.0 / 16_000.0).sin())
        .collect()
}

#[cfg(target_os = "macos")]
#[cfg(test)]
mod tests {
    use super::{Backend, select_backend};

    /// `fluid` → Core ML only with aarch64 + dylib; else ONNX. Pure selection, no dlopen.
    #[test]
    fn for_provider_selects_the_fluid_backend_only_with_the_shim() {
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(select_backend("fluid", true, true), Backend::Fluid);
            assert_eq!(select_backend("FLUID", true, true), Backend::Fluid);
            assert_eq!(select_backend("fluid", true, false), Backend::Onnx);
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            // #211: Intel never builds the Fluid dylib — always ONNX.
            assert_eq!(select_backend("fluid", true, true), Backend::Onnx);
            assert_eq!(select_backend("fluid", true, false), Backend::Onnx);
        }
        assert_eq!(select_backend("mlx", true, true), Backend::Mlx);
        assert_eq!(select_backend("mlx", false, true), Backend::Onnx);
        assert_eq!(select_backend("system", false, false), Backend::System);
        assert_eq!(select_backend("cpu", true, true), Backend::Onnx);
    }

    /// #241: per-family dylib probes must not share one bool (Fluid-without-MLX trap).
    #[test]
    fn each_native_rung_reads_only_its_own_dylib() {
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(select_backend("fluid", false, true), Backend::Fluid);
            assert_eq!(select_backend("fluid", true, false), Backend::Onnx);
            assert_eq!(select_backend("mlx", false, true), Backend::Onnx);
            assert_eq!(select_backend("mlx", true, false), Backend::Mlx);
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            assert_eq!(select_backend("fluid", false, true), Backend::Onnx);
            assert_eq!(select_backend("mlx", true, false), Backend::Mlx);
        }
        assert_eq!(select_backend("system", false, false), Backend::System);
    }
}
