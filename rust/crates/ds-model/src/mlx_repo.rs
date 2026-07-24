//! Self-managed MLX model downloads.
//!
//! We fetch every MLX asset with the same HTTP/retry/SHA/atomic-rename/progress path as ONNX.
//! The native shim loads only these local directories. Each set pins an immutable HF commit
//! and every selected file's path, size, and SHA-256 in this repository. `.ds-ready` records
//! the revision after all pinned bytes verify (status polling stays network-free).

use std::path::{Path, PathBuf};

use crate::download::{DEFAULT_RETRIES, ensure_at};
use crate::hash::verify_sha256_cached;
use crate::spec::ModelSpec;

const HF_HOST: &str = "https://huggingface.co";
/// Written into a model dir once every file is present + verified; holds the pinned revision
/// so bumping the pin invalidates a stale tree and forces a re-fetch.
const READY_MARKER: &str = ".ds-ready";

/// One source-pinned file in an MLX repository.
#[derive(Debug, Clone, Copy)]
pub struct MlxFile {
    pub path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// One MLX model set, pinned to an immutable HF revision and static file manifest.
pub struct MlxRepo {
    pub name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub files: &'static [MlxFile],
    /// Directory name under [`ds_config::mlx_dir`]. `target` is the ambient resolution of
    /// exactly this name — `repo_targets_are_their_dir_name_under_the_mlx_root` pins the pair
    /// so [`repo_dir_under`] can resolve a repo against any root.
    pub dir_name: &'static str,
    pub target: fn() -> Option<PathBuf>,
    /// Library-catalog metadata (the macOS-only Apple-Silicon model sets shown in the
    /// Libraries tab). The license lives WITH the files here — same can't-drift principle
    /// as [`crate::urls::Project`] — so `crate::libraries` can render these alongside the
    /// downloaded ONNX assets without a second, drift-prone source. `display_name`/`usage`
    /// empty ⇒ the set is an internal sub-component of another entry (e.g. the Kokoro G2P
    /// sub-models share the Kokoro repo) and is folded into it, not listed on its own.
    pub display_name: &'static str,
    pub usage: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
}

/// On-disk folder name for the MLX Kokoro model and per-voice safetensors.
pub const KOKORO_MLX_DIR_NAME: &str = "kokoro-82m";
pub const CHATTERBOX_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_chatterbox-8bit";
pub const CHATTERBOX_S3_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_S3TokenizerV2";
pub const QWEN_MLX_DIR_NAME: &str = "qwen3-tts-0.6b-customvoice";
pub const OMNIVOICE_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_OmniVoice-bf16";

fn kokoro_main_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(KOKORO_MLX_DIR_NAME))
}

fn chatterbox_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(CHATTERBOX_MLX_DIR_NAME))
}

fn chatterbox_s3_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(CHATTERBOX_S3_MLX_DIR_NAME))
}

fn qwen_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(QWEN_MLX_DIR_NAME))
}

fn omnivoice_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(OMNIVOICE_MLX_DIR_NAME))
}

/// Exact DontSpeak-managed directory passed to the selected MLX TTS loader.
pub fn tts_mlx_dir(model: ds_config::TtsModel) -> Option<PathBuf> {
    match model {
        ds_config::TtsModel::Kokoro => kokoro_main_target(),
        ds_config::TtsModel::Chatterbox => chatterbox_tts_target(),
        ds_config::TtsModel::Qwen => qwen_tts_target(),
        ds_config::TtsModel::OmniVoice => omnivoice_tts_target(),
    }
}

/// Root-relative form of a repo's `target`, for callers that own their model root.
pub fn repo_dir_under(root: &Path, repo: &MlxRepo) -> PathBuf {
    ds_config::mlx_dir_under(root).join(repo.dir_name)
}

pub const PARAKEET_MLX_DIR_NAME: &str = "parakeet";

/// Exact local directory passed to `ParakeetModel.fromDirectory`. Version-less like every
/// other MLX target, so a pin bump re-fetches in place (the ready marker carries the
/// revision) instead of stranding the previous model's tree.
fn parakeet_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(PARAKEET_MLX_DIR_NAME))
}

pub fn parakeet_mlx_dir() -> Option<PathBuf> {
    parakeet_target()
}

pub const DIARIZATION_MLX_DIR_NAME: &str = "sortformer";
pub const SPEAKER_EMBEDDING_DIR_NAME: &str = "wespeaker";

fn diarization_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(DIARIZATION_MLX_DIR_NAME))
}

fn speaker_embedding_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(SPEAKER_EMBEDDING_DIR_NAME))
}

static KOKORO_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 2_351,
        sha256: "5abb01e2403b072bf03d04fde160443e209d7a0dad49a423be15196b9b43c17f",
    },
    MlxFile {
        path: "kokoro-v1_0.safetensors",
        size: 327_115_152,
        sha256: "4e9ecdf03b8b6cf906070390237feda473dc13327cb8d56a43deaa374c02acd8",
    },
    MlxFile {
        path: "voices/af_alloy.safetensors",
        size: 522_320,
        sha256: "5bb848d02ade7e37981809acad52a1761ef7a586ff9f30d02d65fd71c4af95f9",
    },
    MlxFile {
        path: "voices/af_aoede.safetensors",
        size: 522_320,
        sha256: "23809148777f2a2378983dd856bc14b9c261018279f916f98c23d86e844409a5",
    },
    MlxFile {
        path: "voices/af_bella.safetensors",
        size: 522_320,
        sha256: "112d310468cbb3cf23404d3d0b50ad3adf017b87bf38bf9edd15f4ad572df6a3",
    },
    MlxFile {
        path: "voices/af_heart.safetensors",
        size: 522_320,
        sha256: "2c1c733b0e6576c810e268d3e440c21dea4e0f0131a3ba4cfc98d7fe6136d094",
    },
    MlxFile {
        path: "voices/af_jessica.safetensors",
        size: 522_320,
        sha256: "c358448e4277b79e8b13b92033711660a1a2205c3940c2dfb16698b99fed58a8",
    },
    MlxFile {
        path: "voices/af_kore.safetensors",
        size: 522_320,
        sha256: "c491174280cb1ad25210a842f2f34b46a9ef904ec6f6a8e784839531795fa278",
    },
    MlxFile {
        path: "voices/af_nicole.safetensors",
        size: 522_320,
        sha256: "574656386022c81a029e9a72558191925f44c3de2dad2fa2e45751938557d062",
    },
    MlxFile {
        path: "voices/af_nova.safetensors",
        size: 522_320,
        sha256: "242b9a0a01eac1ac2865c69fc617a756b20d86df82d5fae3970533e2312ca50e",
    },
    MlxFile {
        path: "voices/af_river.safetensors",
        size: 522_320,
        sha256: "82c866b0b976d50e82cbd781ac7bc771471ce5bd21decf05ab92812a08fb1c04",
    },
    MlxFile {
        path: "voices/af_sarah.safetensors",
        size: 522_320,
        sha256: "4940072182542f54c1035d1daf4c1cf3136ca9baa9ac57c8e006b4befcc50be6",
    },
    MlxFile {
        path: "voices/af_sky.safetensors",
        size: 522_320,
        sha256: "957af332330db8e9bd7f9dc449475a946cb0d7d689afef64b91007bbbf20eaa0",
    },
    MlxFile {
        path: "voices/am_adam.safetensors",
        size: 522_320,
        sha256: "a4f60a3b9c20353c2604a17485ba53260502a758681a84d41e8af53cc559d929",
    },
    MlxFile {
        path: "voices/am_echo.safetensors",
        size: 522_320,
        sha256: "031fc608a900332c4e1a29bd0884f5d0e84bd0348261fa79981e5cbd138c950d",
    },
    MlxFile {
        path: "voices/am_eric.safetensors",
        size: 522_320,
        sha256: "1fb4a61dcee1f114f90886ecf29bc2feed05e29eed9caa6ddb109f1934d73274",
    },
    MlxFile {
        path: "voices/am_fenrir.safetensors",
        size: 522_320,
        sha256: "9abed964b906c4cae6f404d9849e76260689aea862bc6ca85fc3f5207ba96538",
    },
    MlxFile {
        path: "voices/am_liam.safetensors",
        size: 522_320,
        sha256: "66b65a96e16c3d91035a6e9019d9986ed524d27ce35b487270cdf61c99e3ebad",
    },
    MlxFile {
        path: "voices/am_michael.safetensors",
        size: 522_320,
        sha256: "3940147ded35deba0bb52e8132f89b719298e0520258c34584358aa5a24da2ea",
    },
    MlxFile {
        path: "voices/am_onyx.safetensors",
        size: 522_320,
        sha256: "b5d6132a5747648d98c82c9c4aaa9cf52d7230e63e403c1cb9c12858446ca5f5",
    },
    MlxFile {
        path: "voices/am_puck.safetensors",
        size: 522_320,
        sha256: "9a8c2e56413bd2063f814cb4c3885fc425876157369117c3f8258d03c8a9ad89",
    },
    MlxFile {
        path: "voices/am_santa.safetensors",
        size: 522_320,
        sha256: "d1f433b57ffccf105ea9e434ea19af6c2a8a7916ba6d1a73c34f0046bd226084",
    },
    MlxFile {
        path: "voices/bf_alice.safetensors",
        size: 522_320,
        sha256: "9c77e390d93d9db7c4a7526c3b1f393290a2be46f233b89a00b8188e850c20a8",
    },
    MlxFile {
        path: "voices/bf_emma.safetensors",
        size: 522_320,
        sha256: "8878a75a6661305849eeb1d6293a7177250193616e161b4c3100636434dfe69f",
    },
    MlxFile {
        path: "voices/bf_isabella.safetensors",
        size: 522_320,
        sha256: "f7b6076f025649699fcfed1a6debf13049a87afdc7aafc8c72b7d81246db6ead",
    },
    MlxFile {
        path: "voices/bf_lily.safetensors",
        size: 522_320,
        sha256: "ee77a419046a765420ac82cb46e8b8cf5754a0b9d20c340fece1d4b18be7ecdb",
    },
    MlxFile {
        path: "voices/bm_daniel.safetensors",
        size: 522_320,
        sha256: "b195dec592ee024f57ddc5bf481464596082ba60998a2a295eba90bfc1064f4b",
    },
    MlxFile {
        path: "voices/bm_fable.safetensors",
        size: 522_320,
        sha256: "9fa80184e96d016a744bc13b0b2e7695e55d6b855556fa003325cb1e5ebf2c2b",
    },
    MlxFile {
        path: "voices/bm_george.safetensors",
        size: 522_320,
        sha256: "a3d9b8995cbbe5536f954b6be2a0f1f312f077118ba0d4d2178fc41dc8306672",
    },
    MlxFile {
        path: "voices/bm_lewis.safetensors",
        size: 522_320,
        sha256: "e1e68013c21a141efe527aaec561e1174c2f5a6951b3bcecc8396adab315b247",
    },
    MlxFile {
        path: "voices/ef_dora.safetensors",
        size: 522_320,
        sha256: "13f6dfe8a498ce97a384186af045b586db6292869acbfde123a0fa2798229351",
    },
    MlxFile {
        path: "voices/em_alex.safetensors",
        size: 522_320,
        sha256: "e3bc4bf56ab47f0d52074cd3f84cd4f1713187285fdd85a545c6e167dfa3ab77",
    },
    MlxFile {
        path: "voices/em_santa.safetensors",
        size: 522_320,
        sha256: "37c44211b77b3f29512f420bd5a2e146c7769a5ad3d904b3455cccd55055db62",
    },
    MlxFile {
        path: "voices/ff_siwis.safetensors",
        size: 522_320,
        sha256: "5c659c9b9e12be28b98a4aa0cd6b1e66f359b6381ba5680264e9072945ac32b8",
    },
    MlxFile {
        path: "voices/hf_alpha.safetensors",
        size: 522_320,
        sha256: "e93355a43e6f57e8cfde96874008c858f1fb7fd8b65dd043114d451882cad3f6",
    },
    MlxFile {
        path: "voices/hf_beta.safetensors",
        size: 522_320,
        sha256: "976ea52ba7edce5da049c41ef06a663f3807fd470d2ea5c359245dfc2fb00d66",
    },
    MlxFile {
        path: "voices/hm_omega.safetensors",
        size: 522_320,
        sha256: "227f0c710d1169686bf617fac486e8496982e96cc01617a3acd3579db75dd126",
    },
    MlxFile {
        path: "voices/hm_psi.safetensors",
        size: 522_320,
        sha256: "03efb26b99e78c8d40ade3217f9c9905f8f84bbad7f21f921e270c036b01144e",
    },
    MlxFile {
        path: "voices/if_sara.safetensors",
        size: 522_320,
        sha256: "2f3d092c8ba16f2007e8b234c9a55bdebec614a1e50143e41b39dd7f89fdb45b",
    },
    MlxFile {
        path: "voices/im_nicola.safetensors",
        size: 522_320,
        sha256: "96b62f7d25c3e7efce4f2506beeaa9f63bcc73524c7b2862738c65433fe9ba16",
    },
    MlxFile {
        path: "voices/jf_alpha.safetensors",
        size: 522_320,
        sha256: "455f78a6ebe633929cf314ce7c4a6b595ad1fb0ec7de6de7bc1d62d37e5264d2",
    },
    MlxFile {
        path: "voices/jf_gongitsune.safetensors",
        size: 522_320,
        sha256: "30d744337db7a7a91185b129dfd24ca86c19f7d46acadf2daf077ba78edaba81",
    },
    MlxFile {
        path: "voices/jf_nezumi.safetensors",
        size: 522_320,
        sha256: "65743c88fa1c8d30d7f41e402ce30a6ce461e2b0f8095c252e51905eb0c0754a",
    },
    MlxFile {
        path: "voices/jf_tebukuro.safetensors",
        size: 522_320,
        sha256: "0cc28d928ce14b2ba4586b4c552edba36828a0961a37649530f80b3ad809bdec",
    },
    MlxFile {
        path: "voices/jm_kumo.safetensors",
        size: 522_320,
        sha256: "9f6b9d85ae099c409193924add0f1c478d7c9b6904ef181f2297154bfe05cc2c",
    },
    MlxFile {
        path: "voices/pf_dora.safetensors",
        size: 522_320,
        sha256: "9a8d587d60d0e041f593f7e7488943e7a6821f0136961bf0e554572e12c91c77",
    },
    MlxFile {
        path: "voices/pm_alex.safetensors",
        size: 522_320,
        sha256: "bec864eaeb05cc1a6fa12777ad31faaae1b2ed6d5eb2a6f7370fb9cdc48e3e2f",
    },
    MlxFile {
        path: "voices/pm_santa.safetensors",
        size: 522_320,
        sha256: "5009747fd93841c0865830be0f577ed50800b41b2122c469dedf51bb8311f78d",
    },
    MlxFile {
        path: "voices/zf_xiaobei.safetensors",
        size: 522_320,
        sha256: "cbda378bbe266c735aa13c94c20b6224f2f8d0e16cf3abe612a4e6d93ebeab51",
    },
    MlxFile {
        path: "voices/zf_xiaoni.safetensors",
        size: 522_320,
        sha256: "ef37a82850e10eb15f18a4549c76707a8eebd682e61facdda6ee4a4dc4eb0bf0",
    },
    MlxFile {
        path: "voices/zf_xiaoxiao.safetensors",
        size: 522_320,
        sha256: "cf507ad2319c50121aca4755cd3b9793bde10eea9aa9caca6cb3b5914d5f258f",
    },
    MlxFile {
        path: "voices/zf_xiaoyi.safetensors",
        size: 522_320,
        sha256: "1f2b7ce315a84870170ca83b2e4c0a072242bacbbd869f8a3b22377cc7d59e0b",
    },
    MlxFile {
        path: "voices/zm_yunjian.safetensors",
        size: 522_320,
        sha256: "a08940c5dd3d8aadfda8a5576aa0f688a184ccbd5e4408d7a2a8144ab1fb3040",
    },
    MlxFile {
        path: "voices/zm_yunxi.safetensors",
        size: 522_320,
        sha256: "78d8bb5ba4a2ea75a7f22c6148214a7434b436db85dc791a2ddf2aa7f6cc6fab",
    },
    MlxFile {
        path: "voices/zm_yunxia.safetensors",
        size: 522_320,
        sha256: "59a4ba431ffa7165b95d5b953097affb71110b3039f81c4439cf0f7464bcb2ee",
    },
    MlxFile {
        path: "voices/zm_yunyang.safetensors",
        size: 522_320,
        sha256: "8ad45c1077ab0d973ebb85ebb84f797caf6c6b188255c1178511a6feba3a0611",
    },
];

static CHATTERBOX_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "Cangjie5_TC.json",
        size: 1_920_163,
        sha256: "7073fd9de919443ae88e0bd2449917a65fe54898a4413ed1edcc4b67f28bce8c",
    },
    MlxFile {
        path: "conds.safetensors",
        size: 105_316,
        sha256: "709e5a7fa80e010a011c8244f553853aed7a49c106fff54008fbd89a0f5a6148",
    },
    MlxFile {
        path: "config.json",
        size: 336,
        sha256: "b52886e6c0d2c9f32bda2507c5154742c359eed20b7cacffac1c57aa45328251",
    },
    MlxFile {
        path: "model.safetensors",
        size: 928_163_417,
        sha256: "ca3de1b7592d6c00850e9b81a93b5a130135fa52250c5649671dd1df30a0aab2",
    },
    MlxFile {
        path: "model.safetensors.index.json",
        size: 355_942,
        sha256: "bc865294257de937442a2634b263fc5d0ac8fcd0ca100e25d82c83ae60ed1aea",
    },
    MlxFile {
        path: "tokenizer.json",
        size: 70_011,
        sha256: "df81a7ca7c31796cbe97f7a7142d5a53b12e88e12417ebe98f66602cafaf0461",
    },
];

static CHATTERBOX_S3_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 126,
        sha256: "8591fcc0eaae8c2bbfc69cf9d439933ecdf2d58cb9be63d00ce88736c4f2aa9d",
    },
    MlxFile {
        path: "model.safetensors",
        size: 494_868_984,
        sha256: "928726bc1f206a613d36b8f49e297eae9c5593a21bf9b92ddfe2c23f85eb92cc",
    },
];

static QWEN_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 6_058,
        sha256: "2eea3665564268139c3beb8d497fd3c2e4524e9eed5452836cdf1de96ed3cdbd",
    },
    MlxFile {
        path: "generation_config.json",
        size: 245,
        sha256: "f1b90b4513f3b34c62851049e2492d7b4c5940daf1276f89c82b8ef04127f3aa",
    },
    MlxFile {
        path: "merges.txt",
        size: 1_671_839,
        sha256: "599bab54075088774b1733fde865d5bd747cbcc7a547c5bc12610e874e26f5e3",
    },
    MlxFile {
        path: "model.safetensors",
        size: 1_286_743_170,
        sha256: "3bcb2c4a127e6243e81a30b7126c7865f686d3559de4f938e5d3b150c6a9560d",
    },
    MlxFile {
        path: "model.safetensors.index.json",
        size: 71_447,
        sha256: "0c92041960fa189cf35ae538c8d9ca07c468edddd0c9bb52274c5d4d287a860b",
    },
    MlxFile {
        path: "speech_tokenizer/config.json",
        size: 2_336,
        sha256: "ee65bb901c876664ab8707c487157aa1a6ee57c65969b28fb5ec9dc211e68167",
    },
    MlxFile {
        path: "speech_tokenizer/configuration.json",
        size: 76,
        sha256: "6bc26d64eb5024b4d1dab5a52371958b429256d6c9d59787f1f5294a54e0cebd",
    },
    MlxFile {
        path: "speech_tokenizer/model.safetensors",
        size: 682_293_092,
        sha256: "836b7b357f5ea43e889936a3709af68dfe3751881acefe4ecf0dbd30ba571258",
    },
    MlxFile {
        path: "speech_tokenizer/preprocessor_config.json",
        size: 234,
        sha256: "fcb3805e597e786d4067706e602f6688524640f8d3396790e2e09b5942fcbdfb",
    },
    MlxFile {
        path: "tokenizer_config.json",
        size: 7_344,
        sha256: "dc3c31c3bdaedd5016382bb3cbe07323026775ad51f5a4fb564505992ae4a670",
    },
    MlxFile {
        path: "vocab.json",
        size: 2_776_833,
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    },
];

static OMNIVOICE_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "audio_tokenizer/config.json",
        size: 2_531,
        sha256: "eefb20806f7104e77c9a5277c9df0f9bb8826b08eb1d4e8ab2b9829b6ef9fac1",
    },
    MlxFile {
        path: "audio_tokenizer/model.safetensors",
        size: 402_864_930,
        sha256: "8ef745bfbabeb3bd9ebbdc69e7b6a05e43e191d0208dabfcf7adc42ca89c6580",
    },
    MlxFile {
        path: "audio_tokenizer/preprocessor_config.json",
        size: 206,
        sha256: "ae61eea88558608ee2fa86d2aec9fce8d99a5ff75d09cb7651ccce21ae1d9084",
    },
    MlxFile {
        path: "chat_template.jinja",
        size: 4_168,
        sha256: "a55ee1b1660128b7098723e0abcd92caa0788061051c62d51cbe87d9cf1974d8",
    },
    MlxFile {
        path: "config.json",
        size: 2_238,
        sha256: "e2e13755cca29061b09d0c0c4b945e1a65179de8ec522de18e86794425f86c9f",
    },
    MlxFile {
        path: "model.safetensors",
        size: 1_225_192_351,
        sha256: "5768f3f1d11ee8b3ec31fe906e5d6f5934fdc397e3e5b818de85b70b0e1a2e7e",
    },
    MlxFile {
        path: "tokenizer.json",
        size: 11_423_986,
        sha256: "408f669b7e2b045fdf54201d815bd364e6667dbd845115da81239c40bc6dcfd1",
    },
    MlxFile {
        path: "tokenizer_config.json",
        size: 533,
        sha256: "49f78845596a82bf15c83673794bdf9f76f812b11f60ab6a2239d9be65b00676",
    },
];

static PARAKEET_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 244_093,
        sha256: "f320f1292511f34ec47f513755fe20fd01dbfc09a925d42730e66059a6e1ef4c",
    },
    MlxFile {
        path: "model.safetensors",
        size: 2_508_288_736,
        sha256: "05e01c7f396c298cf7d23f61da7b504adeab698f0aaeafd9c82d198625464592",
    },
    MlxFile {
        path: "tokenizer.model",
        size: 360_916,
        sha256: "eacec2b0a77f336d4a2ca4a25a7047575d3c2b74de47e997f4c205126ed3135e",
    },
    MlxFile {
        path: "tokenizer.vocab",
        size: 101_024,
        sha256: "41130ff456706304a1adec782ccc9e003c4d417e8e324353d281be958cac4e17",
    },
    MlxFile {
        path: "vocab.txt",
        size: 46_772,
        sha256: "3cde1409fd78783a79b29ed4d32da57c746993856f7c8263bcb905d2e5839db7",
    },
];

static DIARIZATION_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 1_702,
        sha256: "17c9f943bed07b0593f2b8dca01e0be6a418053becc6148b01ecabdff9cbd84d",
    },
    MlxFile {
        path: "model.safetensors",
        size: 236_108_132,
        sha256: "3b60b8df29e59a8abaf8061ceeeae6e9284a68fbcd2e762c68f5e058bfceebfa",
    },
];

static SPEAKER_EMBEDDING_MLX_FILES: &[MlxFile] = &[
    MlxFile {
        path: "config.json",
        size: 590,
        sha256: "5e598e1ef04d0c014a59f47d6a7884f26b9203bdefe08d2a5876c7b86cb40b75",
    },
    MlxFile {
        path: "weights.npz",
        size: 26_614_262,
        sha256: "802706880b81ece11a9acefb2cf523ae91473e3b7615858390a1eded4efcdedf",
    },
];

/// MLX Kokoro TTS weights and voice embeddings. Apache-2.0.
pub static KOKORO_MLX: MlxRepo = MlxRepo {
    name: "kokoro_mlx",
    repo: "mlx-community/Kokoro-82M-bf16",
    revision: "a71e4d38b236d968966a2002c4c895dbd12b1c3c",
    files: KOKORO_MLX_FILES,
    dir_name: KOKORO_MLX_DIR_NAME,
    target: kokoro_main_target,
    display_name: "Kokoro (MLX)",
    usage: "Apple-Silicon text-to-speech model and voice embeddings for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Chatterbox Multilingual 8-bit weights and default voice conditioning. Apache-2.0.
pub static CHATTERBOX_MLX: MlxRepo = MlxRepo {
    name: "chatterbox_mlx",
    repo: "mlx-community/chatterbox-8bit",
    revision: "9617d61b596a03d1bed766a28c341680e993a1b9",
    files: CHATTERBOX_MLX_FILES,
    dir_name: CHATTERBOX_MLX_DIR_NAME,
    target: chatterbox_tts_target,
    display_name: "Chatterbox Multilingual (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model and default voice conditioning",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Chatterbox's local S3 speech tokenizer. Apache-2.0; folded into its model set.
pub static CHATTERBOX_S3_MLX: MlxRepo = MlxRepo {
    name: "chatterbox_s3_mlx",
    repo: "mlx-community/S3TokenizerV2",
    revision: "e0c9886f0e1c35ae85b1f27277416fb19fc72bec",
    files: CHATTERBOX_S3_MLX_FILES,
    dir_name: CHATTERBOX_S3_MLX_DIR_NAME,
    target: chatterbox_s3_target,
    display_name: "S3TokenizerV2 (MLX)",
    usage: "Apple-Silicon speech tokenizer required by Chatterbox MLX",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Qwen3-TTS 0.6B CustomVoice weights. Apache-2.0.
pub static QWEN_MLX: MlxRepo = MlxRepo {
    name: "qwen_mlx",
    repo: "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit",
    revision: "049ef77fe8816b536193c0c25f9a214d17921282",
    files: QWEN_MLX_FILES,
    dir_name: QWEN_MLX_DIR_NAME,
    target: qwen_tts_target,
    display_name: "Qwen3-TTS (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX OmniVoice bfloat16 model and Higgs audio tokenizer.
pub static OMNIVOICE_MLX: MlxRepo = MlxRepo {
    name: "omnivoice_mlx",
    repo: "mlx-community/OmniVoice-bf16",
    revision: "8fb0b754cad788aaefec690cd55c207e8a628f85",
    files: OMNIVOICE_MLX_FILES,
    dir_name: OMNIVOICE_MLX_DIR_NAME,
    target: omnivoice_tts_target,
    display_name: "OmniVoice (MLX)",
    usage: "Apple-Silicon omnilingual text-to-speech model and Higgs Audio 2 tokenizer",
    license: "CC-BY-NC / Boson Community License",
    license_url: "https://huggingface.co/k2-fsa/OmniVoice#license",
};

/// MLX Parakeet TDT 0.6b v3 STT — 25 European languages, detected by the model itself.
/// CC-BY-4.0.
pub static PARAKEET_MLX: MlxRepo = MlxRepo {
    name: "parakeet_mlx",
    repo: "mlx-community/parakeet-tdt-0.6b-v3",
    revision: "ed2b7e8c15f9aaa0b5772e2efb986255eaef7e15",
    files: PARAKEET_MLX_FILES,
    dir_name: PARAKEET_MLX_DIR_NAME,
    target: parakeet_target,
    display_name: "Parakeet (MLX)",
    usage: "Apple-Silicon multilingual speech-to-text model (NVIDIA NeMo; MLX conversion)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// MLX Sortformer speaker diarization. The converted repository does not publish
/// SPDX metadata, so the original NVIDIA model terms are linked explicitly in NOTICE.
pub static DIARIZATION_MLX: MlxRepo = MlxRepo {
    name: "diarization_mlx",
    repo: "mlx-community/diar_streaming_sortformer_4spk-v2.1-fp16",
    revision: "e23e6404bd9859e93edbf94a740eb1c7fc58f12e",
    files: DIARIZATION_MLX_FILES,
    dir_name: DIARIZATION_MLX_DIR_NAME,
    target: diarization_target,
    display_name: "Sortformer diarization (MLX)",
    usage: "Apple-Silicon speaker diarization (NVIDIA Sortformer; MLX conversion)",
    license: "NVIDIA Open Model License",
    license_url: "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/",
};

/// MLX WeSpeaker ResNet34 embedding model used for enrollment and speaker identity matching.
pub static SPEAKER_EMBEDDING_MLX: MlxRepo = MlxRepo {
    name: "speaker_embedding_mlx",
    repo: "mlx-community/wespeaker-voxceleb-resnet34-LM",
    revision: "038a61d379b8729c72d64d7c209e0cee80b11d0f",
    files: SPEAKER_EMBEDDING_MLX_FILES,
    dir_name: SPEAKER_EMBEDDING_DIR_NAME,
    target: speaker_embedding_target,
    display_name: "WeSpeaker embedding (MLX)",
    usage: "Apple-Silicon speaker enrollment and identity matching",
    license: "MIT",
    license_url: "https://opensource.org/license/mit",
};

/// The repos one `DownloadTarget::KokoroMlx` fetch produces. ONE source of truth shared by the engine's
/// download manager (fetch + presence gate) and the status row, so they can never disagree
/// about what "the Kokoro MLX set" is.
pub static KOKORO_MLX_SET: [&MlxRepo; 1] = [&KOKORO_MLX];

/// The repos one `DownloadTarget::ChatterboxMlx` fetch produces.
pub static CHATTERBOX_MLX_SET: [&MlxRepo; 2] = [&CHATTERBOX_MLX, &CHATTERBOX_S3_MLX];

/// The repos one `DownloadTarget::QwenMlx` fetch produces.
pub static QWEN_MLX_SET: [&MlxRepo; 1] = [&QWEN_MLX];

/// The repos one `DownloadTarget::OmniVoiceMlx` fetch produces.
pub static OMNIVOICE_MLX_SET: [&MlxRepo; 1] = [&OMNIVOICE_MLX];

/// Complete pinned asset set for one built-in MLX TTS model.
pub fn tts_mlx_set(model: ds_config::TtsModel) -> &'static [&'static MlxRepo] {
    match model {
        ds_config::TtsModel::Kokoro => &KOKORO_MLX_SET,
        ds_config::TtsModel::Chatterbox => &CHATTERBOX_MLX_SET,
        ds_config::TtsModel::Qwen => &QWEN_MLX_SET,
        ds_config::TtsModel::OmniVoice => &OMNIVOICE_MLX_SET,
    }
}

/// The repos one `DownloadTarget::ParakeetMlx` fetch produces.
pub static PARAKEET_MLX_SET: [&MlxRepo; 1] = [&PARAKEET_MLX];

/// Sortformer segmentation plus WeSpeaker embeddings are one user-visible diarization download.
pub static DIARIZATION_MLX_SET: [&MlxRepo; 2] = [&DIARIZATION_MLX, &SPEAKER_EMBEDDING_MLX];

/// LOCAL presence of a whole set (no network): every repo's completion marker present at the
/// pinned revision — see [`is_mlx_repo_present`].
pub fn is_mlx_set_present(set: &[&MlxRepo]) -> bool {
    set.iter().all(|r| is_mlx_repo_present(r))
}

/// Every MLX repo we self-manage, in the order a clean install fetches them.
pub fn all_mlx_repos() -> [&'static MlxRepo; 8] {
    [
        &KOKORO_MLX,
        &CHATTERBOX_MLX,
        &CHATTERBOX_S3_MLX,
        &QWEN_MLX,
        &OMNIVOICE_MLX,
        &PARAKEET_MLX,
        &DIARIZATION_MLX,
        &SPEAKER_EMBEDDING_MLX,
    ]
}

fn already_have(dest: &Path, file: &MlxFile) -> bool {
    verify_sha256_cached(dest, file.sha256)
}

fn download_one_at(
    url: &str,
    file: &MlxFile,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let spec = ModelSpec {
        file_name: file.path.to_string(),
        url: url.to_string(),
        sha256: file.sha256.to_string(),
    };
    ensure_at(dest, &spec, DEFAULT_RETRIES, progress)
}

/// Download a SET of repos as one unit, reporting ONE overall byte-weighted bar:
/// `progress(done_bytes, total_bytes)` where BOTH are summed across every file of every repo —
/// so the UI shows a single monotonic "Downloading `<pct>`%" over the WHOLE set (a true global
/// percent, not a per-file percent that resets each file). Writes each repo's completion marker
/// once its own files verify — per repo, so a failed member keeps completed repos' markers.
/// Missing files fetch concurrently (bounded pool).
pub fn ensure_mlx_repos(repos: &[&MlxRepo], progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    ensure_mlx_repos_at(HF_HOST, repos, progress)
}

/// Same as [`ensure_mlx_repos`] but resolve URLs are rooted at `host` (production:
/// [`HF_HOST`]; tests: httpmock base URL).
pub(crate) fn ensure_mlx_repos_at(
    host: &str,
    repos: &[&MlxRepo],
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let mut plan: Vec<(&MlxRepo, PathBuf)> = Vec::new();
    for r in repos {
        if is_mlx_repo_present(r) {
            continue;
        }
        if r.files.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("MLX manifest for {} is empty", r.name),
            ));
        }
        let target = (r.target)().ok_or_else(|| {
            std::io::Error::other(format!("cannot resolve target dir for {}", r.name))
        })?;
        plan.push((r, target));
    }
    let total_bytes: u64 = plan
        .iter()
        .flat_map(|(repo, _)| repo.files)
        .map(|file| file.size)
        .sum();
    if total_bytes == 0 {
        progress(1, 1);
        return Ok(());
    }

    // One directory flight per planned repo dir for the whole run — the granularity a
    // `models remove` deletes at, so the two exclude each other instead of interleaving
    // per file. The pre-check above still runs outside the flight: a removal landing in
    // that window makes a concurrent cross-process `ds-helper --prefetch` skip a repo it
    // believed present, which the engine's next `compute_needs` probe re-fetches.
    let dirs: Vec<PathBuf> = plan.iter().map(|(_, target)| target.clone()).collect();
    crate::download::with_destination_flights(&dirs, || {
        ensure_mlx_repos_locked(host, &plan, total_bytes, progress)
    })
}

fn ensure_mlx_repos_locked(
    host: &str,
    plan: &[(&MlxRepo, PathBuf)],
    total_bytes: u64,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let mut pre_credit: u64 = 0;
    let mut jobs: Vec<crate::parallel::DownloadJob> = Vec::new();
    for (repo, target) in plan {
        for file in repo.files {
            let relative = Path::new(file.path);
            if relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsafe path in MLX manifest: {}", file.path),
                ));
            }
            let dest = target.join(relative);
            let size = file.size;
            if already_have(&dest, file) {
                pre_credit = pre_credit.saturating_add(size);
                continue;
            }
            let host = host.to_string();
            let repo_id = repo.repo;
            let revision = repo.revision;
            let file = *file;
            jobs.push(Box::new(move |p| {
                // Local progress is file bytes; pool sums high-waters + pre_credit.
                let emit = |done: u64, _t: u64| p(done.min(size), size);
                let url = format!("{host}/{repo_id}/resolve/{revision}/{}", file.path);
                download_one_at(&url, &file, &dest, &emit)
            }));
        }
    }

    let pool_result = crate::parallel::run_jobs_parallel(
        progress,
        total_bytes,
        pre_credit.min(total_bytes),
        jobs,
    );

    // Per-repo marker atomicity: write each repo's marker as soon as ALL of its own files verify,
    // independent of sibling repos in the same set. A failed sibling no longer discards a completed
    // repo's marker, so the next retry skips the finished repo. A repo with any invalid file gets
    // no marker and self-repairs on the next run.
    for (repo, target) in plan {
        if let Some(missing) = repo
            .files
            .iter()
            .find(|file| !already_have(&target.join(Path::new(file.path)), file))
        {
            // After a *successful* pool run every file must be present; a gap is a real bug, not
            // the partial-failure case (which already carries the pool's own error).
            if pool_result.is_ok() {
                return Err(std::io::Error::other(format!(
                    "missing verified file after download: {}",
                    missing.path
                )));
            }
            continue;
        }
        std::fs::write(target.join(READY_MARKER), repo.revision)?;
    }

    pool_result
}

/// Completion marker in `dir` carries `repo`'s pinned revision. Existence-only half of
/// [`is_mlx_repo_present`], for the root-parameterized inventory probe.
pub(crate) fn ready_marker_matches(dir: &Path, repo: &MlxRepo) -> bool {
    std::fs::read_to_string(dir.join(READY_MARKER))
        .is_ok_and(|marker| marker.trim() == repo.revision)
}

/// LOCAL presence (no network): marker revision matches and every source-pinned file verifies.
pub fn is_mlx_repo_present(repo: &MlxRepo) -> bool {
    let Some(target) = (repo.target)() else {
        return false;
    };
    std::fs::read_to_string(target.join(READY_MARKER))
        .is_ok_and(|marker| marker.trim() == repo.revision)
        && !repo.files.is_empty()
        && repo.files.iter().all(|file| {
            let relative = Path::new(file.path);
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                && verify_sha256_cached(&target.join(relative), file.sha256)
        })
}

// `target` is `fn() -> Option<PathBuf>` (no captures). Thread-local seam for presence tests.
#[cfg(test)]
thread_local! {
    static TEST_TARGET_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    // Second seam so a multi-repo set test can give each repo its own dir/marker.
    static TEST_TARGET_DIR_2: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_target() -> Option<PathBuf> {
    TEST_TARGET_DIR.with(|t| t.borrow().clone())
}

#[cfg(test)]
fn test_target_2() -> Option<PathBuf> {
    TEST_TARGET_DIR_2.with(|t| t.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fixture_file(path: &'static str, content: &'static [u8]) -> MlxFile {
        MlxFile {
            path,
            size: content.len() as u64,
            sha256: Box::leak(crate::hash::sha256_hex(content).into_boxed_str()),
        }
    }

    fn fixture_files(files: Vec<MlxFile>) -> &'static [MlxFile] {
        Box::leak(files.into_boxed_slice())
    }

    fn fixture_repo(
        name: &'static str,
        repo: &'static str,
        revision: &'static str,
        files: &'static [MlxFile],
        target: fn() -> Option<PathBuf>,
    ) -> MlxRepo {
        MlxRepo {
            name,
            repo,
            revision,
            files,
            // Fixtures resolve through the injected `target`, never through a model root.
            dir_name: name,
            target,
            display_name: "",
            usage: "",
            license: "",
            license_url: "",
        }
    }

    #[test]
    fn model_sets_have_the_expected_components() {
        assert_eq!(KOKORO_MLX_SET.len(), 1);
        assert_eq!(CHATTERBOX_MLX_SET.len(), 2);
        assert_eq!(OMNIVOICE_MLX_SET.len(), 1);
        assert_eq!(PARAKEET_MLX_SET.len(), 1);
        assert_eq!(DIARIZATION_MLX_SET.len(), 2);
        assert!(DIARIZATION_MLX.repo.contains("sortformer"));
        assert!(SPEAKER_EMBEDDING_MLX.repo.contains("wespeaker"));
    }

    /// Pure path math (no FS): every production repo's ambient `target` must be exactly its
    /// `dir_name` under the MLX root, or [`repo_dir_under`] resolves a different directory
    /// than the downloader writes.
    #[test]
    fn repo_targets_are_their_dir_name_under_the_mlx_root() {
        for repo in all_mlx_repos() {
            assert_eq!(
                (repo.target)(),
                ds_config::mlx_dir().map(|dir| dir.join(repo.dir_name)),
                "{}",
                repo.name
            );
            assert!(!repo.dir_name.is_empty(), "{}", repo.name);
        }
    }

    #[test]
    fn native_shim_loads_only_rust_managed_local_directories() {
        let shim =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift");
        for forbidden in ["ModelHub", "snapshotDownload", "downloadModel"] {
            assert!(
                !shim.contains(forbidden),
                "native model download API: {forbidden}"
            );
        }
        assert_eq!(
            shim.matches("OmniVoiceModel.fromPretrained").count(),
            1,
            "OmniVoice's repository-only API must remain the sole native repository load"
        );
        assert!(shim.contains("configureManagedHubCache"));
        for required in ["fromModelDirectory", "fromDirectory"] {
            assert!(
                shim.contains(required),
                "missing local model load: {required}"
            );
        }
    }

    #[test]
    fn manifests_are_complete_unique_and_sha256_pinned() {
        let repos = all_mlx_repos();
        let counts: Vec<usize> = repos.iter().map(|repo| repo.files.len()).collect();
        assert_eq!(counts, vec![56, 6, 2, 11, 8, 5, 2, 2]);
        assert_eq!(counts.iter().sum::<usize>(), 92);
        assert_eq!(
            repos
                .iter()
                .flat_map(|repo| repo.files)
                .map(|file| file.size)
                .sum::<u64>(),
            8_165_636_922
        );

        for repo in repos {
            assert_eq!(repo.revision.len(), 40, "{} revision", repo.name);
            assert!(
                repo.revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
            assert!(repo.repo.starts_with("mlx-community/"));
            assert!(!repo.files.is_empty());

            let mut paths = HashSet::new();
            for file in repo.files {
                let relative = Path::new(file.path);
                assert!(
                    relative
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_))),
                    "{} has unsafe path {}",
                    repo.name,
                    file.path
                );
                assert!(paths.insert(file.path), "duplicate path {}", file.path);
                assert!(file.size > 0, "{} has zero size", file.path);
                assert_eq!(file.sha256.len(), 64, "{} digest", file.path);
                assert!(
                    file.sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                    "{} digest is not lowercase hex",
                    file.path
                );
            }
        }
    }

    #[test]
    fn presence_requires_matching_marker_and_every_pinned_digest() {
        const CONTENT: &[u8] = b"verified weights";
        let tmp = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = Some(tmp.path().to_path_buf()));
        let files = fixture_files(vec![fixture_file("model.safetensors", CONTENT)]);
        let repo = fixture_repo(
            "presence",
            "test-org/presence",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            files,
            test_target,
        );

        assert!(!is_mlx_repo_present(&repo));
        std::fs::write(tmp.path().join(READY_MARKER), "different-revision").unwrap();
        assert!(!is_mlx_repo_present(&repo));

        std::fs::write(tmp.path().join(READY_MARKER), repo.revision).unwrap();
        assert!(!is_mlx_repo_present(&repo), "pinned file is still missing");

        let model = tmp.path().join("model.safetensors");
        std::fs::write(&model, CONTENT).unwrap();
        assert!(is_mlx_repo_present(&repo));

        // Different length so file identity (size + mtime) invalidates the SHA
        // cache even on filesystems with coarse mtime resolution.
        std::fs::write(&model, b"tampered").unwrap();
        assert!(
            !is_mlx_repo_present(&repo),
            "matching marker cannot bless changed bytes"
        );
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = None);
    }

    #[test]
    fn download_one_at_persists_only_bytes_matching_the_static_pin() {
        const CONTENT: &[u8] = b"fake model weights";
        let server = httpmock::MockServer::start();
        let good = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/good.bin");
            then.status(200).body(CONTENT);
        });
        let bad = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/bad.bin");
            then.status(200).body(b"tampered bytes");
        });

        let dir = tempfile::tempdir().unwrap();
        let file = fixture_file("model.bin", CONTENT);
        let good_dest = dir.path().join("good.bin");
        download_one_at(&server.url("/good.bin"), &file, &good_dest, &|_, _| {})
            .expect("matching bytes land");
        good.assert();
        assert_eq!(std::fs::read(&good_dest).unwrap(), CONTENT);

        let bad_dest = dir.path().join("bad.bin");
        let error =
            download_one_at(&server.url("/bad.bin"), &file, &bad_dest, &|_, _| {}).unwrap_err();
        bad.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!bad_dest.exists());
    }

    #[test]
    fn orchestrator_uses_static_paths_precredits_and_writes_marker() {
        const A: &[u8] = b"already present";
        const B: &[u8] = b"download me";
        const REV: &str = "abc1230000000000000000000000000000000000";
        const REPO_ID: &str = "test-org/static-model";
        let server = httpmock::MockServer::start();
        let blob_a = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/a.bin"));
            then.status(200).body(A);
        });
        let blob_b = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/nested/b.bin"));
            then.status(200).body(B);
        });

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), A).unwrap();
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = Some(tmp.path().to_path_buf()));
        let files = fixture_files(vec![
            fixture_file("a.bin", A),
            fixture_file("nested/b.bin", B),
        ]);
        let repo = fixture_repo("static", REPO_ID, REV, files, test_target);
        let progress = std::sync::Mutex::new(Vec::new());

        ensure_mlx_repos_at(&server.base_url(), &[&repo], &|done, total| {
            assert_eq!(total, (A.len() + B.len()) as u64);
            progress.lock().unwrap().push(done);
        })
        .expect("static manifest downloads");

        assert_eq!(blob_a.calls(), 0, "verified file is precredited");
        blob_b.assert();
        assert_eq!(std::fs::read(tmp.path().join("nested/b.bin")).unwrap(), B);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(READY_MARKER)).unwrap(),
            REV
        );
        assert!(is_mlx_repo_present(&repo));
        let progress = progress.lock().unwrap();
        assert!(progress.windows(2).all(|values| values[1] >= values[0]));
        assert_eq!(*progress.last().unwrap(), (A.len() + B.len()) as u64);
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = None);
    }

    #[test]
    fn failed_file_does_not_write_a_ready_marker() {
        const GOOD: &[u8] = b"good bytes";
        const BAD_EXPECTED: &[u8] = b"expected bytes";
        const REV: &str = "abc1240000000000000000000000000000000000";
        const REPO_ID: &str = "test-org/failing-model";
        let server = httpmock::MockServer::start();
        let _good = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/good.bin"));
            then.status(200).body(GOOD);
        });
        let bad = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/bad.bin"));
            then.status(200).body(b"wrong bytes");
        });

        let tmp = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = Some(tmp.path().to_path_buf()));
        let files = fixture_files(vec![
            fixture_file("good.bin", GOOD),
            fixture_file("bad.bin", BAD_EXPECTED),
        ]);
        let repo = fixture_repo("failure", REPO_ID, REV, files, test_target);

        let error = ensure_mlx_repos_at(&server.base_url(), &[&repo], &|_, _| {}).unwrap_err();
        bad.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!tmp.path().join(READY_MARKER).exists());
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = None);
    }

    /// #208 class: the set installer holds a flight over every planned repo dir for its whole
    /// run, so a `models remove` of the same model cannot unlink a repo between two of its
    /// files. The fixture repo is pointed at the directory `remove_at` derives for Kokoro's
    /// MLX set, and the rendezvous sits in the aggregate progress callback — inside the flight.
    #[test]
    fn a_repo_set_install_blocks_a_concurrent_removal_of_the_same_model() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const BODY: &[u8] = b"freshly downloaded mlx weights";
        const REV: &str = "abc1270000000000000000000000000000000000";
        const REPO_ID: &str = "test-org/kokoro-mlx";
        let server = httpmock::MockServer::start();
        let blob = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/weights.safetensors"));
            then.status(200).body(BODY);
        });

        let root = tempfile::tempdir().unwrap();
        let target = repo_dir_under(root.path(), &KOKORO_MLX);
        assert!(
            crate::download::sweep_root_of(&target)
                .is_some_and(|resolved| resolved.starts_with(root.path())),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );
        let files = fixture_files(vec![fixture_file("weights.safetensors", BODY)]);
        let repo: &'static MlxRepo = Box::leak(Box::new(fixture_repo(
            "kokoro-mlx",
            REPO_ID,
            REV,
            files,
            test_target,
        )));

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let base = server.base_url();
        let install_target = target.clone();
        let installer = std::thread::spawn(move || {
            TEST_TARGET_DIR.with(|slot| *slot.borrow_mut() = Some(install_target));
            let announced = AtomicBool::new(false);
            ensure_mlx_repos_at(&base, &[repo], &|_done, _total| {
                if !announced.swap(true, Ordering::SeqCst) {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect("main thread releases the install");
                }
            })
            .expect("the fixture set downloads");
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the installer enters the set flight");

        let removal_root = root.path().to_path_buf();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            removed_tx
                .send(
                    crate::inventory::remove_at(
                        &removal_root,
                        &ds_config::VoiceConfig::default(),
                        "kokoro",
                    )
                    .unwrap(),
                )
                .unwrap();
        });
        assert!(
            removed_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "removal must not delete a repo dir the set installer is writing"
        );

        release_tx.send(()).unwrap();
        installer.join().unwrap();
        removed_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the removal completes once the install releases");
        remover.join().unwrap();
        blob.assert();
        assert!(!target.exists());
    }

    #[test]
    fn completed_repo_keeps_its_marker_when_a_sibling_fails() {
        const GOOD: &[u8] = b"good repository";
        const BAD_EXPECTED: &[u8] = b"expected sibling";
        const GOOD_REV: &str = "abc1250000000000000000000000000000000000";
        const BAD_REV: &str = "abc1260000000000000000000000000000000000";
        const GOOD_ID: &str = "test-org/good";
        const BAD_ID: &str = "test-org/bad";
        let server = httpmock::MockServer::start();
        let good_blob = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{GOOD_ID}/resolve/{GOOD_REV}/good.bin"));
            then.status(200).body(GOOD);
        });
        let bad_blob = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{BAD_ID}/resolve/{BAD_REV}/bad.bin"));
            then.status(200).body(b"wrong sibling");
        });

        let good_dir = tempfile::tempdir().unwrap();
        let bad_dir = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = Some(good_dir.path().to_path_buf()));
        TEST_TARGET_DIR_2.with(|target| *target.borrow_mut() = Some(bad_dir.path().to_path_buf()));
        let good_files = fixture_files(vec![fixture_file("good.bin", GOOD)]);
        let bad_files = fixture_files(vec![fixture_file("bad.bin", BAD_EXPECTED)]);
        let good_repo = fixture_repo("good", GOOD_ID, GOOD_REV, good_files, test_target);
        let bad_repo = fixture_repo("bad", BAD_ID, BAD_REV, bad_files, test_target_2);

        let error = ensure_mlx_repos_at(&server.base_url(), &[&good_repo, &bad_repo], &|_, _| {})
            .unwrap_err();

        good_blob.assert();
        bad_blob.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(good_dir.path().join(READY_MARKER)).unwrap(),
            GOOD_REV
        );
        assert!(!bad_dir.path().join(READY_MARKER).exists());
        TEST_TARGET_DIR.with(|target| *target.borrow_mut() = None);
        TEST_TARGET_DIR_2.with(|target| *target.borrow_mut() = None);
    }
}
