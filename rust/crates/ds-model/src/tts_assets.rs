//! Pinned ONNX assets for every built-in TTS model.

use std::path::PathBuf;

use ds_config::TtsModel;

use crate::download::ensure_in_dir;
use crate::hash::verify_sha256_cached;
use crate::setup::{DownloadStep, run_download_set};
use crate::spec::ModelSpec;
use crate::urls::{self, Download};

macro_rules! hf_url {
    ($repo:literal, CHATTERBOX_REV, $path:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/452d3f434aa592098f1eedac9099f33642ab2da5/",
            $path
        )
    };
    ($repo:literal, QWEN_ONNX_REV, $path:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/18b0bf898d211718bc33082fadc36a448e0cbc0c/",
            $path
        )
    };
    ($repo:literal, QWEN_TOKENIZER_REV, $path:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/85e237c12c027371202489a0ec509ded67b5e4b5/",
            $path
        )
    };
    ($repo:literal, OMNIVOICE_REV, $path:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/65b5f5324837023204c6a6fa8c0c3ece2f1ab2bf/",
            $path
        )
    };
    ($repo:literal, OMNIVOICE_BIDI_REV, $path:literal) => {
        concat!(
            "https://huggingface.co/",
            $repo,
            "/resolve/a0109dfbd1ec0ec5874d15a1b32353886a5f17dc/",
            $path
        )
    };
}

macro_rules! hf_asset {
    ($name:ident, $file:literal, $repo:literal, $rev:ident, $path:literal, $sha:literal, $size:literal) => {
        const $name: Download = Download {
            file_name: $file,
            url: hf_url!($repo, $rev, $path),
            sha256: $sha,
            size_bytes: $size,
        };
    };
}

hf_asset!(
    CB_SPEECH_ENCODER,
    "speech_encoder.onnx",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/speech_encoder.onnx",
    "8f1c8a0f89b77bf9cd5dd8f2e034eb2c79dc00fe70d41196b28c257643b00ccb",
    1_184_608
);
hf_asset!(
    CB_SPEECH_ENCODER_DATA,
    "speech_encoder.onnx_data",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/speech_encoder.onnx_data",
    "92f8f290fc9720e169bc2412c507209e20b03f6564bc3243739e25c56f7dfb8f",
    591_274_880
);
hf_asset!(
    CB_EMBED_TOKENS,
    "embed_tokens.onnx",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/embed_tokens.onnx",
    "f785819ca4f6271262d5bb8971d62796c3a909e3b031982c113dbe83a4c3b854",
    13_286
);
hf_asset!(
    CB_EMBED_TOKENS_DATA,
    "embed_tokens.onnx_data",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/embed_tokens.onnx_data",
    "2a15f7dd73b2ee47f6edf87740324011594b5a528ed6471ae55e327ed6cad68c",
    68_390_912
);
hf_asset!(
    CB_LANGUAGE_MODEL,
    "language_model_fp16.onnx",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/language_model_fp16.onnx",
    "0c36a5bbbc2a4ed8c345033896612cd320fd0971a0f5e6447ab4cdd2d7f22e36",
    172_657
);
hf_asset!(
    CB_LANGUAGE_MODEL_DATA,
    "language_model_fp16.onnx_data",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/language_model_fp16.onnx_data",
    "16dca11ae994e78427fa3090cc6faf347a15988ca40809c1bd9f2721f3b759a0",
    1_040_316_416
);
hf_asset!(
    CB_DECODER,
    "conditional_decoder.onnx",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/conditional_decoder.onnx",
    "1656d0d31332bae1854839959a3139300ebb67c178651dfa3f8c5fbfa5351351",
    6_350_448
);
hf_asset!(
    CB_DECODER_DATA,
    "conditional_decoder.onnx_data",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/conditional_decoder.onnx_data",
    "51d58345a272747665ec9d5bb61e01835258a940e321a288582ac4c18cf01b5a",
    533_970_816
);
hf_asset!(
    CB_TOKENIZER,
    "tokenizer.json",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "tokenizer.json",
    "29d48c4a178f6af3ad5130097c34744639e9294847b38a7b912c8c68027cb819",
    71_798
);
hf_asset!(
    CB_TOKENIZER_CONFIG,
    "tokenizer_config.json",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "tokenizer_config.json",
    "b35967f93e30313d05fc9d520721ca9f671aaa5b3edbb03059aed3ff68b4c4c0",
    244
);
hf_asset!(
    CB_DEFAULT_VOICE,
    "default_voice.wav",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "default_voice.wav",
    "3ebc531cdaba358a327099c1c4f0448026719957bcf4d8e9868767f227e02f4e",
    714_320
);
hf_asset!(
    CB_CANGJIE,
    "Cangjie5_TC.json",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "Cangjie5_TC.json",
    "7073fd9de919443ae88e0bd2449917a65fe54898a4413ed1edcc4b67f28bce8c",
    1_920_163
);

hf_asset!(
    QWEN_CODE_PREDICTOR,
    "code_predictor.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/code_predictor.onnx",
    "2bf685d424c0e7b36363c2139fe18c381da3188e037587f87ccfda42c2bd085f",
    285_552_428
);
hf_asset!(
    QWEN_CODEC_EMBED,
    "codec_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/codec_embed.onnx",
    "7f5a774e01e8d8b7d788b4e3839e1188ce5ad3497044aeabca93dfe21ee6aec0",
    6_291_797
);
hf_asset!(
    QWEN_RESIDUAL_EMBED,
    "residual_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/residual_embed.onnx",
    "95a5d7ff95bdda5ea71a772dfb4a092313679b8a874af45801de1aa881adc7c4",
    69_215_780
);
hf_asset!(
    QWEN_TALKER,
    "talker_cache.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/talker_cache.onnx",
    "a4170b103de61c89ab78a2471c2ca19eea9f6daad7b53b869d1a22c6aa1068c3",
    891_756_744
);
hf_asset!(
    QWEN_TEXT_EMBED,
    "text_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/text_embed.onnx",
    "3c1ec888fde0960d81e1a17f13c9ad737fb60693453916a49d7e78b171214204",
    634_920_759
);
hf_asset!(
    QWEN_DECODER,
    "tok_decoder.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_fp16/tok_decoder.onnx",
    "8ec10051735029f6e08b04834128c9108428885c39384f443e49a6790ccb129f",
    458_268_831
);
hf_asset!(
    QWEN_CONFIG,
    "config.json",
    "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_TOKENIZER_REV,
    "config.json",
    "81aca2b6fac304944d8acf345272d8a9a727d5fc2e2e66b222ab4729340c7455",
    4_908
);
hf_asset!(
    QWEN_MERGES,
    "merges.txt",
    "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_TOKENIZER_REV,
    "merges.txt",
    "599bab54075088774b1733fde865d5bd747cbcc7a547c5bc12610e874e26f5e3",
    1_671_839
);
hf_asset!(
    QWEN_TOKENIZER_CONFIG,
    "tokenizer_config.json",
    "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_TOKENIZER_REV,
    "tokenizer_config.json",
    "dc3c31c3bdaedd5016382bb3cbe07323026775ad51f5a4fb564505992ae4a670",
    7_344
);
hf_asset!(
    QWEN_VOCAB,
    "vocab.json",
    "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_TOKENIZER_REV,
    "vocab.json",
    "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    2_776_833
);

hf_asset!(
    OMNI_AUDIO_EMBED,
    "audio_embeddings_encoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_embeddings_encoder.onnx",
    "5b216f18a58e33e52e7d2a85ea843b996b1b7da6ecfbc5d70fb82f5e15136ab6",
    2_172
);
hf_asset!(
    OMNI_AUDIO_EMBED_DATA,
    "audio_embeddings_encoder.onnx.data",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_embeddings_encoder.onnx.data",
    "e2c605b03749beefd0b9d1c037b1ebd2d0c9c5c29928d7e382a1d4e5d3316660",
    327_426_048
);
hf_asset!(
    OMNI_AUDIO_HEADS,
    "audio_heads_decoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_heads_decoder.onnx",
    "38e69afe9c9aa531fa59f23337d7fecbe84b7b04dc9c3ce0b871249a1c659ad4",
    16_795_584
);
// One fp32 profile serves every EP. Keep the on-disk basename embedded by the graph or
// its external weights will not resolve.
hf_asset!(
    OMNI_LLM,
    "llm_backbone_fp32.onnx",
    "dellusional/OmniVoice-ONNX-bidirectional",
    OMNIVOICE_BIDI_REV,
    "llm_backbone_fp32.onnx",
    "5643dbbf00e50b1f500123d1669e7966f7888a7770b10acb4cf69c5a5f6b6d09",
    9_111_300
);
hf_asset!(
    OMNI_LLM_DATA,
    "llm_backbone_fp32.onnx.data",
    "dellusional/OmniVoice-ONNX-bidirectional",
    OMNIVOICE_BIDI_REV,
    "llm_backbone_fp32.onnx.data",
    "7ba81a2fdcfb63f9cc7c1d0186414a8edc437cb913c92ca6b2a7778c87baca17",
    1_761_869_824
);
hf_asset!(
    OMNI_TOKENIZER,
    "tokenizer.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "tokenizer.json",
    "408f669b7e2b045fdf54201d815bd364e6667dbd845115da81239c40bc6dcfd1",
    11_423_986
);
hf_asset!(
    OMNI_CONFIG,
    "config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "config.json",
    "a8957a1c6e980b1f13347af7f33b339be6566f35dec66365dedaedabf51f61e9",
    1_416
);
hf_asset!(
    OMNI_MODEL_CONFIG,
    "model_config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "model_config.json",
    "cc4d867113078c19d500469bf5dd8bfd5344dfdae5acb2da6be8110c40b2fc8d",
    341
);
hf_asset!(
    OMNI_TOKENIZER_CONFIG,
    "tokenizer_config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "tokenizer_config.json",
    "49f78845596a82bf15c83673794bdf9f76f812b11f60ab6a2239d9be65b00676",
    533
);
hf_asset!(
    OMNI_WAVE_DECODER,
    "higgs_decoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_tokenizer/fp16/higgs_decoder.onnx",
    "19b368e460fcf4a0352fd49127d37b75327b34a17e82c31599155f8968ce4002",
    308_494
);
hf_asset!(
    OMNI_WAVE_DECODER_DATA,
    "higgs_decoder.onnx.data",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_tokenizer/fp16/higgs_decoder.onnx.data",
    "5be7ff51d117dd1cd001b87ec6e6e93f61cbe923355c50841a15fcaf19f64a19",
    43_100_160
);

static KOKORO_FILES: [Download; 4] = [
    urls::KOKORO_ONNX,
    urls::KOKORO_VOICES,
    urls::KOKORO_G2P_ENCODER,
    urls::KOKORO_G2P_DECODER,
];
static CHATTERBOX_FILES: [Download; 12] = [
    CB_SPEECH_ENCODER,
    CB_SPEECH_ENCODER_DATA,
    CB_EMBED_TOKENS,
    CB_EMBED_TOKENS_DATA,
    CB_LANGUAGE_MODEL,
    CB_LANGUAGE_MODEL_DATA,
    CB_DECODER,
    CB_DECODER_DATA,
    CB_TOKENIZER,
    CB_TOKENIZER_CONFIG,
    CB_DEFAULT_VOICE,
    CB_CANGJIE,
];
static QWEN_FILES: [Download; 10] = [
    QWEN_CODE_PREDICTOR,
    QWEN_CODEC_EMBED,
    QWEN_RESIDUAL_EMBED,
    QWEN_TALKER,
    QWEN_TEXT_EMBED,
    QWEN_DECODER,
    QWEN_CONFIG,
    QWEN_MERGES,
    QWEN_TOKENIZER_CONFIG,
    QWEN_VOCAB,
];
static OMNIVOICE_FILES: [Download; 11] = [
    OMNI_AUDIO_EMBED,
    OMNI_AUDIO_EMBED_DATA,
    OMNI_AUDIO_HEADS,
    OMNI_LLM,
    OMNI_LLM_DATA,
    OMNI_TOKENIZER,
    OMNI_CONFIG,
    OMNI_MODEL_CONFIG,
    OMNI_TOKENIZER_CONFIG,
    OMNI_WAVE_DECODER,
    OMNI_WAVE_DECODER_DATA,
];
/// Attribution-only partitions for files with a different upstream license. Downloads
/// and presence checks still use the parent set.
#[derive(Debug)]
pub struct DerivedAttribution {
    pub project: &'static urls::Project,
    pub files: &'static [Download],
}

static OMNIVOICE_HIGGS_FILES: [Download; 2] = [OMNI_WAVE_DECODER, OMNI_WAVE_DECODER_DATA];
static OMNIVOICE_BIDI_FILES: [Download; 2] = [OMNI_LLM, OMNI_LLM_DATA];
static OMNIVOICE_ATTRIBUTION_PARTITIONS: [DerivedAttribution; 2] = [
    DerivedAttribution {
        project: &urls::OMNIVOICE_HIGGS_TOKENIZER,
        files: &OMNIVOICE_HIGGS_FILES,
    },
    DerivedAttribution {
        project: &urls::OMNIVOICE_BIDI_EXPORT,
        files: &OMNIVOICE_BIDI_FILES,
    },
];

#[derive(Debug)]
pub struct TtsOrtAssetSet {
    pub model: TtsModel,
    pub dir_name: Option<&'static str>,
    /// Files every provider needs.
    pub files: &'static [Download],
    /// Additional assets for CUDA-realized loads.
    pub cuda_files: &'static [Download],
    pub display_name: &'static str,
    /// Explicit project page for the Libraries-tab attribution — never derived from a
    /// file URL's shape, and never the license URL.
    pub homepage: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
    pub attribution_partitions: &'static [DerivedAttribution],
}

pub static TTS_ORT_ASSETS: [TtsOrtAssetSet; 4] = [
    TtsOrtAssetSet {
        model: TtsModel::Kokoro,
        dir_name: None,
        files: &KOKORO_FILES,
        cuda_files: &[],
        display_name: "Kokoro",
        homepage: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
        attribution_partitions: &[],
    },
    TtsOrtAssetSet {
        model: TtsModel::Chatterbox,
        dir_name: Some("chatterbox-multilingual"),
        files: &CHATTERBOX_FILES,
        cuda_files: &[],
        display_name: "Chatterbox Multilingual",
        homepage: "https://huggingface.co/onnx-community/chatterbox-multilingual-ONNX",
        license: "MIT",
        license_url: "https://opensource.org/license/mit",
        attribution_partitions: &[],
    },
    TtsOrtAssetSet {
        model: TtsModel::Qwen,
        dir_name: Some("qwen3-tts"),
        files: &QWEN_FILES,
        cuda_files: &[],
        display_name: "Qwen3-TTS",
        homepage: "https://huggingface.co/onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
        attribution_partitions: &[],
    },
    TtsOrtAssetSet {
        model: TtsModel::OmniVoice,
        dir_name: Some("omnivoice"),
        files: &OMNIVOICE_FILES,
        cuda_files: &[],
        display_name: "OmniVoice",
        homepage: "https://huggingface.co/onnx-community/OmniVoice-Onnx",
        // Apache-2.0 covers code, not these weights; Higgs files use a partition.
        license: "CC-BY-NC-4.0",
        license_url: "https://creativecommons.org/licenses/by-nc/4.0/",
        attribution_partitions: &OMNIVOICE_ATTRIBUTION_PARTITIONS,
    },
];

impl TtsOrtAssetSet {
    /// Single source for byte totals and steps so progress cannot finish before CUDA extras.
    pub fn files_for(&self, cuda_assets: bool) -> impl Iterator<Item = &'static Download> {
        let extra: &'static [Download] = if cuda_assets { self.cuda_files } else { &[] };
        self.files.iter().chain(extra)
    }
}

pub fn tts_ort_asset_set(model: TtsModel) -> &'static TtsOrtAssetSet {
    &TTS_ORT_ASSETS[model as usize]
}

/// False on targets without a published CUDA runtime.
pub fn cuda_runtime_available() -> bool {
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    {
        crate::ort::is_cuda_runtime_present()
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    {
        false
    }
}

/// Select CUDA assets only when both preference and an installed runtime allow CUDA.
pub fn tts_wants_cuda_assets(model: TtsModel, preference: &str) -> bool {
    tts_wants_cuda_assets_with(model, preference, cuda_runtime_available())
}

/// Injected-runtime form shared with provider realization to prevent drift.
pub fn tts_wants_cuda_assets_with(model: TtsModel, preference: &str, cuda_available: bool) -> bool {
    model.descriptor().wants_cuda(preference) && cuda_available
}

pub fn tts_model_dir(model: TtsModel) -> Option<PathBuf> {
    let base = ds_config::model_dir()?;
    Some(match tts_ort_asset_set(model).dir_name {
        Some(dir) => base.join(dir),
        None => base,
    })
}

pub fn tts_model_file_path(model: TtsModel, file_name: &str) -> Option<PathBuf> {
    Some(tts_model_dir(model)?.join(file_name))
}

/// `cuda_assets` is the effective-provider decision, not the raw preference.
pub fn tts_model_files_present(model: TtsModel, cuda_assets: bool) -> bool {
    let Some(dir) = tts_model_dir(model) else {
        return false;
    };
    tts_ort_asset_set(model)
        .files_for(cuda_assets)
        .all(|file| dir.join(file.file_name).is_file())
}

pub fn is_tts_model_present(model: TtsModel, cuda_assets: bool) -> bool {
    let Some(dir) = tts_model_dir(model) else {
        return false;
    };
    tts_ort_asset_set(model)
        .files_for(cuda_assets)
        .all(|file| verify_sha256_cached(&dir.join(file.file_name), file.sha256))
        && crate::ort::is_onnxruntime_dylib_version_ok()
}

fn spec(file: &Download) -> ModelSpec {
    ModelSpec {
        file_name: file.file_name.to_string(),
        url: file.url.to_string(),
        sha256: file.sha256.to_string(),
    }
}

/// Fetch one effective-provider set, using the same iterator for totals and steps.
pub fn run_setup_tts_model_with_progress(
    model: TtsModel,
    cuda_assets: bool,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let set = tts_ort_asset_set(model);
    let dir = tts_model_dir(model).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    let mut total: u64 = set.files_for(cuda_assets).map(|file| file.size_bytes).sum();
    if crate::ort::onnxruntime_dist().is_some() {
        total += crate::urls::ONNXRUNTIME_DIST_SIZE_BYTES;
    }
    let mut steps: Vec<DownloadStep> = set
        .files_for(cuda_assets)
        .map(|file| {
            let dir = dir.clone();
            let spec = spec(file);
            Box::new(move |p: &dyn Fn(u64, u64)| ensure_in_dir(&dir, &spec, p).map(|_| ()))
                as DownloadStep
        })
        .collect();
    steps.push(Box::new(|p| {
        crate::ort::ensure_onnxruntime_with_progress(p).map(|_| ())
    }));
    run_download_set(progress, total, steps)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    #[test]
    fn registry_matches_config_models_and_pins_every_file() {
        for model in TtsModel::ALL.iter().copied() {
            let set = tts_ort_asset_set(model);
            assert_eq!(set.model, model);
            assert!(!set.files.is_empty());
            assert!(set.homepage.starts_with("https://"), "{}", set.display_name);
            assert_ne!(set.homepage, set.license_url, "{}", set.display_name);
            for file in set.files_for(true) {
                assert!(file.url.starts_with("https://"));
                assert!(!file.url.contains("/resolve/main/"));
                assert_eq!(file.sha256.len(), 64, "{}", file.file_name);
                assert!(file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(file.size_bytes > 0);
            }
        }
    }

    #[test]
    fn portable_profiles_stay_on_the_closest_compatible_precision() {
        let chatterbox = tts_ort_asset_set(TtsModel::Chatterbox);
        assert!(
            chatterbox
                .files
                .iter()
                .any(|file| { file.url.contains("/onnx/language_model_fp16.onnx") })
        );
        assert!(!chatterbox.files.iter().any(|file| file.url.contains("q4")));

        let qwen = tts_ort_asset_set(TtsModel::Qwen);
        assert!(
            qwen.files
                .iter()
                .filter(|file| file.file_name.ends_with(".onnx"))
                .all(|file| file.url.contains("/cpu_fp16/"))
        );

        let omnivoice = tts_ort_asset_set(TtsModel::OmniVoice);
        assert!(
            omnivoice
                .files
                .iter()
                .any(|file| file.url.contains("fp32") && file.file_name.starts_with("llm_"))
        );
        for file in omnivoice.files_for(true) {
            assert!(!file.url.contains("int4"), "{}", file.url);
            assert!(!file.url.contains("/cuda/"), "{}", file.url);
        }
        assert!(omnivoice.files.iter().any(|file| {
            file.url
                .contains("/audio_tokenizer/fp16/higgs_decoder.onnx")
        }));
    }

    #[test]
    fn no_model_pins_cuda_only_assets() {
        assert!(tts_ort_asset_set(TtsModel::OmniVoice).cuda_files.is_empty());
        for set in &TTS_ORT_ASSETS {
            assert!(set.cuda_files.is_empty(), "{}", set.display_name);
            for file in set.files {
                assert!(!file.file_name.starts_with("cuda/"), "{}", file.file_name);
                assert!(!file.url.contains("/cuda/"), "{}", file.url);
            }
        }
    }

    #[test]
    fn cuda_extras_are_listed_only_for_a_cuda_effective_load() {
        for model in TtsModel::ALL.iter().copied() {
            let set = tts_ort_asset_set(model);
            assert_eq!(set.files_for(false).count(), set.files.len(), "{model:?}");
            assert_eq!(
                set.files_for(true).count(),
                set.files.len() + set.cuda_files.len(),
                "{model:?}"
            );
        }
        assert!(tts_ort_asset_set(TtsModel::Qwen).cuda_files.is_empty());
    }

    #[test]
    fn cuda_assets_need_the_runtime_not_just_the_preference() {
        for preference in ["auto", "cuda"] {
            assert!(tts_wants_cuda_assets_with(
                TtsModel::OmniVoice,
                preference,
                true
            ));
            assert!(!tts_wants_cuda_assets_with(
                TtsModel::OmniVoice,
                preference,
                false
            ));
        }
        assert!(!tts_wants_cuda_assets_with(
            TtsModel::OmniVoice,
            "cpu",
            true
        ));
        assert!(!tts_wants_cuda_assets_with(
            TtsModel::OmniVoice,
            "mlx",
            true
        ));
    }

    #[test]
    fn attribution_partitions_reference_set_files_exactly_once() {
        for model in TtsModel::ALL.iter().copied() {
            let set = tts_ort_asset_set(model);
            let all: Vec<&str> = set.files_for(true).map(|file| file.url).collect();
            let mut seen: Vec<&str> = Vec::new();
            for partition in set.attribution_partitions {
                assert!(!partition.files.is_empty(), "{model:?} empty partition");
                assert!(!partition.project.license.is_empty());
                for file in partition.files {
                    assert!(
                        all.contains(&file.url),
                        "{model:?} partitions a file its set does not pin: {}",
                        file.url
                    );
                    assert!(
                        !seen.contains(&file.url),
                        "{model:?} partitions {} twice",
                        file.url
                    );
                    seen.push(file.url);
                }
            }
        }
    }

    #[test]
    fn omnivoice_licensing_is_never_apache() {
        let omnivoice = tts_ort_asset_set(TtsModel::OmniVoice);
        assert_ne!(omnivoice.license, "Apache-2.0");
        assert_eq!(omnivoice.license, "CC-BY-NC-4.0");
        let partitioned: Vec<&str> = omnivoice
            .attribution_partitions
            .iter()
            .flat_map(|partition| partition.files.iter().map(|file| file.url))
            .collect();
        for file in omnivoice.files_for(true) {
            if file.url.contains("audio_tokenizer") {
                assert!(
                    partitioned.contains(&file.url),
                    "audio_tokenizer file {} must carry the Boson partition",
                    file.url
                );
            }
        }
    }

    #[test]
    fn model_subdirectories_do_not_collide() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("DONTSPEAK_MODEL_DIR");
        // SAFETY: this test serializes the process-wide environment and restores it below.
        unsafe { std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path()) };
        assert_eq!(
            tts_model_dir(TtsModel::Kokoro),
            Some(tmp.path().to_path_buf())
        );
        assert_eq!(
            tts_model_dir(TtsModel::Qwen),
            Some(tmp.path().join("qwen3-tts"))
        );
        assert!(!tts_model_files_present(TtsModel::OmniVoice, false));
        assert!(!tts_model_files_present(TtsModel::OmniVoice, true));
        // SAFETY: restore the environment before releasing the serialization guard.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("DONTSPEAK_MODEL_DIR", value),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }
    }
}
