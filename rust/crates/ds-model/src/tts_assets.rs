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
    "language_model_q4f16.onnx",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/language_model_q4f16.onnx",
    "3b78e9235be5e2e2a811e482399155cb30415f6d87c98c21d12bf48843fc928f",
    229_388
);
hf_asset!(
    CB_LANGUAGE_MODEL_DATA,
    "language_model_q4f16.onnx_data",
    "onnx-community/chatterbox-multilingual-ONNX",
    CHATTERBOX_REV,
    "onnx/language_model_q4f16.onnx_data",
    "bdbc79504d20742b5d028074b4f1cdca8872e013fdfbbcea6b8b03154fe85a42",
    304_737_408
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
    "cpu_int4/code_predictor.onnx",
    "f0b3a431b90a56d2784599948bcff678d5188e38481d3a991d80c550d0d0bfa2",
    91_668_866
);
hf_asset!(
    QWEN_CODEC_EMBED,
    "codec_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_int4/codec_embed.onnx",
    "bd76cd6b77907c76b6d0055ea6cf7572468ab99dc3451c1176ea2189d1a55f70",
    2_015_779
);
hf_asset!(
    QWEN_RESIDUAL_EMBED,
    "residual_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_int4/residual_embed.onnx",
    "eec76513b562f558e76eeeb9b0bfa982de512104cddac372ec368ab504b080a0",
    22_179_258
);
hf_asset!(
    QWEN_TALKER,
    "talker_cache.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_int4/talker_cache.onnx",
    "910e1d556cd74f11bbb55fff9d509a962bfc43f23608b62fb0a8215c7acc882f",
    288_573_853
);
hf_asset!(
    QWEN_TEXT_EMBED,
    "text_embed.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_int4/text_embed.onnx",
    "b2923edfce01344dea9bd719e2daf94bda0b45bfd05a307514e9dd5f793dcd09",
    203_384_915
);
hf_asset!(
    QWEN_DECODER,
    "tok_decoder.onnx",
    "onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
    QWEN_ONNX_REV,
    "cpu_int4/tok_decoder.onnx",
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
    "int4/audio_embeddings_encoder.onnx",
    "f21daf4ee076c841d210b8941063a057c6e36d2bc04f4405daefa15b82c05563",
    2_363
);
hf_asset!(
    OMNI_AUDIO_EMBED_DATA,
    "audio_embeddings_encoder.onnx.data",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/audio_embeddings_encoder.onnx.data",
    "f6ed8d313fa5f6d2d33f3ad0af077dd1ba6d607f2a573cb389f28eec53f84393",
    87_160_832
);
hf_asset!(
    OMNI_AUDIO_HEADS,
    "audio_heads_decoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/audio_heads_decoder.onnx",
    "389f2133371f7f0bd678aba8cf1eac102ee4b227d3372f49bd181c591b890360",
    4_462_676
);
hf_asset!(
    OMNI_LLM,
    "llm_decoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/llm_decoder.onnx",
    "dba2abc6753795f47e9c2f79e274ba1149de8a056f4cda35c7fb30fa0e50fedc",
    298_798
);
hf_asset!(
    OMNI_LLM_DATA,
    "llm_decoder.onnx.data",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/llm_decoder.onnx.data",
    "59aa22f43d7b501d9ce64183106e60b65f97fb67e92f5d9e088d07504cf63383",
    296_484_864
);
hf_asset!(
    OMNI_TOKENIZER,
    "tokenizer.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/tokenizer.json",
    "408f669b7e2b045fdf54201d815bd364e6667dbd845115da81239c40bc6dcfd1",
    11_423_986
);
hf_asset!(
    OMNI_CONFIG,
    "config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/config.json",
    "a8957a1c6e980b1f13347af7f33b339be6566f35dec66365dedaedabf51f61e9",
    1_416
);
hf_asset!(
    OMNI_MODEL_CONFIG,
    "model_config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/model_config.json",
    "cc4d867113078c19d500469bf5dd8bfd5344dfdae5acb2da6be8110c40b2fc8d",
    341
);
hf_asset!(
    OMNI_TOKENIZER_CONFIG,
    "tokenizer_config.json",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "int4/tokenizer_config.json",
    "49f78845596a82bf15c83673794bdf9f76f812b11f60ab6a2239d9be65b00676",
    533
);
hf_asset!(
    OMNI_WAVE_DECODER,
    "higgs_decoder.onnx",
    "onnx-community/OmniVoice-Onnx",
    OMNIVOICE_REV,
    "audio_tokenizer/higgs_decoder.onnx",
    "b28b3317a6cbf0d0f4a71b476ecb5c767612a31702da6561154783c94b6fa806",
    86_500_102
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
static OMNIVOICE_FILES: [Download; 10] = [
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
];

#[derive(Debug)]
pub struct TtsOrtAssetSet {
    pub model: TtsModel,
    pub dir_name: Option<&'static str>,
    pub files: &'static [Download],
    pub display_name: &'static str,
    /// Explicit project page for the Libraries-tab attribution — never derived from a
    /// file URL's shape, and never the license URL.
    pub homepage: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
}

pub static TTS_ORT_ASSETS: [TtsOrtAssetSet; 4] = [
    TtsOrtAssetSet {
        model: TtsModel::Kokoro,
        dir_name: None,
        files: &KOKORO_FILES,
        display_name: "Kokoro",
        homepage: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    },
    TtsOrtAssetSet {
        model: TtsModel::Chatterbox,
        dir_name: Some("chatterbox-multilingual"),
        files: &CHATTERBOX_FILES,
        display_name: "Chatterbox Multilingual",
        homepage: "https://huggingface.co/onnx-community/chatterbox-multilingual-ONNX",
        license: "MIT",
        license_url: "https://opensource.org/license/mit",
    },
    TtsOrtAssetSet {
        model: TtsModel::Qwen,
        dir_name: Some("qwen3-tts"),
        files: &QWEN_FILES,
        display_name: "Qwen3-TTS",
        homepage: "https://huggingface.co/onnx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    },
    TtsOrtAssetSet {
        model: TtsModel::OmniVoice,
        dir_name: Some("omnivoice"),
        files: &OMNIVOICE_FILES,
        display_name: "OmniVoice",
        homepage: "https://huggingface.co/onnx-community/OmniVoice-Onnx",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    },
];

pub fn tts_ort_asset_set(model: TtsModel) -> &'static TtsOrtAssetSet {
    &TTS_ORT_ASSETS[model as usize]
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

pub fn tts_model_files_present(model: TtsModel) -> bool {
    let Some(dir) = tts_model_dir(model) else {
        return false;
    };
    tts_ort_asset_set(model)
        .files
        .iter()
        .all(|file| dir.join(file.file_name).is_file())
}

pub fn is_tts_model_present(model: TtsModel) -> bool {
    let Some(dir) = tts_model_dir(model) else {
        return false;
    };
    tts_ort_asset_set(model)
        .files
        .iter()
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

pub fn run_setup_tts_model_with_progress(
    model: TtsModel,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    let set = tts_ort_asset_set(model);
    let dir = tts_model_dir(model).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    let mut total: u64 = set.files.iter().map(|file| file.size_bytes).sum();
    if crate::ort::onnxruntime_dist().is_some() {
        total += crate::urls::ONNXRUNTIME_DIST_SIZE_BYTES;
    }
    let mut steps: Vec<DownloadStep> = set
        .files
        .iter()
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
            for file in set.files {
                assert!(file.url.starts_with("https://"));
                assert!(!file.url.contains("/resolve/main/"));
                assert_eq!(file.sha256.len(), 64, "{}", file.file_name);
                assert!(file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(file.size_bytes > 0);
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
        assert!(!tts_model_files_present(TtsModel::OmniVoice));
        // SAFETY: restore the environment before releasing the serialization guard.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("DONTSPEAK_MODEL_DIR", value),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }
    }
}
