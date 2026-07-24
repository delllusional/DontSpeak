//! Pinned MLX Audio model manifests.
//!
//! Manifests only: the shape, the roots, the transfer and the presence probe live in
//! [`crate::hf_repo`], which every self-managed Hugging Face set shares. Each set here pins
//! an immutable HF commit and every selected file's path, size, and SHA-256; the native shim
//! loads only these local directories.

use std::path::PathBuf;

use crate::hf_repo::{HfFile, HfRepo, RepoRoot, repo_dir};

/// On-disk folder name for the MLX Kokoro model and per-voice safetensors.
pub const KOKORO_MLX_DIR_NAME: &str = "kokoro-82m";
pub const CHATTERBOX_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_chatterbox-8bit";
pub const CHATTERBOX_S3_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_S3TokenizerV2";
pub const QWEN_MLX_DIR_NAME: &str = "qwen3-tts-0.6b-customvoice";
pub const OMNIVOICE_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_OmniVoice-bf16";

/// Exact DontSpeak-managed directory passed to the selected MLX TTS loader. Ambient form of
/// [`crate::hf_repo::ModelRoots::dir_for`] — derived, never a second resolution.
pub fn tts_mlx_dir(model: ds_config::TtsModel) -> Option<PathBuf> {
    repo_dir(match model {
        ds_config::TtsModel::Kokoro => &KOKORO_MLX,
        ds_config::TtsModel::Chatterbox => &CHATTERBOX_MLX,
        ds_config::TtsModel::Qwen => &QWEN_MLX,
        ds_config::TtsModel::OmniVoice => &OMNIVOICE_MLX,
    })
}

/// Exact local directory passed to `ParakeetModel.fromDirectory`. Version-less like every
/// other MLX target, so a pin bump re-fetches in place (the ready marker carries the
/// revision) instead of stranding the previous model's tree.
pub const PARAKEET_MLX_DIR_NAME: &str = "parakeet";

pub fn parakeet_mlx_dir() -> Option<PathBuf> {
    repo_dir(&PARAKEET_MLX)
}

pub const DIARIZATION_MLX_DIR_NAME: &str = "sortformer";
pub const SPEAKER_EMBEDDING_DIR_NAME: &str = "wespeaker";

static KOKORO_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 2_351,
        sha256: "5abb01e2403b072bf03d04fde160443e209d7a0dad49a423be15196b9b43c17f",
    },
    HfFile {
        path: "kokoro-v1_0.safetensors",
        size: 327_115_152,
        sha256: "4e9ecdf03b8b6cf906070390237feda473dc13327cb8d56a43deaa374c02acd8",
    },
    HfFile {
        path: "voices/af_alloy.safetensors",
        size: 522_320,
        sha256: "5bb848d02ade7e37981809acad52a1761ef7a586ff9f30d02d65fd71c4af95f9",
    },
    HfFile {
        path: "voices/af_aoede.safetensors",
        size: 522_320,
        sha256: "23809148777f2a2378983dd856bc14b9c261018279f916f98c23d86e844409a5",
    },
    HfFile {
        path: "voices/af_bella.safetensors",
        size: 522_320,
        sha256: "112d310468cbb3cf23404d3d0b50ad3adf017b87bf38bf9edd15f4ad572df6a3",
    },
    HfFile {
        path: "voices/af_heart.safetensors",
        size: 522_320,
        sha256: "2c1c733b0e6576c810e268d3e440c21dea4e0f0131a3ba4cfc98d7fe6136d094",
    },
    HfFile {
        path: "voices/af_jessica.safetensors",
        size: 522_320,
        sha256: "c358448e4277b79e8b13b92033711660a1a2205c3940c2dfb16698b99fed58a8",
    },
    HfFile {
        path: "voices/af_kore.safetensors",
        size: 522_320,
        sha256: "c491174280cb1ad25210a842f2f34b46a9ef904ec6f6a8e784839531795fa278",
    },
    HfFile {
        path: "voices/af_nicole.safetensors",
        size: 522_320,
        sha256: "574656386022c81a029e9a72558191925f44c3de2dad2fa2e45751938557d062",
    },
    HfFile {
        path: "voices/af_nova.safetensors",
        size: 522_320,
        sha256: "242b9a0a01eac1ac2865c69fc617a756b20d86df82d5fae3970533e2312ca50e",
    },
    HfFile {
        path: "voices/af_river.safetensors",
        size: 522_320,
        sha256: "82c866b0b976d50e82cbd781ac7bc771471ce5bd21decf05ab92812a08fb1c04",
    },
    HfFile {
        path: "voices/af_sarah.safetensors",
        size: 522_320,
        sha256: "4940072182542f54c1035d1daf4c1cf3136ca9baa9ac57c8e006b4befcc50be6",
    },
    HfFile {
        path: "voices/af_sky.safetensors",
        size: 522_320,
        sha256: "957af332330db8e9bd7f9dc449475a946cb0d7d689afef64b91007bbbf20eaa0",
    },
    HfFile {
        path: "voices/am_adam.safetensors",
        size: 522_320,
        sha256: "a4f60a3b9c20353c2604a17485ba53260502a758681a84d41e8af53cc559d929",
    },
    HfFile {
        path: "voices/am_echo.safetensors",
        size: 522_320,
        sha256: "031fc608a900332c4e1a29bd0884f5d0e84bd0348261fa79981e5cbd138c950d",
    },
    HfFile {
        path: "voices/am_eric.safetensors",
        size: 522_320,
        sha256: "1fb4a61dcee1f114f90886ecf29bc2feed05e29eed9caa6ddb109f1934d73274",
    },
    HfFile {
        path: "voices/am_fenrir.safetensors",
        size: 522_320,
        sha256: "9abed964b906c4cae6f404d9849e76260689aea862bc6ca85fc3f5207ba96538",
    },
    HfFile {
        path: "voices/am_liam.safetensors",
        size: 522_320,
        sha256: "66b65a96e16c3d91035a6e9019d9986ed524d27ce35b487270cdf61c99e3ebad",
    },
    HfFile {
        path: "voices/am_michael.safetensors",
        size: 522_320,
        sha256: "3940147ded35deba0bb52e8132f89b719298e0520258c34584358aa5a24da2ea",
    },
    HfFile {
        path: "voices/am_onyx.safetensors",
        size: 522_320,
        sha256: "b5d6132a5747648d98c82c9c4aaa9cf52d7230e63e403c1cb9c12858446ca5f5",
    },
    HfFile {
        path: "voices/am_puck.safetensors",
        size: 522_320,
        sha256: "9a8c2e56413bd2063f814cb4c3885fc425876157369117c3f8258d03c8a9ad89",
    },
    HfFile {
        path: "voices/am_santa.safetensors",
        size: 522_320,
        sha256: "d1f433b57ffccf105ea9e434ea19af6c2a8a7916ba6d1a73c34f0046bd226084",
    },
    HfFile {
        path: "voices/bf_alice.safetensors",
        size: 522_320,
        sha256: "9c77e390d93d9db7c4a7526c3b1f393290a2be46f233b89a00b8188e850c20a8",
    },
    HfFile {
        path: "voices/bf_emma.safetensors",
        size: 522_320,
        sha256: "8878a75a6661305849eeb1d6293a7177250193616e161b4c3100636434dfe69f",
    },
    HfFile {
        path: "voices/bf_isabella.safetensors",
        size: 522_320,
        sha256: "f7b6076f025649699fcfed1a6debf13049a87afdc7aafc8c72b7d81246db6ead",
    },
    HfFile {
        path: "voices/bf_lily.safetensors",
        size: 522_320,
        sha256: "ee77a419046a765420ac82cb46e8b8cf5754a0b9d20c340fece1d4b18be7ecdb",
    },
    HfFile {
        path: "voices/bm_daniel.safetensors",
        size: 522_320,
        sha256: "b195dec592ee024f57ddc5bf481464596082ba60998a2a295eba90bfc1064f4b",
    },
    HfFile {
        path: "voices/bm_fable.safetensors",
        size: 522_320,
        sha256: "9fa80184e96d016a744bc13b0b2e7695e55d6b855556fa003325cb1e5ebf2c2b",
    },
    HfFile {
        path: "voices/bm_george.safetensors",
        size: 522_320,
        sha256: "a3d9b8995cbbe5536f954b6be2a0f1f312f077118ba0d4d2178fc41dc8306672",
    },
    HfFile {
        path: "voices/bm_lewis.safetensors",
        size: 522_320,
        sha256: "e1e68013c21a141efe527aaec561e1174c2f5a6951b3bcecc8396adab315b247",
    },
    HfFile {
        path: "voices/ef_dora.safetensors",
        size: 522_320,
        sha256: "13f6dfe8a498ce97a384186af045b586db6292869acbfde123a0fa2798229351",
    },
    HfFile {
        path: "voices/em_alex.safetensors",
        size: 522_320,
        sha256: "e3bc4bf56ab47f0d52074cd3f84cd4f1713187285fdd85a545c6e167dfa3ab77",
    },
    HfFile {
        path: "voices/em_santa.safetensors",
        size: 522_320,
        sha256: "37c44211b77b3f29512f420bd5a2e146c7769a5ad3d904b3455cccd55055db62",
    },
    HfFile {
        path: "voices/ff_siwis.safetensors",
        size: 522_320,
        sha256: "5c659c9b9e12be28b98a4aa0cd6b1e66f359b6381ba5680264e9072945ac32b8",
    },
    HfFile {
        path: "voices/hf_alpha.safetensors",
        size: 522_320,
        sha256: "e93355a43e6f57e8cfde96874008c858f1fb7fd8b65dd043114d451882cad3f6",
    },
    HfFile {
        path: "voices/hf_beta.safetensors",
        size: 522_320,
        sha256: "976ea52ba7edce5da049c41ef06a663f3807fd470d2ea5c359245dfc2fb00d66",
    },
    HfFile {
        path: "voices/hm_omega.safetensors",
        size: 522_320,
        sha256: "227f0c710d1169686bf617fac486e8496982e96cc01617a3acd3579db75dd126",
    },
    HfFile {
        path: "voices/hm_psi.safetensors",
        size: 522_320,
        sha256: "03efb26b99e78c8d40ade3217f9c9905f8f84bbad7f21f921e270c036b01144e",
    },
    HfFile {
        path: "voices/if_sara.safetensors",
        size: 522_320,
        sha256: "2f3d092c8ba16f2007e8b234c9a55bdebec614a1e50143e41b39dd7f89fdb45b",
    },
    HfFile {
        path: "voices/im_nicola.safetensors",
        size: 522_320,
        sha256: "96b62f7d25c3e7efce4f2506beeaa9f63bcc73524c7b2862738c65433fe9ba16",
    },
    HfFile {
        path: "voices/jf_alpha.safetensors",
        size: 522_320,
        sha256: "455f78a6ebe633929cf314ce7c4a6b595ad1fb0ec7de6de7bc1d62d37e5264d2",
    },
    HfFile {
        path: "voices/jf_gongitsune.safetensors",
        size: 522_320,
        sha256: "30d744337db7a7a91185b129dfd24ca86c19f7d46acadf2daf077ba78edaba81",
    },
    HfFile {
        path: "voices/jf_nezumi.safetensors",
        size: 522_320,
        sha256: "65743c88fa1c8d30d7f41e402ce30a6ce461e2b0f8095c252e51905eb0c0754a",
    },
    HfFile {
        path: "voices/jf_tebukuro.safetensors",
        size: 522_320,
        sha256: "0cc28d928ce14b2ba4586b4c552edba36828a0961a37649530f80b3ad809bdec",
    },
    HfFile {
        path: "voices/jm_kumo.safetensors",
        size: 522_320,
        sha256: "9f6b9d85ae099c409193924add0f1c478d7c9b6904ef181f2297154bfe05cc2c",
    },
    HfFile {
        path: "voices/pf_dora.safetensors",
        size: 522_320,
        sha256: "9a8d587d60d0e041f593f7e7488943e7a6821f0136961bf0e554572e12c91c77",
    },
    HfFile {
        path: "voices/pm_alex.safetensors",
        size: 522_320,
        sha256: "bec864eaeb05cc1a6fa12777ad31faaae1b2ed6d5eb2a6f7370fb9cdc48e3e2f",
    },
    HfFile {
        path: "voices/pm_santa.safetensors",
        size: 522_320,
        sha256: "5009747fd93841c0865830be0f577ed50800b41b2122c469dedf51bb8311f78d",
    },
    HfFile {
        path: "voices/zf_xiaobei.safetensors",
        size: 522_320,
        sha256: "cbda378bbe266c735aa13c94c20b6224f2f8d0e16cf3abe612a4e6d93ebeab51",
    },
    HfFile {
        path: "voices/zf_xiaoni.safetensors",
        size: 522_320,
        sha256: "ef37a82850e10eb15f18a4549c76707a8eebd682e61facdda6ee4a4dc4eb0bf0",
    },
    HfFile {
        path: "voices/zf_xiaoxiao.safetensors",
        size: 522_320,
        sha256: "cf507ad2319c50121aca4755cd3b9793bde10eea9aa9caca6cb3b5914d5f258f",
    },
    HfFile {
        path: "voices/zf_xiaoyi.safetensors",
        size: 522_320,
        sha256: "1f2b7ce315a84870170ca83b2e4c0a072242bacbbd869f8a3b22377cc7d59e0b",
    },
    HfFile {
        path: "voices/zm_yunjian.safetensors",
        size: 522_320,
        sha256: "a08940c5dd3d8aadfda8a5576aa0f688a184ccbd5e4408d7a2a8144ab1fb3040",
    },
    HfFile {
        path: "voices/zm_yunxi.safetensors",
        size: 522_320,
        sha256: "78d8bb5ba4a2ea75a7f22c6148214a7434b436db85dc791a2ddf2aa7f6cc6fab",
    },
    HfFile {
        path: "voices/zm_yunxia.safetensors",
        size: 522_320,
        sha256: "59a4ba431ffa7165b95d5b953097affb71110b3039f81c4439cf0f7464bcb2ee",
    },
    HfFile {
        path: "voices/zm_yunyang.safetensors",
        size: 522_320,
        sha256: "8ad45c1077ab0d973ebb85ebb84f797caf6c6b188255c1178511a6feba3a0611",
    },
];

static CHATTERBOX_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "Cangjie5_TC.json",
        size: 1_920_163,
        sha256: "7073fd9de919443ae88e0bd2449917a65fe54898a4413ed1edcc4b67f28bce8c",
    },
    HfFile {
        path: "conds.safetensors",
        size: 105_316,
        sha256: "709e5a7fa80e010a011c8244f553853aed7a49c106fff54008fbd89a0f5a6148",
    },
    HfFile {
        path: "config.json",
        size: 336,
        sha256: "b52886e6c0d2c9f32bda2507c5154742c359eed20b7cacffac1c57aa45328251",
    },
    HfFile {
        path: "model.safetensors",
        size: 928_163_417,
        sha256: "ca3de1b7592d6c00850e9b81a93b5a130135fa52250c5649671dd1df30a0aab2",
    },
    HfFile {
        path: "model.safetensors.index.json",
        size: 355_942,
        sha256: "bc865294257de937442a2634b263fc5d0ac8fcd0ca100e25d82c83ae60ed1aea",
    },
    HfFile {
        path: "tokenizer.json",
        size: 70_011,
        sha256: "df81a7ca7c31796cbe97f7a7142d5a53b12e88e12417ebe98f66602cafaf0461",
    },
];

static CHATTERBOX_S3_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 126,
        sha256: "8591fcc0eaae8c2bbfc69cf9d439933ecdf2d58cb9be63d00ce88736c4f2aa9d",
    },
    HfFile {
        path: "model.safetensors",
        size: 494_868_984,
        sha256: "928726bc1f206a613d36b8f49e297eae9c5593a21bf9b92ddfe2c23f85eb92cc",
    },
];

static QWEN_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 6_058,
        sha256: "2eea3665564268139c3beb8d497fd3c2e4524e9eed5452836cdf1de96ed3cdbd",
    },
    HfFile {
        path: "generation_config.json",
        size: 245,
        sha256: "f1b90b4513f3b34c62851049e2492d7b4c5940daf1276f89c82b8ef04127f3aa",
    },
    HfFile {
        path: "merges.txt",
        size: 1_671_839,
        sha256: "599bab54075088774b1733fde865d5bd747cbcc7a547c5bc12610e874e26f5e3",
    },
    HfFile {
        path: "model.safetensors",
        size: 1_286_743_170,
        sha256: "3bcb2c4a127e6243e81a30b7126c7865f686d3559de4f938e5d3b150c6a9560d",
    },
    HfFile {
        path: "model.safetensors.index.json",
        size: 71_447,
        sha256: "0c92041960fa189cf35ae538c8d9ca07c468edddd0c9bb52274c5d4d287a860b",
    },
    HfFile {
        path: "speech_tokenizer/config.json",
        size: 2_336,
        sha256: "ee65bb901c876664ab8707c487157aa1a6ee57c65969b28fb5ec9dc211e68167",
    },
    HfFile {
        path: "speech_tokenizer/configuration.json",
        size: 76,
        sha256: "6bc26d64eb5024b4d1dab5a52371958b429256d6c9d59787f1f5294a54e0cebd",
    },
    HfFile {
        path: "speech_tokenizer/model.safetensors",
        size: 682_293_092,
        sha256: "836b7b357f5ea43e889936a3709af68dfe3751881acefe4ecf0dbd30ba571258",
    },
    HfFile {
        path: "speech_tokenizer/preprocessor_config.json",
        size: 234,
        sha256: "fcb3805e597e786d4067706e602f6688524640f8d3396790e2e09b5942fcbdfb",
    },
    HfFile {
        path: "tokenizer_config.json",
        size: 7_344,
        sha256: "dc3c31c3bdaedd5016382bb3cbe07323026775ad51f5a4fb564505992ae4a670",
    },
    HfFile {
        path: "vocab.json",
        size: 2_776_833,
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    },
];

static OMNIVOICE_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "audio_tokenizer/config.json",
        size: 2_531,
        sha256: "eefb20806f7104e77c9a5277c9df0f9bb8826b08eb1d4e8ab2b9829b6ef9fac1",
    },
    HfFile {
        path: "audio_tokenizer/model.safetensors",
        size: 402_864_930,
        sha256: "8ef745bfbabeb3bd9ebbdc69e7b6a05e43e191d0208dabfcf7adc42ca89c6580",
    },
    HfFile {
        path: "audio_tokenizer/preprocessor_config.json",
        size: 206,
        sha256: "ae61eea88558608ee2fa86d2aec9fce8d99a5ff75d09cb7651ccce21ae1d9084",
    },
    HfFile {
        path: "chat_template.jinja",
        size: 4_168,
        sha256: "a55ee1b1660128b7098723e0abcd92caa0788061051c62d51cbe87d9cf1974d8",
    },
    HfFile {
        path: "config.json",
        size: 2_238,
        sha256: "e2e13755cca29061b09d0c0c4b945e1a65179de8ec522de18e86794425f86c9f",
    },
    HfFile {
        path: "model.safetensors",
        size: 1_225_192_351,
        sha256: "5768f3f1d11ee8b3ec31fe906e5d6f5934fdc397e3e5b818de85b70b0e1a2e7e",
    },
    HfFile {
        path: "tokenizer.json",
        size: 11_423_986,
        sha256: "408f669b7e2b045fdf54201d815bd364e6667dbd845115da81239c40bc6dcfd1",
    },
    HfFile {
        path: "tokenizer_config.json",
        size: 533,
        sha256: "49f78845596a82bf15c83673794bdf9f76f812b11f60ab6a2239d9be65b00676",
    },
];

static PARAKEET_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 244_093,
        sha256: "f320f1292511f34ec47f513755fe20fd01dbfc09a925d42730e66059a6e1ef4c",
    },
    HfFile {
        path: "model.safetensors",
        size: 2_508_288_736,
        sha256: "05e01c7f396c298cf7d23f61da7b504adeab698f0aaeafd9c82d198625464592",
    },
    HfFile {
        path: "tokenizer.model",
        size: 360_916,
        sha256: "eacec2b0a77f336d4a2ca4a25a7047575d3c2b74de47e997f4c205126ed3135e",
    },
    HfFile {
        path: "tokenizer.vocab",
        size: 101_024,
        sha256: "41130ff456706304a1adec782ccc9e003c4d417e8e324353d281be958cac4e17",
    },
    HfFile {
        path: "vocab.txt",
        size: 46_772,
        sha256: "3cde1409fd78783a79b29ed4d32da57c746993856f7c8263bcb905d2e5839db7",
    },
];

static DIARIZATION_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 1_702,
        sha256: "17c9f943bed07b0593f2b8dca01e0be6a418053becc6148b01ecabdff9cbd84d",
    },
    HfFile {
        path: "model.safetensors",
        size: 236_108_132,
        sha256: "3b60b8df29e59a8abaf8061ceeeae6e9284a68fbcd2e762c68f5e058bfceebfa",
    },
];

static SPEAKER_EMBEDDING_MLX_FILES: &[HfFile] = &[
    HfFile {
        path: "config.json",
        size: 590,
        sha256: "5e598e1ef04d0c014a59f47d6a7884f26b9203bdefe08d2a5876c7b86cb40b75",
    },
    HfFile {
        path: "weights.npz",
        size: 26_614_262,
        sha256: "802706880b81ece11a9acefb2cf523ae91473e3b7615858390a1eded4efcdedf",
    },
];

/// MLX Kokoro TTS weights and voice embeddings. Apache-2.0.
pub static KOKORO_MLX: HfRepo = HfRepo {
    name: "kokoro_mlx",
    repo: "mlx-community/Kokoro-82M-bf16",
    revision: "a71e4d38b236d968966a2002c4c895dbd12b1c3c",
    files: KOKORO_MLX_FILES,
    dir_name: KOKORO_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "Kokoro (MLX)",
    usage: "Apple-Silicon text-to-speech model and voice embeddings for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Chatterbox Multilingual 8-bit weights and default voice conditioning. Apache-2.0.
pub static CHATTERBOX_MLX: HfRepo = HfRepo {
    name: "chatterbox_mlx",
    repo: "mlx-community/chatterbox-8bit",
    revision: "9617d61b596a03d1bed766a28c341680e993a1b9",
    files: CHATTERBOX_MLX_FILES,
    dir_name: CHATTERBOX_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "Chatterbox Multilingual (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model and default voice conditioning",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Chatterbox's local S3 speech tokenizer. Apache-2.0; folded into its model set.
pub static CHATTERBOX_S3_MLX: HfRepo = HfRepo {
    name: "chatterbox_s3_mlx",
    repo: "mlx-community/S3TokenizerV2",
    revision: "e0c9886f0e1c35ae85b1f27277416fb19fc72bec",
    files: CHATTERBOX_S3_MLX_FILES,
    dir_name: CHATTERBOX_S3_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "S3TokenizerV2 (MLX)",
    usage: "Apple-Silicon speech tokenizer required by Chatterbox MLX",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Qwen3-TTS 0.6B CustomVoice weights. Apache-2.0.
pub static QWEN_MLX: HfRepo = HfRepo {
    name: "qwen_mlx",
    repo: "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit",
    revision: "049ef77fe8816b536193c0c25f9a214d17921282",
    files: QWEN_MLX_FILES,
    dir_name: QWEN_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "Qwen3-TTS (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX OmniVoice bfloat16 model and Higgs audio tokenizer.
pub static OMNIVOICE_MLX: HfRepo = HfRepo {
    name: "omnivoice_mlx",
    repo: "mlx-community/OmniVoice-bf16",
    revision: "8fb0b754cad788aaefec690cd55c207e8a628f85",
    files: OMNIVOICE_MLX_FILES,
    dir_name: OMNIVOICE_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "OmniVoice (MLX)",
    usage: "Apple-Silicon omnilingual text-to-speech model and Higgs Audio 2 tokenizer",
    license: "CC-BY-NC / Boson Community License",
    license_url: "https://huggingface.co/k2-fsa/OmniVoice#license",
};

/// MLX Parakeet TDT 0.6b v3 STT — 25 European languages, detected by the model itself.
/// CC-BY-4.0.
pub static PARAKEET_MLX: HfRepo = HfRepo {
    name: "parakeet_mlx",
    repo: "mlx-community/parakeet-tdt-0.6b-v3",
    revision: "ed2b7e8c15f9aaa0b5772e2efb986255eaef7e15",
    files: PARAKEET_MLX_FILES,
    dir_name: PARAKEET_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "Parakeet (MLX)",
    usage: "Apple-Silicon multilingual speech-to-text model (NVIDIA NeMo; MLX conversion)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// MLX Sortformer speaker diarization. The converted repository does not publish
/// SPDX metadata, so the original NVIDIA model terms are linked explicitly in NOTICE.
pub static DIARIZATION_MLX: HfRepo = HfRepo {
    name: "diarization_mlx",
    repo: "mlx-community/diar_streaming_sortformer_4spk-v2.1-fp16",
    revision: "e23e6404bd9859e93edbf94a740eb1c7fc58f12e",
    files: DIARIZATION_MLX_FILES,
    dir_name: DIARIZATION_MLX_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "Sortformer diarization (MLX)",
    usage: "Apple-Silicon speaker diarization (NVIDIA Sortformer; MLX conversion)",
    license: "NVIDIA Open Model License",
    license_url: "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/",
};

/// MLX WeSpeaker ResNet34 embedding model used for enrollment and speaker identity matching.
pub static SPEAKER_EMBEDDING_MLX: HfRepo = HfRepo {
    name: "speaker_embedding_mlx",
    repo: "mlx-community/wespeaker-voxceleb-resnet34-LM",
    revision: "038a61d379b8729c72d64d7c209e0cee80b11d0f",
    files: SPEAKER_EMBEDDING_MLX_FILES,
    dir_name: SPEAKER_EMBEDDING_DIR_NAME,
    root: RepoRoot::Mlx,
    display_name: "WeSpeaker embedding (MLX)",
    usage: "Apple-Silicon speaker enrollment and identity matching",
    license: "MIT",
    license_url: "https://opensource.org/license/mit",
};

/// The repos one `DownloadTarget::KokoroMlx` fetch produces. ONE source of truth shared by the engine's
/// download manager (fetch + presence gate) and the status row, so they can never disagree
/// about what "the Kokoro MLX set" is.
pub static KOKORO_MLX_SET: [&HfRepo; 1] = [&KOKORO_MLX];

/// The repos one `DownloadTarget::ChatterboxMlx` fetch produces.
pub static CHATTERBOX_MLX_SET: [&HfRepo; 2] = [&CHATTERBOX_MLX, &CHATTERBOX_S3_MLX];

/// The repos one `DownloadTarget::QwenMlx` fetch produces.
pub static QWEN_MLX_SET: [&HfRepo; 1] = [&QWEN_MLX];

/// The repos one `DownloadTarget::OmniVoiceMlx` fetch produces.
pub static OMNIVOICE_MLX_SET: [&HfRepo; 1] = [&OMNIVOICE_MLX];

/// Complete pinned asset set for one built-in MLX TTS model.
pub fn tts_mlx_set(model: ds_config::TtsModel) -> &'static [&'static HfRepo] {
    match model {
        ds_config::TtsModel::Kokoro => &KOKORO_MLX_SET,
        ds_config::TtsModel::Chatterbox => &CHATTERBOX_MLX_SET,
        ds_config::TtsModel::Qwen => &QWEN_MLX_SET,
        ds_config::TtsModel::OmniVoice => &OMNIVOICE_MLX_SET,
    }
}

/// The repos one `DownloadTarget::ParakeetMlx` fetch produces.
pub static PARAKEET_MLX_SET: [&HfRepo; 1] = [&PARAKEET_MLX];

/// Sortformer segmentation plus WeSpeaker embeddings are one user-visible diarization download.
pub static DIARIZATION_MLX_SET: [&HfRepo; 2] = [&DIARIZATION_MLX, &SPEAKER_EMBEDDING_MLX];

/// Every MLX repo we self-manage, in the order a clean install fetches them.
pub fn all_mlx_repos() -> [&'static HfRepo; 8] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hf_repo::{ModelRoots, fixture_file, fixture_files, fixture_repo};
    use std::collections::HashSet;
    use std::path::Path;

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

    /// Source-text scan of the two native shims. This proves the shims themselves call no
    /// native download API; it CANNOT prove FluidAudio's own download call sites
    /// (`ensureG2PAssets`, `ensureVoicePack`) never fire, because those live inside the
    /// package, not in `Fluid.swift` -- only the on-device offline `speak` gate proves that.
    #[test]
    fn native_shim_loads_only_rust_managed_local_directories() {
        let shim =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift");
        let fluid =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/Fluid.swift");

        // Universal bans: no native model-download API in either shim.
        for (name, src) in [("shim.swift", shim), ("Fluid.swift", fluid)] {
            for forbidden in ["snapshotDownload", "downloadModel"] {
                assert!(
                    !src.contains(forbidden),
                    "{name}: native model download API: {forbidden}"
                );
            }
        }

        // shim.swift's own scoped assertions -- the Intel build compiles it ALONE, so these
        // must not be diluted by concatenating the other file.
        assert!(
            !shim.contains("ModelHub"),
            "shim.swift must not manage the native hub cache directly"
        );
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
        // Section 4 boundary: the one-dylib-on-Intel decision rests on shim.swift never
        // referencing a Fluid.swift symbol, so a FluidAudio reference outside
        // `#if !SYSTEM_ONLY` would break the Intel build with no local test to catch it.
        assert!(
            !shim.contains("FluidAudio"),
            "shim.swift must not reference FluidAudio (Intel compatibility-build boundary)"
        );

        // Fluid.swift's ONLY ModelHub use is the offline switch that keeps it load-only:
        // every `ModelHub` occurrence must be a `ModelHub.offlineMode = true` (each init path --
        // TTS, batch ASR, streaming ASR -- sets it), never a download or cache-management call.
        assert!(
            fluid.contains("ModelHub.offlineMode = true"),
            "Fluid.swift must load offline"
        );
        assert_eq!(
            fluid.matches("ModelHub").count(),
            fluid.matches("ModelHub.offlineMode = true").count(),
            "every Fluid.swift ModelHub use must be the offline switch"
        );
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

    /// #208 class: the set installer holds a flight over every planned repo dir for its whole
    /// run, so a `models remove` of the same model cannot unlink a repo between two of its
    /// files. The fixture carries Kokoro's real `dir_name` and root, and the remover takes the
    /// SAME `ModelRoots` value the installer's plan derives from — disagree on either and the
    /// two touch different directories, the contention window stops overlapping, and this
    /// passes vacuously. The rendezvous sits in the aggregate progress callback, inside the
    /// flight.
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
        let roots = ModelRoots::under(root.path());
        let files = fixture_files(vec![fixture_file("weights.safetensors", BODY)]);
        let repo: &'static HfRepo = Box::leak(Box::new(HfRepo {
            dir_name: KOKORO_MLX_DIR_NAME,
            ..fixture_repo("kokoro-mlx", REPO_ID, REV, files, RepoRoot::Mlx)
        }));
        let target = roots.dir_for(repo);
        assert_eq!(target, roots.dir_for(&KOKORO_MLX));
        assert!(
            crate::download::sweep_root_of(&target)
                .is_some_and(|resolved| resolved.starts_with(root.path())),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let base = server.base_url();
        let install_roots = roots.clone();
        let installer = std::thread::spawn(move || {
            let announced = AtomicBool::new(false);
            crate::hf_repo::ensure_hf_repos_at(&install_roots, &base, &[repo], &|_done, _total| {
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

        let removal_roots = ModelRoots::under(root.path());
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            removed_tx
                .send(
                    crate::inventory::remove_at(
                        &removal_roots,
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
}
