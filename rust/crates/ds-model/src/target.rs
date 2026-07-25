//! Canonical download / installer-prefetch targets.
//!
//! One enum every dispatcher matches so wire tokens cannot typo-fallthrough. Parse only
//! at the process boundary (CLI); in-process APIs take the enum. Naming: `<brand>_<flavor>`
//! for models, bare nouns for runtimes/groups. Platform gate:
//! [`is_supported_on_this_host`](DownloadTarget::is_supported_on_this_host).

use ds_config::host::{Arch, Os};

/// Download/prefetch target. [`as_str`](DownloadTarget::as_str) is the stable wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadTarget {
    /// Shared ORT dylib. Wire `"onnxruntime"` (not bare `"onnx"` — that means a model flavor).
    Onnxruntime,
    /// Full portable Kokoro + ORT. Wire `"kokoro_model"` (not brand `"kokoro"`).
    KokoroModel,
    /// OOV G2P + ORT (shared by both Kokoro backends). Wire `"kokoro_frontend"`.
    KokoroFrontend,
    /// MLX Kokoro set ([`crate::mlx_repo::KOKORO_MLX_SET`]). Apple-Silicon macOS.
    KokoroMlx,
    /// FluidAudio Core ML Kokoro set ([`crate::coreml_repo::KOKORO_COREML_SET`]).
    /// Apple-Silicon macOS.
    KokoroFluid,
    /// Full Parakeet streaming set + ORT. Wire `"parakeet_model"`.
    ParakeetModel,
    /// MLX Parakeet set ([`crate::mlx_repo::PARAKEET_MLX_SET`]). Apple-Silicon macOS.
    ParakeetMlx,
    /// FluidAudio Core ML Parakeet sets ([`crate::coreml_repo::PARAKEET_COREML_SET`]).
    /// Apple-Silicon macOS.
    ParakeetFluid,
    /// SepFormer speaker-lock + ORT. macOS. Wire `"sepformer_model"`.
    SepformerModel,
    /// Chatterbox Multilingual ONNX set + ORT.
    ChatterboxModel,
    /// Chatterbox Multilingual MLX set. Apple-Silicon macOS.
    ChatterboxMlx,
    /// Qwen3-TTS CustomVoice ONNX set + ORT.
    QwenModel,
    /// Qwen3-TTS CustomVoice MLX set. Apple-Silicon macOS.
    QwenMlx,
    /// OmniVoice ONNX set + ORT.
    OmniVoiceModel,
    /// OmniVoice MLX set. Apple-Silicon macOS.
    OmniVoiceMlx,
    /// CUDA EP wheels (~1.4 GB). x86_64 Windows/Linux only.
    Cuda,
    /// Speaker diarization via MLX (Apple-Silicon macOS). Wire `"diarization_mlx"`.
    DiarizationMlx,
    /// Speaker diarization via FluidAudio Core ML
    /// ([`crate::coreml_repo::DIARIZATION_COREML_SET`]). Apple-Silicon macOS.
    DiarizationFluid,
    /// Installer group: default Kokoro TTS + Parakeet STT ONNX models.
    Models,
}

impl DownloadTarget {
    const TTS_TARGETS: [(ds_config::TtsModel, Self, Self); 4] = [
        (
            ds_config::TtsModel::Kokoro,
            Self::KokoroModel,
            Self::KokoroMlx,
        ),
        (
            ds_config::TtsModel::Chatterbox,
            Self::ChatterboxModel,
            Self::ChatterboxMlx,
        ),
        (ds_config::TtsModel::Qwen, Self::QwenModel, Self::QwenMlx),
        (
            ds_config::TtsModel::OmniVoice,
            Self::OmniVoiceModel,
            Self::OmniVoiceMlx,
        ),
    ];

    /// FluidAudio Core ML is partial (Kokoro only), so not a fourth column of
    /// [`Self::TTS_TARGETS`] (every built-in TTS model must fill that table).
    const FLUID_TTS_TARGETS: [(ds_config::TtsModel, Self); 1] =
        [(ds_config::TtsModel::Kokoro, Self::KokoroFluid)];

    /// Stable wire token (IPC / CLI / installer).
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadTarget::Onnxruntime => "onnxruntime",
            DownloadTarget::KokoroModel => "kokoro_model",
            DownloadTarget::KokoroFrontend => "kokoro_frontend",
            DownloadTarget::KokoroMlx => "kokoro_mlx",
            DownloadTarget::KokoroFluid => "kokoro_fluid",
            DownloadTarget::ParakeetModel => "parakeet_model",
            DownloadTarget::ParakeetMlx => "parakeet_mlx",
            DownloadTarget::ParakeetFluid => "parakeet_fluid",
            DownloadTarget::SepformerModel => "sepformer_model",
            DownloadTarget::ChatterboxModel => "chatterbox_model",
            DownloadTarget::ChatterboxMlx => "chatterbox_mlx",
            DownloadTarget::QwenModel => "qwen_model",
            DownloadTarget::QwenMlx => "qwen_mlx",
            DownloadTarget::OmniVoiceModel => "omnivoice_model",
            DownloadTarget::OmniVoiceMlx => "omnivoice_mlx",
            DownloadTarget::Cuda => "cuda",
            DownloadTarget::DiarizationMlx => "diarization_mlx",
            DownloadTarget::DiarizationFluid => "diarization_fluid",
            DownloadTarget::Models => "models",
        }
    }

    /// Parse a wire token. One canonical token per target; unknown → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "onnxruntime" => DownloadTarget::Onnxruntime,
            "kokoro_model" => DownloadTarget::KokoroModel,
            "kokoro_frontend" => DownloadTarget::KokoroFrontend,
            "kokoro_mlx" => DownloadTarget::KokoroMlx,
            "kokoro_fluid" => DownloadTarget::KokoroFluid,
            "parakeet_model" => DownloadTarget::ParakeetModel,
            "parakeet_mlx" => DownloadTarget::ParakeetMlx,
            "parakeet_fluid" => DownloadTarget::ParakeetFluid,
            "sepformer_model" => DownloadTarget::SepformerModel,
            "chatterbox_model" => DownloadTarget::ChatterboxModel,
            "chatterbox_mlx" => DownloadTarget::ChatterboxMlx,
            "qwen_model" => DownloadTarget::QwenModel,
            "qwen_mlx" => DownloadTarget::QwenMlx,
            "omnivoice_model" => DownloadTarget::OmniVoiceModel,
            "omnivoice_mlx" => DownloadTarget::OmniVoiceMlx,
            "cuda" => DownloadTarget::Cuda,
            "diarization_mlx" => DownloadTarget::DiarizationMlx,
            "diarization_fluid" => DownloadTarget::DiarizationFluid,
            "models" => DownloadTarget::Models,
            _ => return None,
        })
    }

    /// Fetchable on the running platform? One spelling of the gate — dispatchers
    /// (`start_download`, [`crate::spec::prefetch_items`], `run_prefetch`) must mirror it
    /// via [`ds_config::host`] rather than restating `(os, arch)`:
    ///
    /// * MLX + FluidAudio Core ML sets — Apple-Silicon macOS only
    /// * `sepformer_model` — macOS any arch (plain ONNX speaker-lock)
    /// * `cuda` — x86_64 Windows/Linux only
    /// * else (ORT dylib, ONNX model sets, `models` group) — universal
    pub fn is_supported_on_this_host(self) -> bool {
        match self {
            DownloadTarget::KokoroMlx
            | DownloadTarget::ChatterboxMlx
            | DownloadTarget::QwenMlx
            | DownloadTarget::OmniVoiceMlx
            | DownloadTarget::ParakeetMlx
            | DownloadTarget::DiarizationMlx
            | DownloadTarget::KokoroFluid
            | DownloadTarget::ParakeetFluid
            | DownloadTarget::DiarizationFluid => {
                ds_config::host::apple_silicon(Os::this(), Arch::this())
            }
            DownloadTarget::SepformerModel => Os::this() == Os::MacOs,
            DownloadTarget::Cuda => ds_config::host::cuda_host(Os::this(), Arch::this()),
            DownloadTarget::Onnxruntime
            | DownloadTarget::KokoroModel
            | DownloadTarget::KokoroFrontend
            | DownloadTarget::ParakeetModel
            | DownloadTarget::ChatterboxModel
            | DownloadTarget::QwenModel
            | DownloadTarget::OmniVoiceModel
            | DownloadTarget::Models => true,
        }
    }

    /// Built-in TTS model fetched by this target.
    pub fn tts_model(self) -> Option<ds_config::TtsModel> {
        Self::TTS_TARGETS
            .iter()
            .find_map(|(model, portable, mlx)| {
                (*portable == self || *mlx == self).then_some(*model)
            })
            .or_else(|| {
                Self::FLUID_TTS_TARGETS
                    .iter()
                    .find_map(|(model, fluid)| (*fluid == self).then_some(*model))
            })
    }

    /// Apple-MLX download target for one built-in TTS model.
    pub fn mlx_for_tts(model: ds_config::TtsModel) -> Self {
        Self::TTS_TARGETS
            .iter()
            .find_map(|(candidate, _, mlx)| (*candidate == model).then_some(*mlx))
            .expect("every TTS model has an MLX target")
    }

    /// Portable ONNX download target for one built-in TTS model.
    pub fn portable_for_tts(model: ds_config::TtsModel) -> Self {
        Self::TTS_TARGETS
            .iter()
            .find_map(|(candidate, portable, _)| (*candidate == model).then_some(*portable))
            .expect("every TTS model has a portable target")
    }

    pub fn is_mlx_tts(self) -> bool {
        Self::TTS_TARGETS.iter().any(|(_, _, mlx)| *mlx == self)
    }

    /// FluidAudio Core ML target for a built-in TTS model, or `None` when that backend has
    /// no set. Fallible unlike [`Self::mlx_for_tts`] — the flavor is partial.
    pub fn fluid_for_tts(model: ds_config::TtsModel) -> Option<Self> {
        Self::FLUID_TTS_TARGETS
            .iter()
            .find_map(|(candidate, fluid)| (*candidate == model).then_some(*fluid))
    }

    pub fn is_fluid_tts(self) -> bool {
        Self::FLUID_TTS_TARGETS
            .iter()
            .any(|(_, fluid)| *fluid == self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_as_str_and_parse() {
        for t in [
            DownloadTarget::Onnxruntime,
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroFrontend,
            DownloadTarget::KokoroMlx,
            DownloadTarget::KokoroFluid,
            DownloadTarget::ParakeetModel,
            DownloadTarget::ParakeetMlx,
            DownloadTarget::ParakeetFluid,
            DownloadTarget::SepformerModel,
            DownloadTarget::ChatterboxModel,
            DownloadTarget::ChatterboxMlx,
            DownloadTarget::QwenModel,
            DownloadTarget::QwenMlx,
            DownloadTarget::OmniVoiceModel,
            DownloadTarget::OmniVoiceMlx,
            DownloadTarget::Cuda,
            DownloadTarget::DiarizationMlx,
            DownloadTarget::DiarizationFluid,
            DownloadTarget::Models,
        ] {
            assert_eq!(DownloadTarget::parse(t.as_str()), Some(t), "{:?}", t);
        }
    }

    /// Partial Fluid TTS: `fluid_for_tts` fallible where `mlx_for_tts` is total;
    /// `tts_model` must still map the one target that exists.
    #[test]
    fn fluid_targets_exist_only_for_kokoro() {
        assert_eq!(
            DownloadTarget::fluid_for_tts(ds_config::TtsModel::Kokoro),
            Some(DownloadTarget::KokoroFluid)
        );
        for model in ds_config::TtsModel::ALL.iter().copied() {
            let fluid = DownloadTarget::fluid_for_tts(model);
            assert_eq!(
                fluid.is_some(),
                model == ds_config::TtsModel::Kokoro,
                "{model:?}"
            );
            if let Some(target) = fluid {
                assert_eq!(target.tts_model(), Some(model));
                assert!(target.is_fluid_tts());
                assert!(!target.is_mlx_tts(), "the two flavors stay disjoint");
            }
        }
        for other in [
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroMlx,
            DownloadTarget::ParakeetFluid,
            DownloadTarget::DiarizationFluid,
        ] {
            assert!(!other.is_fluid_tts(), "{other:?}");
        }
        assert_eq!(DownloadTarget::ParakeetFluid.tts_model(), None);
    }

    #[test]
    fn unknown_token_is_none() {
        assert_eq!(DownloadTarget::parse("bogus"), None);
        assert_eq!(DownloadTarget::parse(""), None);
    }

    // Literal per-OS expectations (not re-evaluated `cfg!` — that would be tautological);
    // a drifted gate fails on that platform's CI leg.
    #[test]
    fn platform_support_matrix() {
        use DownloadTarget::*;
        for t in [
            Onnxruntime,
            KokoroModel,
            KokoroFrontend,
            ParakeetModel,
            ChatterboxModel,
            QwenModel,
            OmniVoiceModel,
            Models,
        ] {
            assert!(t.is_supported_on_this_host(), "{t:?} must be universal");
        }
        // MLX/Fluid: Apple Silicon only. SepFormer: macOS any arch (matters on Intel).
        let apple_silicon_only = [
            KokoroMlx,
            ChatterboxMlx,
            QwenMlx,
            OmniVoiceMlx,
            ParakeetMlx,
            DiarizationMlx,
            KokoroFluid,
            ParakeetFluid,
            DiarizationFluid,
        ];
        #[cfg(target_os = "macos")]
        {
            #[cfg(target_arch = "aarch64")]
            for t in apple_silicon_only {
                assert!(
                    t.is_supported_on_this_host(),
                    "{t:?} is an Apple-Silicon target"
                );
            }
            #[cfg(not(target_arch = "aarch64"))]
            for t in apple_silicon_only {
                assert!(
                    !t.is_supported_on_this_host(),
                    "{t:?} is Apple-Silicon-only"
                );
            }
            assert!(
                SepformerModel.is_supported_on_this_host(),
                "SepformerModel is macOS on any arch"
            );
            assert!(
                !Cuda.is_supported_on_this_host(),
                "Cuda is not a macOS target"
            );
        }
        #[cfg(target_os = "windows")]
        {
            for t in apple_silicon_only {
                assert!(!t.is_supported_on_this_host(), "{t:?} is macOS-only");
            }
            assert!(!SepformerModel.is_supported_on_this_host());
            assert_eq!(
                Cuda.is_supported_on_this_host(),
                cfg!(target_arch = "x86_64")
            );
        }
        #[cfg(target_os = "linux")]
        {
            for t in apple_silicon_only {
                assert!(
                    !t.is_supported_on_this_host(),
                    "{t:?} is not a Linux target"
                );
            }
            assert!(!SepformerModel.is_supported_on_this_host());
            assert_eq!(
                Cuda.is_supported_on_this_host(),
                cfg!(target_arch = "x86_64")
            );
        }
    }
}
