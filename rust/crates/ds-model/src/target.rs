//! Canonical download / installer-prefetch targets.
//!
//! ONE enum every dispatcher matches (`prefetch_items`, `run_prefetch`, engine download
//! manager) so wire tokens can't typo-fallthrough. Parse only at the process boundary
//! (CLI); in-process APIs take the enum. Naming: `<brand>_<flavor>` for models,
//! bare nouns for runtimes/groups. Platform gate: [`is_supported_on_this_host`](DownloadTarget::is_supported_on_this_host).

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
    /// Full Parakeet streaming set + ORT. Wire `"parakeet_model"`.
    ParakeetModel,
    /// MLX Parakeet set ([`crate::mlx_repo::PARAKEET_MLX_SET`]). Apple-Silicon macOS.
    ParakeetMlx,
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

    /// The stable wire token for this target (what the IPC / CLI / installer pass).
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadTarget::Onnxruntime => "onnxruntime",
            DownloadTarget::KokoroModel => "kokoro_model",
            DownloadTarget::KokoroFrontend => "kokoro_frontend",
            DownloadTarget::KokoroMlx => "kokoro_mlx",
            DownloadTarget::ParakeetModel => "parakeet_model",
            DownloadTarget::ParakeetMlx => "parakeet_mlx",
            DownloadTarget::SepformerModel => "sepformer_model",
            DownloadTarget::ChatterboxModel => "chatterbox_model",
            DownloadTarget::ChatterboxMlx => "chatterbox_mlx",
            DownloadTarget::QwenModel => "qwen_model",
            DownloadTarget::QwenMlx => "qwen_mlx",
            DownloadTarget::OmniVoiceModel => "omnivoice_model",
            DownloadTarget::OmniVoiceMlx => "omnivoice_mlx",
            DownloadTarget::Cuda => "cuda",
            DownloadTarget::DiarizationMlx => "diarization_mlx",
            DownloadTarget::Models => "models",
        }
    }

    /// Parse a wire token into a target. Each target has exactly ONE canonical token (no
    /// legacy aliases). Returns `None` for an unknown token.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "onnxruntime" => DownloadTarget::Onnxruntime,
            "kokoro_model" => DownloadTarget::KokoroModel,
            "kokoro_frontend" => DownloadTarget::KokoroFrontend,
            "kokoro_mlx" => DownloadTarget::KokoroMlx,
            "parakeet_model" => DownloadTarget::ParakeetModel,
            "parakeet_mlx" => DownloadTarget::ParakeetMlx,
            "sepformer_model" => DownloadTarget::SepformerModel,
            "chatterbox_model" => DownloadTarget::ChatterboxModel,
            "chatterbox_mlx" => DownloadTarget::ChatterboxMlx,
            "qwen_model" => DownloadTarget::QwenModel,
            "qwen_mlx" => DownloadTarget::QwenMlx,
            "omnivoice_model" => DownloadTarget::OmniVoiceModel,
            "omnivoice_mlx" => DownloadTarget::OmniVoiceMlx,
            "cuda" => DownloadTarget::Cuda,
            "diarization_mlx" => DownloadTarget::DiarizationMlx,
            "models" => DownloadTarget::Models,
            _ => return None,
        })
    }

    /// Can this target actually be fetched on the RUNNING platform? The ONE spelling of
    /// every target's platform gate — the `#[cfg]`-gated fetch arms in each dispatcher
    /// (the engine's `start_download`, [`crate::spec::prefetch_items`], ds-helper's
    /// `run_prefetch`) must mirror this matrix:
    ///
    /// * the MLX sets (`kokoro_mlx` / `parakeet_mlx` / `diarization_mlx`)
    ///   exist only where the native shim runs — macOS;
    /// * `sepformer_model` is macOS-only too (the speaker-lock is macOS code), though it
    ///   is plain ONNX, not MLX;
    /// * `cuda` (the ONNX CUDA EP wheels) exists only on x86_64 Windows/Linux;
    /// * legacy `dotnet` / `winapp` tokens are not fetchable on any host;
    /// * everything else (the onnxruntime dylib and the ONNX model sets) is universal.
    pub fn is_supported_on_this_host(self) -> bool {
        match self {
            DownloadTarget::KokoroMlx
            | DownloadTarget::ChatterboxMlx
            | DownloadTarget::QwenMlx
            | DownloadTarget::OmniVoiceMlx
            | DownloadTarget::ParakeetMlx
            | DownloadTarget::DiarizationMlx => {
                cfg!(all(target_os = "macos", target_arch = "aarch64"))
            }
            DownloadTarget::SepformerModel => cfg!(target_os = "macos"),
            DownloadTarget::Cuda => cfg!(all(
                any(target_os = "windows", target_os = "linux"),
                target_arch = "x86_64"
            )),
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
        Self::TTS_TARGETS.iter().find_map(|(model, portable, mlx)| {
            (*portable == self || *mlx == self).then_some(*model)
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

    /// Whether this target is one of the four built-in MLX TTS sets.
    pub fn is_mlx_tts(self) -> bool {
        Self::TTS_TARGETS.iter().any(|(_, _, mlx)| *mlx == self)
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
            DownloadTarget::ParakeetModel,
            DownloadTarget::ParakeetMlx,
            DownloadTarget::SepformerModel,
            DownloadTarget::ChatterboxModel,
            DownloadTarget::ChatterboxMlx,
            DownloadTarget::QwenModel,
            DownloadTarget::QwenMlx,
            DownloadTarget::OmniVoiceModel,
            DownloadTarget::OmniVoiceMlx,
            DownloadTarget::Cuda,
            DownloadTarget::DiarizationMlx,
            DownloadTarget::Models,
        ] {
            assert_eq!(DownloadTarget::parse(t.as_str()), Some(t), "{:?}", t);
        }
    }

    #[test]
    fn every_target_has_one_canonical_token_no_legacy_aliases() {
        assert_eq!(DownloadTarget::KokoroModel.as_str(), "kokoro_model");
        assert_eq!(DownloadTarget::KokoroFrontend.as_str(), "kokoro_frontend");
        assert_eq!(DownloadTarget::ParakeetModel.as_str(), "parakeet_model");
        assert_eq!(DownloadTarget::ChatterboxModel.as_str(), "chatterbox_model");
        assert_eq!(DownloadTarget::ChatterboxMlx.as_str(), "chatterbox_mlx");
        assert_eq!(DownloadTarget::QwenModel.as_str(), "qwen_model");
        assert_eq!(DownloadTarget::QwenMlx.as_str(), "qwen_mlx");
        assert_eq!(DownloadTarget::OmniVoiceModel.as_str(), "omnivoice_model");
        assert_eq!(DownloadTarget::OmniVoiceMlx.as_str(), "omnivoice_mlx");
        assert_eq!(DownloadTarget::parse("chatterbox"), None);
        assert_eq!(DownloadTarget::parse("chatterbox_en_model"), None);
        assert_eq!(DownloadTarget::Onnxruntime.as_str(), "onnxruntime");
        assert_eq!(DownloadTarget::DiarizationMlx.as_str(), "diarization_mlx");
        // The pre-rename bare brand tokens are NOT accepted (single canonical name, no aliases).
        assert_eq!(DownloadTarget::parse("kokoro"), None);
        assert_eq!(DownloadTarget::parse("parakeet"), None);
        assert_eq!(DownloadTarget::parse("kokoro_voices"), None);
        // Pre-MLX Apple target tokens are not aliases.
        assert_eq!(DownloadTarget::parse("kokoro_coreml"), None);
        assert_eq!(DownloadTarget::parse("parakeet_coreml"), None);
        assert_eq!(DownloadTarget::parse("diarization_coreml"), None);
        // Same for the older runtime/diarization tokens.
        assert_eq!(DownloadTarget::parse("onnx"), None);
        assert_eq!(DownloadTarget::parse("diarization"), None);
        // The legacy "all" combined-fetch target is GONE: the engine sequences per-model
        // targets (each row shows its own %), and the installer prefetch's no-arg default
        // falls through run_prefetch's unknown-token arm to the same models+cuda behavior.
        assert_eq!(DownloadTarget::parse("all"), None);
    }

    #[test]
    fn unknown_token_is_none() {
        assert_eq!(DownloadTarget::parse("bogus"), None);
        assert_eq!(DownloadTarget::parse(""), None);
    }

    // The platform-support matrix, pinned as LITERAL per-OS expectations (not re-evaluated
    // `cfg!` expressions, which would be tautological) — a drifted gate shows up on that
    // platform's CI leg rather than silently mis-routing a dispatcher.
    #[test]
    fn platform_support_matrix() {
        use DownloadTarget::*;
        // Universal targets: fetchable everywhere.
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
        // The four MLX sets are Apple-Silicon-only; the plain-ONNX speaker-lock separator
        // is macOS on ANY arch — the split matters on Intel macOS.
        let mlx_only = [
            KokoroMlx,
            ChatterboxMlx,
            QwenMlx,
            OmniVoiceMlx,
            ParakeetMlx,
            DiarizationMlx,
        ];
        #[cfg(target_os = "macos")]
        {
            #[cfg(target_arch = "aarch64")]
            for t in mlx_only {
                assert!(
                    t.is_supported_on_this_host(),
                    "{t:?} is an Apple-Silicon target"
                );
            }
            #[cfg(not(target_arch = "aarch64"))]
            for t in mlx_only {
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
            for t in mlx_only {
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
            for t in mlx_only {
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
