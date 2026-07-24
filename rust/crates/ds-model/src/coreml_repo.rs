//! Pinned FluidAudio (Core ML / ANE) model manifests.
//!
//! Manifests only: the shape, the roots, the transfer and the presence probe live in
//! [`crate::hf_repo`], which every self-managed Hugging Face set shares. Each set here pins
//! an immutable HF commit and every selected file's path, size, and SHA-256; a `.mlmodelc`
//! is a directory of ordinary files, so its members are plain manifest entries.

use std::path::PathBuf;

use crate::hf_repo::{HfFile, HfRepo, ModelRoots, RepoRoot};

/// On-disk folder name of the Apple-native Kokoro Core ML chain.
pub const KOKORO_COREML_DIR_NAME: &str = "kokoro-82m-coreml";
/// The Kokoro variant subfolder we fetch — MIRRORS the `.english` variant the shim requests
/// (`KokoroAneManager(variant: .english)`), whose FluidAudio `folderName` is
/// `<KOKORO_COREML_DIR_NAME>/ANE`. Mandarin (`ANE-zh`) and Japanese (`ANE-ja`) are unrouted.
pub const KOKORO_ANE_VARIANT: &str = "ANE";
/// Folder name inside FluidAudio's OWN cache. Its `G2PModel` singleton hardcodes
/// `TtsCacheDirectory.ensure()/Models/kokoro`, so this name is theirs, not ours.
pub const KOKORO_G2P_COREML_DIR_NAME: &str = "kokoro";
pub const PARAKEET_COREML_DIR_NAME: &str = "parakeet-tdt-0.6b-v2";
/// On-disk folder name of the streaming STT set (Parakeet EOU 120M).
pub const PARAKEET_EOU_DIR_NAME: &str = "parakeet-eou-streaming";
/// The EOU variant subfolder we fetch — MIRRORS the chunk size the shim requests
/// (`StreamingEouAsrManager(chunkSize: .ms160)`), so the two cannot drift. 160 ms is
/// FluidAudio's lowest-latency EOU variant (~6 partials/sec).
pub const PARAKEET_EOU_VARIANT: &str = "160ms";
pub const DIARIZATION_COREML_DIR_NAME: &str = "speaker-diarization-coreml";
/// The two `.mlmodelc` bundles the diarizer loads from [`diarization_coreml_dir`]. Mirrored
/// as literals on the Swift side, so a rename here must move with them.
pub const DIARIZATION_SEGMENTATION_MODEL: &str = "pyannote_segmentation.mlmodelc";
pub const DIARIZATION_EMBEDDING_MODEL: &str = "wespeaker_v2.mlmodelc";

/// The repo + pinned revision BOTH Kokoro Core ML sets share — the runtime ANE chain
/// ([`KOKORO_COREML`]) and the G2P/lexicon sub-models ([`KOKORO_G2P_COREML`]) are different
/// sub-paths of one tree. ONE source of truth so a pin bump cannot strand one of them on a
/// stale tree; `kokoro_coreml_sets_share_one_repo_and_revision` pins that.
const KOKORO_HF_REPO: &str = "FluidInference/kokoro-82m-coreml";
const KOKORO_HF_REVISION: &str = "c94edcb4b671856795458645cd389c0a9184e8bb";

/// `<model>/coreml/kokoro-82m-coreml/ANE` — the exact directory the ANE Kokoro chain loads
/// voice packs from (`<voice>.bin`; only `af_heart.bin` ships). A pack materialized for that
/// chain MUST land here: DontSpeak initializes it with its OWN root, not FluidAudio's cache.
pub fn kokoro_ane_dir(roots: &ModelRoots) -> PathBuf {
    roots.dir_for(&KOKORO_COREML).join(KOKORO_ANE_VARIANT)
}

/// The directory handed to FluidAudio's `KokoroAneManager(directory:)` — the Core ML root, NOT
/// the Kokoro set dir. `KokoroAneResourceDownloader.ensureModels` appends the variant's whole
/// `folderName` (`kokoro-82m-coreml/ANE` for `.english`), so the argument is one level ABOVE
/// the set dir; handing it the set dir resolves to `…/kokoro-82m-coreml/kokoro-82m-coreml/ANE`,
/// which misses and — under `ModelHub.offlineMode` — fails as
/// `networkDisabled(download(kokoro-82m-coreml/ANE))` instead of loading. `kokoro_hub_layout`
/// pins this against [`kokoro_ane_dir`], where the downloader actually writes.
pub fn kokoro_hub_root(roots: &ModelRoots) -> PathBuf {
    ds_config::coreml_dir_under(&roots.model)
}

/// The directory handed to FluidAudio's `AsrModels.load(from:version:.v2)` for the batch set.
/// The v0.15.5 loader does `from.deletingLastPathComponent().appendingPathComponent(<repo
/// folder>)`, and for `.v2` that folder is literally `parakeet-tdt-0.6b-v2` — so handing it the
/// set directory itself round-trips to the same files under `<model>/coreml/`. That is why the
/// batch set stays a normal `CoreMl` repo rather than needing a bare-`model_dir` root.
pub fn parakeet_batch_dir(roots: &ModelRoots) -> PathBuf {
    roots.dir_for(&PARAKEET_COREML)
}

/// The exact directory `StreamingEouAsrManager.loadModels(from:)` reads the streaming
/// `.mlmodelc` set + `vocab.json` from. The manifest carries the repo's `160ms/` prefix, so
/// the loadable directory is that subfolder of the download target.
pub fn parakeet_eou_dir(roots: &ModelRoots) -> PathBuf {
    roots
        .dir_for(&PARAKEET_EOU_COREML)
        .join(PARAKEET_EOU_VARIANT)
}

/// The directory the two diarization `.mlmodelc` load from.
pub fn diarization_coreml_dir(roots: &ModelRoots) -> PathBuf {
    roots.dir_for(&DIARIZATION_COREML)
}

static KOKORO_COREML_FILES: &[HfFile] = &[
    HfFile {
        path: "ANE/KokoroAlbert.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "14a61873d8759a38b79c93f9021ae865f408f56301209a0168dbfc2283265ccd",
    },
    HfFile {
        path: "ANE/KokoroAlbert.mlmodelc/coremldata.bin",
        size: 433,
        sha256: "70b6d2f8429229f6800dda9480341669f7ac0eabf05a82934d8632ab2b4b63a6",
    },
    HfFile {
        path: "ANE/KokoroAlbert.mlmodelc/metadata.json",
        size: 2_480,
        sha256: "fe1d005481f646707a948267ac089dfb2cd2ccea7d5827a14c28587eb4c89930",
    },
    HfFile {
        path: "ANE/KokoroAlbert.mlmodelc/model.mil",
        size: 101_485,
        sha256: "2038154d06a20e399a8ae35ec5c9242702cb23db8dd24dde7679cd53708e4eae",
    },
    HfFile {
        path: "ANE/KokoroAlbert.mlmodelc/weights/weight.bin",
        size: 5_718_848,
        sha256: "36089a39359b800d3e2c60e5e8ac9217d8f2d1010a8b9273192290e621f1fabc",
    },
    HfFile {
        path: "ANE/KokoroAlignment.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "f6074d1039a9151d0f97dc6ec9ee0cd9c7b865f1af646d718ab38b659fa84f3f",
    },
    HfFile {
        path: "ANE/KokoroAlignment.mlmodelc/coremldata.bin",
        size: 484,
        sha256: "9a0fb4a536f665a052914d7f17b0e6ac80a3614f702e4be87ac0302c1143a4ea",
    },
    HfFile {
        path: "ANE/KokoroAlignment.mlmodelc/metadata.json",
        size: 3_021,
        sha256: "6f610d23b9f93c7f1a968d6a861efa99725a191de696bf1c3be334ead39b7f00",
    },
    HfFile {
        path: "ANE/KokoroAlignment.mlmodelc/model.mil",
        size: 8_194,
        sha256: "eb3a618bda0cdf95cdffab586ef5c76333d0d0a1dcb881190b3ef414f3421b3e",
    },
    HfFile {
        path: "ANE/KokoroAlignment.mlmodelc/weights/weight.bin",
        size: 4_128,
        sha256: "2e7d69128b59d615fc3d3cf85637a687235fc086b1eb136359adb11a61615f6b",
    },
    HfFile {
        path: "ANE/KokoroNoise.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "de1b584b3959d91905a458b20fc3fb6bd26b68d1eac0b4576aeccd1a7235b133",
    },
    HfFile {
        path: "ANE/KokoroNoise.mlmodelc/coremldata.bin",
        size: 440,
        sha256: "6e675b07b45f7586222870758a1edfc7f5b87cd639c06acdb7191db09783283f",
    },
    HfFile {
        path: "ANE/KokoroNoise.mlmodelc/metadata.json",
        size: 3_020,
        sha256: "eb3102217532d952479c6f1b04d26d7ac4cdbc59875b6a23e8aae86e5e02c2bc",
    },
    HfFile {
        path: "ANE/KokoroNoise.mlmodelc/model.mil",
        size: 91_698,
        sha256: "5af26daba7b289a1e635846ea3ad8400d346e3aee9f05b5a8912ae97db286bb0",
    },
    HfFile {
        path: "ANE/KokoroNoise.mlmodelc/weights/weight.bin",
        size: 4_580_160,
        sha256: "5da4ccb85b789bdf0ac634cf817171c428853c39d8bc81f29b864f0eba3aa0d0",
    },
    HfFile {
        path: "ANE/KokoroNoise_v2.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "53af6bf61482f6002bdb6e3a62f30774cde6e96411aebd888222a34b369f3d04",
    },
    HfFile {
        path: "ANE/KokoroNoise_v2.mlmodelc/coremldata.bin",
        size: 440,
        sha256: "9911047f924b41f8b92811c32c50b7c718bd7b0c14842b3bc1b66b9ffe341a19",
    },
    HfFile {
        path: "ANE/KokoroNoise_v2.mlmodelc/metadata.json",
        size: 3_020,
        sha256: "eb3102217532d952479c6f1b04d26d7ac4cdbc59875b6a23e8aae86e5e02c2bc",
    },
    HfFile {
        path: "ANE/KokoroNoise_v2.mlmodelc/model.mil",
        size: 93_152,
        sha256: "60233949d896f15ef38aea19afda558935d7569fa77a3ca4babddd3ffe845a36",
    },
    HfFile {
        path: "ANE/KokoroNoise_v2.mlmodelc/weights/weight.bin",
        size: 4_580_160,
        sha256: "1102fc2d31dfcfe3de3978a4c78b65202ff8b0a4d55a9304213bd4e8bda66bc2",
    },
    HfFile {
        path: "ANE/KokoroPostAlbert.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "6640044c875505382edbc361cdd56f3f1c30f082953e0f9473a31ae3f71e6c43",
    },
    HfFile {
        path: "ANE/KokoroPostAlbert.mlmodelc/coremldata.bin",
        size: 556,
        sha256: "86de3ab0c1e8c6f8842b57bc24695a5099590c49b864e3fa2737b1fb5b15ba3b",
    },
    HfFile {
        path: "ANE/KokoroPostAlbert.mlmodelc/metadata.json",
        size: 4_171,
        sha256: "2e3be1af412a76e340120a01cd04a502666371ada86b1fdaf5a622896e0bf979",
    },
    HfFile {
        path: "ANE/KokoroPostAlbert.mlmodelc/model.mil",
        size: 60_047,
        sha256: "450b73a3b179e1702e7b2210bad008eac20d58b2d6076e3c2de76290a66e5fef",
    },
    HfFile {
        path: "ANE/KokoroPostAlbert.mlmodelc/weights/weight.bin",
        size: 13_806_464,
        sha256: "e4f300a23cc2e05d38680d9fc94681cc722d445076d1d248f83255122ba091c8",
    },
    HfFile {
        path: "ANE/KokoroProsody.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "7708aab83deabd72539512acd850447fa955187608d9710a828943aa8498c6b8",
    },
    HfFile {
        path: "ANE/KokoroProsody.mlmodelc/coremldata.bin",
        size: 421,
        sha256: "d65d87f246a6546c0fb7af6efe020106c38604e5c2b9ca00a78561c115dda33e",
    },
    HfFile {
        path: "ANE/KokoroProsody.mlmodelc/metadata.json",
        size: 2_721,
        sha256: "a780887a790fe54e2ad14d26b2d936eca4fc6ab9f727e60f7e01f0c208163eef",
    },
    HfFile {
        path: "ANE/KokoroProsody.mlmodelc/model.mil",
        size: 79_705,
        sha256: "e6b332f1ed1b22178a406a16c9767c0eed2d64b7f0c17c86ccb2b6abf863b1ed",
    },
    HfFile {
        path: "ANE/KokoroProsody.mlmodelc/weights/weight.bin",
        size: 8_454_272,
        sha256: "d3c2670eb0c528802f0815d917dcb77bf17faf063bd2bc09cfd0970e2bc1444c",
    },
    HfFile {
        path: "ANE/KokoroTail.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "7dd3d6b8cfbcdcac37b46f6eb1312b842b972dc75f7cb9968f349fedfc0d77db",
    },
    HfFile {
        path: "ANE/KokoroTail.mlmodelc/coremldata.bin",
        size: 392,
        sha256: "f28f17e4217d7ec1bed48bff4c65287169daa9198315f6fffb00a1483831f5d3",
    },
    HfFile {
        path: "ANE/KokoroTail.mlmodelc/metadata.json",
        size: 1_872,
        sha256: "7708ecc145eecf8e3ef5ef8979ea7a4f77d04c8da787454c6d9190e5300fc50b",
    },
    HfFile {
        path: "ANE/KokoroTail.mlmodelc/model.mil",
        size: 7_014,
        sha256: "b0b8fd573bac76ba7eb85730eeb25538fc7f1c666ecbf939fd4b3a4ad4495ad7",
    },
    HfFile {
        path: "ANE/KokoroTail.mlmodelc/weights/weight.bin",
        size: 81_088,
        sha256: "2d4877b5d2725a9f017653e391638bee1262d1877a080bce09726aae128fecb2",
    },
    HfFile {
        path: "ANE/KokoroVocoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "8c7c1a25a46ad46b1068905ece8f841c4bd23df23306551bad175d0da28ae74b",
    },
    HfFile {
        path: "ANE/KokoroVocoder.mlmodelc/coremldata.bin",
        size: 626,
        sha256: "e73aaf146c7543c0f75f544ac52e3287fa4eaa7e9a04bf3ac6a94d0023f16c00",
    },
    HfFile {
        path: "ANE/KokoroVocoder.mlmodelc/metadata.json",
        size: 4_392,
        sha256: "0c3decd8c05850a80964fda07e3f9be17030fcb0724e223ea5b187d370a821b1",
    },
    HfFile {
        path: "ANE/KokoroVocoder.mlmodelc/model.mil",
        size: 309_130,
        sha256: "c42be65f1e0b502dc80aba3173df1cbb02e1c8f474c550feb0f6d83c3128aae7",
    },
    HfFile {
        path: "ANE/KokoroVocoder.mlmodelc/weights/weight.bin",
        size: 48_889_920,
        sha256: "6d1f96eb50218ab687b12d6d862d2ae854c12b7165c3cd9b6b5cef261ef02ff1",
    },
    HfFile {
        path: "ANE/af_heart.bin",
        size: 522_240,
        sha256: "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b",
    },
    HfFile {
        path: "ANE/vocab.json",
        size: 1_416,
        sha256: "8d65b0188b77eafc60751dac42bbac7ab5f5685074af44db91d1877b42dc1d7c",
    },
];

static KOKORO_G2P_COREML_FILES: &[HfFile] = &[
    HfFile {
        path: "G2PDecoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "dbf1767747fdc188222d467a45b04608b396c76c71db3abb18a5fb3680ef9827",
    },
    HfFile {
        path: "G2PDecoder.mlmodelc/coremldata.bin",
        size: 545,
        sha256: "607e960f19b4d9a30317a5a11869fcce84b300a909fcab2cc756c0d98e2dacd9",
    },
    HfFile {
        path: "G2PDecoder.mlmodelc/metadata.json",
        size: 3_064,
        sha256: "e54e98484fd60d26f22fd3c4e7fe87b0d92a5d2de1f958cc3c4bb36d4ae06a44",
    },
    HfFile {
        path: "G2PDecoder.mlmodelc/model.mil",
        size: 19_737,
        sha256: "fe647c598e0d9454d360b8ee49a59ae57ca147fc5330863ba84ccb90dce482ad",
    },
    HfFile {
        path: "G2PDecoder.mlmodelc/weights/weight.bin",
        size: 828_030,
        sha256: "cbaeb4e743359f607ab161af0c6d8a817462fdaec622ee788ef8ef952c5f8214",
    },
    HfFile {
        path: "G2PEncoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "cf7fbd7e7a65529b2d2bf3941e458a0ab6dff7a298bf48e205a1727c81c26a99",
    },
    HfFile {
        path: "G2PEncoder.mlmodelc/coremldata.bin",
        size: 398,
        sha256: "0f14d46ca9fd06c68b4717294575b2b99449e67d40b7a2c56f926bf05cd90b11",
    },
    HfFile {
        path: "G2PEncoder.mlmodelc/metadata.json",
        size: 2_034,
        sha256: "c8e0cfd7f494ac1b3662ff8f1914b2b45f79ffb2791724cdc0576981996732e1",
    },
    HfFile {
        path: "G2PEncoder.mlmodelc/model.mil",
        size: 20_392,
        sha256: "8c617e569f37286b056dad800d862dc145be9a95fa9ed43857bb646ba199d7da",
    },
    HfFile {
        path: "G2PEncoder.mlmodelc/weights/weight.bin",
        size: 694_592,
        sha256: "6926bcd2827d21fec82839487b987e06f85fd8a6a5bb896bc4f6062461d014ec",
    },
    HfFile {
        path: "g2p_vocab.json",
        size: 1_665,
        sha256: "295ed64b86c2820cd665b0602ae50c6947c0e82ac643082873e0be87dca282ce",
    },
    HfFile {
        path: "us_lexicon_cache.json",
        size: 10_444_631,
        sha256: "6b36ba313202227d6914ad32cd684a0304bd2757e9ec4158ea7bc36ec40e224e",
    },
];

static PARAKEET_COREML_FILES: &[HfFile] = &[
    HfFile {
        path: "Decoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "46de1a6fe2e49d19a2125bc91acf020df7f2aea84ba821532aade8427a440b05",
    },
    HfFile {
        path: "Decoder.mlmodelc/coremldata.bin",
        size: 554,
        sha256: "d200ca07694a347f6d02a3886a062ae839831e094e443222f2e48a14945966a8",
    },
    HfFile {
        path: "Decoder.mlmodelc/metadata.json",
        size: 3_427,
        sha256: "90a279b822496316458febc0ce761ab05954fadd9d66aa97bea077a35fc8f2b2",
    },
    HfFile {
        path: "Decoder.mlmodelc/model.mil",
        size: 13_106,
        sha256: "7b95a5a6b672c652000348a67b6d4d92bb8e176b978c6666fe73c28a4d7ec579",
    },
    HfFile {
        path: "Decoder.mlmodelc/weights/weight.bin",
        size: 14_429_952,
        sha256: "27d26890221d82322c1092fd99d7b40578e435d5cf4b83c887c42603caf97aba",
    },
    HfFile {
        path: "Encoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "42e638870d73f26b332918a3496ce36793fbb413a81cbd3d16ba01328637a105",
    },
    HfFile {
        path: "Encoder.mlmodelc/coremldata.bin",
        size: 485,
        sha256: "4def7aa848599ad0e17a8b9a982edcdbf33cf92e1f4b798de32e2ca0bc74b030",
    },
    HfFile {
        path: "Encoder.mlmodelc/metadata.json",
        size: 2_926,
        sha256: "58222fbc48c13c49d9715567803cd50cb9c23e4360462e0f8ffcea59a2c73c63",
    },
    HfFile {
        path: "Encoder.mlmodelc/model.mil",
        size: 959_769,
        sha256: "ed7b19156ca29fa7dfd6891deb9fda4b0e8893f68597c985d135736546a43808",
    },
    HfFile {
        path: "Encoder.mlmodelc/weights/weight.bin",
        size: 445_187_200,
        sha256: "4adc7ad44f9d05e1bffeb2b06d3bb02861a5c7602dff63a6b494aed3bf8a6c3e",
    },
    HfFile {
        path: "JointDecision.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "f1183ba213bb94a918c8d2cad19ab045320618f97f6ca662245b3936d7b090f7",
    },
    HfFile {
        path: "JointDecision.mlmodelc/coremldata.bin",
        size: 534,
        sha256: "e2c6752f1c8cf2d3f6f26ec93195c9bfa759ad59edf9f806696a138154f96f11",
    },
    HfFile {
        path: "JointDecision.mlmodelc/metadata.json",
        size: 2_936,
        sha256: "ba8d309417b9acd4a175fdb15687de6a941db2f5b06666a60e7cf3cc8e2d3c3c",
    },
    HfFile {
        path: "JointDecision.mlmodelc/model.mil",
        size: 9_722,
        sha256: "93bf82042235127cb81ab537dcae47a1c2e7e242ce4ffdaf772981b45eedc4f0",
    },
    HfFile {
        path: "JointDecision.mlmodelc/weights/weight.bin",
        size: 3_453_388,
        sha256: "ca22a65903a05e64137677da608077578a8606090a598abf4875fa6199aaa19d",
    },
    HfFile {
        path: "Preprocessor.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "03ab3c1327a054c54c07a40325db967ec574f2c91dcc8192bfa44aa561bcf2d8",
    },
    HfFile {
        path: "Preprocessor.mlmodelc/coremldata.bin",
        size: 494,
        sha256: "d88ea1fc349459c9e100d6a96688c5b29a1f0d865f544be103001724b986b6d6",
    },
    HfFile {
        path: "Preprocessor.mlmodelc/metadata.json",
        size: 2_974,
        sha256: "fb16c581ff5e1b962e7cb2181ed892cd32f9f84c12b6e80ff3e089f28e35bcbb",
    },
    HfFile {
        path: "Preprocessor.mlmodelc/model.mil",
        size: 27_166,
        sha256: "3e06d16fd061294c8a75be68c43a3b1ed1f593d4a9c35249e9cdbccadc59721e",
    },
    HfFile {
        path: "Preprocessor.mlmodelc/weights/weight.bin",
        size: 298_880,
        sha256: "a5f7df6c7f47147ae9486fe18cc7792f9a44d093ec3c6a11e91ef2dc363c48dc",
    },
    HfFile {
        path: "config.json",
        size: 3,
        sha256: "ca3d163bab055381827226140568f3bef7eaac187cebd76878e0b63e9e442356",
    },
    HfFile {
        path: "parakeet_vocab.json",
        size: 18_762,
        sha256: "57019fe3c745772ca83a1b048a4bb951cd51329504ea33d4d83316b96e279a97",
    },
];

static PARAKEET_EOU_COREML_FILES: &[HfFile] = &[
    HfFile {
        path: "160ms/decoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "3996975a8cbc1949159c55605b3132b39b2484f51acbd55d796d93c70de02b49",
    },
    HfFile {
        path: "160ms/decoder.mlmodelc/coremldata.bin",
        size: 497,
        sha256: "c3ccbff963d8cf07e2be2bd56ea3384a89ea49628922c6bd95ff62e2ae57dc34",
    },
    HfFile {
        path: "160ms/decoder.mlmodelc/metadata.json",
        size: 3_283,
        sha256: "0977480649f2756894b0acfe2fdf4231a991f25e3fe02562bfb71b65ca944575",
    },
    HfFile {
        path: "160ms/decoder.mlmodelc/model.mil",
        size: 7_409,
        sha256: "b7c084a35bdbc887d69d6226cd533e2c11b2792c37d7352cf878f9f6f3c13555",
    },
    HfFile {
        path: "160ms/decoder.mlmodelc/weights/weight.bin",
        size: 7_873_600,
        sha256: "0b4cacecdcd9df79ab1e56de67230baf5a8664d2afe0bb8f3408eefa972cb2f4",
    },
    HfFile {
        path: "160ms/joint_decision.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "5bca32ad130dcad6605cc00044c752aa5b45ef57d14c17f2d1a2fa49d6cf55b5",
    },
    HfFile {
        path: "160ms/joint_decision.mlmodelc/coremldata.bin",
        size: 493,
        sha256: "22d4abc4625b935ee035b5f8ce7cb28d1041b9b01c12173e287bf4b5f5d99625",
    },
    HfFile {
        path: "160ms/joint_decision.mlmodelc/metadata.json",
        size: 3_181,
        sha256: "e970ae87137730020690d24d971813db3633bbdfed602d43b6a9c84deced6dc8",
    },
    HfFile {
        path: "160ms/joint_decision.mlmodelc/model.mil",
        size: 9_608,
        sha256: "45e8590bc87e34c162b547e43a4f60e64db15b017f48395d7835a6867884804f",
    },
    HfFile {
        path: "160ms/joint_decision.mlmodelc/weights/weight.bin",
        size: 2_794_182,
        sha256: "7039b2010a269153f5a96edf28637f921a86ef8822f248f2d6712f7a6bce84b4",
    },
    HfFile {
        path: "160ms/streaming_encoder.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "a981b257db79b4f86e6fa06a92562160a0ae71554746c24af24d8634b85f0356",
    },
    HfFile {
        path: "160ms/streaming_encoder.mlmodelc/coremldata.bin",
        size: 670,
        sha256: "e762abc60d999bcd10aab985b68191a602f2e8e03165cf08671c60f93936037a",
    },
    HfFile {
        path: "160ms/streaming_encoder.mlmodelc/metadata.json",
        size: 5_327,
        sha256: "75be31534cdd91711b08ba3a46046523eb9be9909618cd569cce1ea79e842a95",
    },
    HfFile {
        path: "160ms/streaming_encoder.mlmodelc/model.mil",
        size: 639_646,
        sha256: "709f9280eb0bba1fd698cc252275ba802885c2c53cdb60d399277281dac09b5d",
    },
    HfFile {
        path: "160ms/streaming_encoder.mlmodelc/weights/weight.bin",
        size: 212_691_776,
        sha256: "12cd781a4300b52b6687587b7d8e37e0ce5c8ccb1dbea036008275e6abf5070c",
    },
    HfFile {
        path: "160ms/vocab.json",
        size: 17_437,
        sha256: "83fd42ad33dae1bd3ceee6c0bb6c625f314cf0b2dc8430be441ac1e2643d5c36",
    },
];

static DIARIZATION_COREML_FILES: &[HfFile] = &[
    HfFile {
        path: "pyannote_segmentation.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "b379db0541b35344a34bb7540783ae704c11599bbed5aa8bbbda11c20ad215ee",
    },
    HfFile {
        path: "pyannote_segmentation.mlmodelc/coremldata.bin",
        size: 316,
        sha256: "4a450ea1b053b9eb7eef0cab6971018076600840c7e246d064e7c5387f456c98",
    },
    HfFile {
        path: "pyannote_segmentation.mlmodelc/metadata.json",
        size: 1_763,
        sha256: "44e1fa36d6abafacf688beccad99f7569394248d8bb41545829997c67668c08c",
    },
    HfFile {
        path: "pyannote_segmentation.mlmodelc/model.mil",
        size: 29_490,
        sha256: "97f2dec6f83e80bf4247b98e13c2dde19f92c05820ef08068bbf554488d70bdd",
    },
    HfFile {
        path: "pyannote_segmentation.mlmodelc/weights/weight.bin",
        size: 5_734_720,
        sha256: "0266f4ad4d843ecf31ef9220ad6b80616b3ec64a4404b64f3ea0371554e236ec",
    },
    HfFile {
        path: "wespeaker_v2.mlmodelc/analytics/coremldata.bin",
        size: 243,
        sha256: "d2b1fcde6121aea3ff0e14c1dc50d09dacb0314a2e89156353c31804230a422f",
    },
    HfFile {
        path: "wespeaker_v2.mlmodelc/coremldata.bin",
        size: 359,
        sha256: "6feb2472a71fa9d8a84020c85206138a4f6261c565c9884bf518d59dd5838da7",
    },
    HfFile {
        path: "wespeaker_v2.mlmodelc/metadata.json",
        size: 2_738,
        sha256: "ddc4858b4051254098015cd0b97080149839d697faf7b036f933190e70b26758",
    },
    HfFile {
        path: "wespeaker_v2.mlmodelc/model.mil",
        size: 706_900,
        sha256: "2850f775d6ba659f01f616fed77ce6a45a25de3eb7e4bf3a4b07b658be4e13dd",
    },
    HfFile {
        path: "wespeaker_v2.mlmodelc/weights/weight.bin",
        size: 7_243_904,
        sha256: "34004f6798d35cad7071e2fdc67e63faaa782f53697e1cb49bcb452cf81ae151",
    },
];

// counts=[42, 12, 22, 16, 10] total_files=102 total_bytes=801_616_892

/// Apple-native Kokoro TTS runtime chain (the `ANE/` subtree).
pub static KOKORO_COREML: HfRepo = HfRepo {
    name: "kokoro_coreml",
    repo: KOKORO_HF_REPO,
    revision: KOKORO_HF_REVISION,
    files: KOKORO_COREML_FILES,
    dir_name: KOKORO_COREML_DIR_NAME,
    root: RepoRoot::CoreMl,
    display_name: "Kokoro (Core ML / ANE)",
    usage: "Apple-Silicon text-to-speech voice model (FluidAudio Core ML / ANE)",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Kokoro's G2P + lexicon sub-models, in the SAME repo at the SAME revision as the runtime
/// chain. They live under FluidAudio's own cache because `KokoroAneManager.initialize()`
/// ensures them through a singleton that hardcodes that path — DontSpeak pre-fills the exact
/// directory it reads. A pre-fill for `initialize()`, never a second G2P in the synthesis
/// path: phonemes still come from Rust. Removing the `kokoro` asset reclaims this directory
/// too, which FluidAudio would re-create for itself if run independently.
pub static KOKORO_G2P_COREML: HfRepo = HfRepo {
    name: "kokoro_g2p_coreml",
    repo: KOKORO_HF_REPO,
    revision: KOKORO_HF_REVISION,
    files: KOKORO_G2P_COREML_FILES,
    dir_name: KOKORO_G2P_COREML_DIR_NAME,
    root: RepoRoot::FluidCache,
    // Empty display_name/usage ⇒ folded into the Kokoro Core ML catalog entry; the license
    // record stays real, because these bytes are downloaded like any other.
    display_name: "",
    usage: "",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Apple-native Parakeet TDT 0.6b **v2** STT — English only, unlike the MLX rung's
/// multilingual v3.
///
/// Stays a normal `CoreMl` repo at `parakeet-tdt-0.6b-v2`: FluidAudio 0.15.5's
/// `AsrModels.load(from:version:.v2)` resolves the model directory as
/// `from.deletingLastPathComponent()` + the v2 repo folder (`parakeet-tdt-0.6b-v2`), so the
/// shim hands it this set directory ([`parakeet_batch_dir`]) and the load round-trips to the
/// same files under `<model>/coreml/`. No bare-`model_dir` root is required.
pub static PARAKEET_COREML: HfRepo = HfRepo {
    name: "parakeet_coreml",
    repo: "FluidInference/parakeet-tdt-0.6b-v2-coreml",
    revision: "ee09c569f73759e6d44c9bd16766f477b2b36d39",
    files: PARAKEET_COREML_FILES,
    dir_name: PARAKEET_COREML_DIR_NAME,
    root: RepoRoot::CoreMl,
    display_name: "Parakeet (Core ML)",
    usage: "Apple-Silicon speech-to-text model (NVIDIA NeMo; Core ML export by FluidInference)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// Parakeet EOU 120M streaming STT (the live dictation overlay), `160ms/` variant only. The
/// model card declares NVIDIA's open model license, same terms as the MLX Sortformer set.
pub static PARAKEET_EOU_COREML: HfRepo = HfRepo {
    name: "parakeet_eou_coreml",
    repo: "FluidInference/parakeet-realtime-eou-120m-coreml",
    revision: "40a23f4c0b333aa17ad8c0f2ea47ec2347f2f355",
    files: PARAKEET_EOU_COREML_FILES,
    dir_name: PARAKEET_EOU_DIR_NAME,
    root: RepoRoot::CoreMl,
    display_name: "Parakeet EOU streaming (Core ML)",
    usage: "Apple-Silicon real-time streaming speech-to-text (NVIDIA NeMo; Core ML export by FluidInference)",
    license: "NVIDIA Open Model License",
    license_url: "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/",
};

/// Apple-native speaker diarization: pyannote segmentation plus the WeSpeaker v2 embedding.
pub static DIARIZATION_COREML: HfRepo = HfRepo {
    name: "diarization_coreml",
    repo: "FluidInference/speaker-diarization-coreml",
    revision: "1ed7a662fdc7109e36d822db793ee6eebdaf8594",
    files: DIARIZATION_COREML_FILES,
    dir_name: DIARIZATION_COREML_DIR_NAME,
    root: RepoRoot::CoreMl,
    display_name: "Diarization (Core ML)",
    usage: "Apple-Silicon speaker diarization (pyannote segmentation + wespeaker embedding)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// The repos one `DownloadTarget::KokoroFluid` fetch produces — the runtime ANE chain plus
/// its G2P/lexicon sub-models. ONE source of truth shared by the download manager, the
/// presence gate and the inventory row, so they can never disagree about what the set is.
pub static KOKORO_COREML_SET: [&HfRepo; 2] = [&KOKORO_COREML, &KOKORO_G2P_COREML];

/// The repos one `DownloadTarget::ParakeetFluid` fetch produces — the streaming EOU set (the
/// live overlay) plus the batch set the offline path transcribes with.
pub static PARAKEET_COREML_SET: [&HfRepo; 2] = [&PARAKEET_EOU_COREML, &PARAKEET_COREML];

/// The repos one `DownloadTarget::DiarizationFluid` fetch produces.
pub static DIARIZATION_COREML_SET: [&HfRepo; 1] = [&DIARIZATION_COREML];

/// Every Core ML repo we self-manage, in the order a clean install fetches them.
pub fn all_coreml_repos() -> [&'static HfRepo; 5] {
    [
        &KOKORO_COREML,
        &KOKORO_G2P_COREML,
        &PARAKEET_COREML,
        &PARAKEET_EOU_COREML,
        &DIARIZATION_COREML,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Component, Path};

    #[test]
    fn coreml_manifests_are_complete_unique_and_sha256_pinned() {
        let repos = all_coreml_repos();
        let counts: Vec<usize> = repos.iter().map(|repo| repo.files.len()).collect();
        assert_eq!(counts, vec![42, 12, 22, 16, 10]);
        assert_eq!(counts.iter().sum::<usize>(), 102);
        assert_eq!(
            repos
                .iter()
                .flat_map(|repo| repo.files)
                .map(|file| file.size)
                .sum::<u64>(),
            801_616_892
        );

        for repo in repos {
            assert_eq!(repo.revision.len(), 40, "{} revision", repo.name);
            assert!(
                repo.revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
            assert!(repo.repo.starts_with("FluidInference/"));
            assert!(!repo.files.is_empty());

            let mut paths = HashSet::new();
            for file in repo.files {
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

    /// Both Kokoro sets are sub-paths of ONE tree: a pin bump that moved only one of them
    /// would mix model files across two commits.
    #[test]
    fn kokoro_coreml_sets_share_one_repo_and_revision() {
        for repo in KOKORO_COREML_SET {
            assert_eq!(repo.repo, KOKORO_HF_REPO, "{}", repo.name);
            assert_eq!(repo.revision, KOKORO_HF_REVISION, "{}", repo.name);
        }
        assert_ne!(KOKORO_COREML.dir_name, KOKORO_G2P_COREML.dir_name);
        assert_ne!(KOKORO_COREML.root, KOKORO_G2P_COREML.root);
    }

    /// Cross-language drift guard: the two diarization `.mlmodelc` basenames the shim loads
    /// MUST be exactly the constants this manifest pins, or the Rust presence probe and the
    /// Swift loader (`ds_fluid_diar_init`) would look at different files. The Swift side hard-
    /// codes them as string literals; this asserts they still appear verbatim in `Fluid.swift`.
    /// Four levels up from `ds-model/src/` is the repo root — the same base `mlx_repo.rs`'s
    /// Swift guard uses (`include_str!` resolves against this source file, not the CWD).
    #[test]
    fn diarization_model_names_match_prefixes() {
        const FLUID_SWIFT: &str =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakFluid/Fluid.swift");
        assert!(
            FLUID_SWIFT.contains(DIARIZATION_SEGMENTATION_MODEL),
            "Fluid.swift must load {DIARIZATION_SEGMENTATION_MODEL}"
        );
        assert!(
            FLUID_SWIFT.contains(DIARIZATION_EMBEDDING_MODEL),
            "Fluid.swift must load {DIARIZATION_EMBEDDING_MODEL}"
        );
    }

    /// `.mlmodelc` members are nested manifest paths, so a hand-edited `..` would escape the
    /// download target — the transfer rejects that at run time; this rejects it at review time.
    #[test]
    fn coreml_directory_bundle_paths_are_normal_components() {
        for repo in all_coreml_repos() {
            for file in repo.files {
                assert!(
                    Path::new(file.path)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                    "{} has unsafe path {}",
                    repo.name,
                    file.path
                );
            }
        }
    }

    /// Pure path math (no FS): the layout each set promises, including the one set that lands
    /// OUTSIDE DontSpeak's own cache because FluidAudio's G2P singleton hardcodes it.
    #[test]
    fn fluid_kokoro_g2p_lives_outside_the_model_root() {
        let roots = ModelRoots::under(Path::new("/roots"));
        let coreml = roots.model.join("coreml");

        assert_eq!(
            roots.dir_for(&KOKORO_COREML),
            coreml.join(KOKORO_COREML_DIR_NAME)
        );
        assert_eq!(kokoro_ane_dir(&roots), coreml.join("kokoro-82m-coreml/ANE"));
        assert_eq!(
            roots.dir_for(&PARAKEET_COREML),
            coreml.join(PARAKEET_COREML_DIR_NAME)
        );
        assert_eq!(
            parakeet_eou_dir(&roots),
            coreml.join("parakeet-eou-streaming/160ms")
        );
        assert_eq!(
            diarization_coreml_dir(&roots),
            coreml.join(DIARIZATION_COREML_DIR_NAME)
        );

        let g2p = roots.dir_for(&KOKORO_G2P_COREML);
        assert_eq!(g2p, roots.fluid.join(KOKORO_G2P_COREML_DIR_NAME));
        assert!(
            !g2p.starts_with(&roots.model),
            "the G2P set is pre-filled into FluidAudio's own cache"
        );
    }

    /// FluidAudio resolves its Kokoro models as `directory + <variant folderName>`, and for
    /// `.english` that folder is the two-component `kokoro-82m-coreml/ANE` — so the argument is
    /// the Core ML ROOT, not the set dir. Passing the set dir resolved one level too deep and,
    /// under `ModelHub.offlineMode`, surfaced as `networkDisabled(download(...))` rather than a
    /// load. This pins the argument against where the downloader actually writes.
    #[test]
    fn kokoro_hub_layout() {
        let roots = ModelRoots::under(Path::new("/roots"));

        assert_eq!(kokoro_hub_root(&roots), roots.model.join("coreml"));
        assert_eq!(
            kokoro_hub_root(&roots).join(format!("{KOKORO_COREML_DIR_NAME}/{KOKORO_ANE_VARIANT}")),
            kokoro_ane_dir(&roots),
            "FluidAudio's append must land on the downloaded set"
        );
        assert_ne!(
            kokoro_hub_root(&roots),
            roots.dir_for(&KOKORO_COREML),
            "the hub root is ABOVE the set dir"
        );
    }

    /// Cross-language drift guard: the variant this manifest fetches (`ANE/` prefixes, pinned by
    /// [`KOKORO_ANE_VARIANT`]) MUST be the variant `ds_fluid_tts_init` asks FluidAudio for, or
    /// the loader would resolve a folder we never downloaded. The Swift side names it as an enum
    /// case, so assert that case still appears verbatim.
    #[test]
    fn kokoro_variant_matches_the_swift_loader() {
        const FLUID_SWIFT: &str =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakFluid/Fluid.swift");
        assert_eq!(KOKORO_ANE_VARIANT, "ANE", "the `.english` variant folder");
        assert!(
            FLUID_SWIFT.contains("variant: .english"),
            "Fluid.swift must request the .english Kokoro variant"
        );
    }
}
