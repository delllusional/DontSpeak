//! THE single registry of every resource the app downloads.
//!
//! One file to open to see — or change — WHAT the app fetches and from WHERE: the Kokoro
//! TTS + Parakeet STT model files AND the ONNX Runtime dylib/CUDA package, each as on-disk
//! name + source URL + pinned SHA-256 + size. This module is PURE DATA. The behaviour that
//! consumes it lives elsewhere and reads only from here:
//!   * `spec.rs` — the `ModelSpec`/`DownloadFile` builders + network-free presence probes.
//!   * `ort.rs`  — the per-OS runtime SELECTION + archive download/extract.
//!
//! To update a pin: change the URL + `sha256` (`shasum -a 256`) + `size_bytes` here, and
//! nowhere else.

/// A single downloadable file: everything needed to fetch, verify, and size it.
#[derive(Debug, Clone, Copy)]
pub struct Download {
    /// On-disk name (saved flat under `model_dir()`). The installer stages by
    /// `download::prefetch_key(url)`, not this name — basenames may repeat across sets.
    pub file_name: &'static str,
    /// Source URL.
    pub url: &'static str,
    /// Pinned lowercase-hex SHA-256 (a mismatch makes `ensure` reject the download).
    pub sha256: &'static str,
    /// Exact (Kokoro/Parakeet release blobs) or expected size in bytes — the up-front total
    /// shown before a fetch; the live `Content-Length` drives the actual progress bar. Any
    /// human "~310 MB" label is formatted from this at the DISPLAY site, not stored here.
    pub size_bytes: u64,
}

// ── Kokoro TTS — onnx-community/Kokoro-82M-v1.0-ONNX FP32 ─────────────
//
// FP32, not the half-size FP16 export from the same revision: FP16 overflows to NaN for
// whole utterances (always under the CUDA EP, and under the CPU EP whenever the CUDA
// build of the ORT dylib is loaded). The graph takes and returns FP32 either way, so the
// precision is internal and cannot be worked around from the caller.

pub const KOKORO_ONNX: Download = Download {
    file_name: "kokoro-v1.0-fp32.onnx",
    url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231/onnx/model.onnx",
    sha256: "8fbea51ea711f2af382e88c833d9e288c6dc82ce5e98421ea61c058ce21a34cb",
    size_bytes: 325_532_232,
};

pub const KOKORO_VOICES: Download = Download {
    file_name: "voices-v1.0.bin",
    url: "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin",
    sha256: "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
    size_bytes: 28_214_398,
};

// Kokoro-compatible English OOV G2P. These are standard Optimum ONNX exports of the
// PeterReid checkpoint, pinned to an immutable export commit. Keeping them as runtime assets
// avoids Cargo's inability to smudge Git LFS and lets the normal downloader verify the bytes.
pub const KOKORO_G2P_ENCODER: Download = Download {
    file_name: "encoder_model.onnx",
    url: "https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us/resolve/9470bafd46d1e5c05225f2942853b1de90bc9658/onnx/encoder_model.onnx",
    sha256: "5419f10161ea94c960c24890b4a125f44295d80ed56dd80a43e3d90dd75e01ae",
    size_bytes: 1_414_890,
};

pub const KOKORO_G2P_DECODER: Download = Download {
    file_name: "decoder_model.onnx",
    url: "https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us/resolve/9470bafd46d1e5c05225f2942853b1de90bc9658/onnx/decoder_model.onnx",
    sha256: "c091bca25466cf3c29b2c720c804774e26e9244b856f1c92c08308ac54d5201e",
    size_bytes: 1_750_816,
};

// MLX Audio/Misaki uses espeakng-loader for Kokoro's Spanish, French, Hindi,
// Italian, and Portuguese frontends. Wheels contain the shared library and its
// espeak-ng-data tree; selection and extraction live in `kokoro_frontend.rs`.
pub const ESPEAKNG_LOADER_VERSION: &str = "0.2.4";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/9d/ed/a3d872fbad4f3a3f3db0e8c31768ab14e77cd77306de16b8b20b1e1df7ea/espeakng_loader-0.2.4-py3-none-win_amd64.whl";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "41f1e08ac9deda2efd1ea9de0b81dab9f5ae3c4b24284f76533d0a7b1dd7abd7";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 9_437_292;

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/29/64/0b75bc50ec53b4e000bac913625511215aa96124adf5dba8c4baa17c02cd/espeakng_loader-0.2.4-py3-none-win_arm64.whl";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "d7a2928843eaeb2df82f99a370f44e8a630f59b02f9b0d1f168a03c4eeb76b89";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 9_426_841;

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/f8/92/f44ed7f531143c3c6c97d56e2b0f9be8728dc05e18b96d46eb539230ed46/espeakng_loader-0.2.4-py3-none-macosx_10_12_x86_64.whl";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "b77477ae2ddf62a748e04e49714eabb2f3a24f344166200b00539083bd669904";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 9_938_387;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/a8/26/258c0cd43b9bc1043301c5f61767d6a6c3b679df82790c9cb43a3277b865/espeakng_loader-0.2.4-py3-none-macosx_11_0_arm64.whl";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "d27cdca31112226e7299d8562e889d3e38a1e48055c9ee381b45d669072ee59f";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 9_892_565;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/de/1e/25ec5ab07528c0fbb215a61800a38eca05c8a99445515a02d7fa5debcb32/espeakng_loader-0.2.4-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "08721baf27d13d461f6be6eed9a65277e70d68234ff484fd8b9897b222cdcb6d";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 10_078_484;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_URL: &str = "https://files.pythonhosted.org/packages/d9/ad/1b768d8daffc2996e07bbcb6f534d8de3202cd75fce1f1c45eced1ce6465/espeakng_loader-0.2.4-py3-none-manylinux_2_28_aarch64.whl";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SHA256: &str =
    "d1e798141b46a050cdb75fcf3c17db969bb2c40394f3f4a48910655d547508b9";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ESPEAKNG_LOADER_SIZE_BYTES: u64 = 10_037_736;

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64")
))]
pub const ESPEAKNG_LOADER: Download = Download {
    file_name: "espeakng-loader-0.2.4.whl",
    url: ESPEAKNG_LOADER_URL,
    sha256: ESPEAKNG_LOADER_SHA256,
    size_bytes: ESPEAKNG_LOADER_SIZE_BYTES,
};

// ── Parakeet STT: TDT 0.6b v3 int8 (NVIDIA NeMo) ──────────────────────────
// sherpa-onnx export of nvidia/parakeet-tdt-0.6b-v3 — 25 European languages, detected by
// the model. Exported as the plain transducer branch (no TDT durations, no encoder cache),
// so `ds_stt::streaming` encodes a whole speech segment at a time. ~671 MB. URLs pin the
// export repo's commit, not `main`, so the bytes behind them cannot move.

pub const PARAKEET_ENCODER: Download = Download {
    file_name: "encoder.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/encoder.int8.onnx",
    sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
    size_bytes: 652_184_281,
};

pub const PARAKEET_DECODER: Download = Download {
    file_name: "decoder.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/decoder.int8.onnx",
    sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
    size_bytes: 11_845_275,
};

pub const PARAKEET_JOINER: Download = Download {
    file_name: "joiner.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/joiner.int8.onnx",
    sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
    size_bytes: 6_355_277,
};

pub const PARAKEET_TOKENS: Download = Download {
    file_name: "tokens.txt",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78/tokens.txt",
    sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    size_bytes: 93_939,
};

// ── SepFormer speech separator — int8 ONNX export of SpeechBrain's sepformer-wsj02mix ──
// Powers the macOS dictation speaker-lock (`ds-stt::separate`): splits a single-mic mixture
// into its constituent voices so the enrolled user's stream can be isolated before
// transcription. 8 kHz mono, 2 sources, dynamic time axis (CPU EP). Published by this
// project (the one-off export predates the repo);
// pinned by sha like every other model.

pub const SEPFORMER: Download = Download {
    file_name: "sepformer_int8.onnx",
    url: "https://huggingface.co/dellusional/sepformer-wsj02mix-int8-onnx/resolve/main/sepformer_int8.onnx",
    sha256: "c28ef4168295b182fbf4b18b3c5743d649d39cbf7e0eee8b0e49a653f35bcb5e",
    size_bytes: 29_927_349,
};

// On-disk file-name aliases — kept as standalone consts because they are part of the
// crate's public API (`ds_model::KOKORO_ONNX_FILE`, …), consumed by callers that
// resolve a path without needing the full `Download`.
pub const KOKORO_ONNX_FILE: &str = KOKORO_ONNX.file_name;
pub const KOKORO_VOICES_FILE: &str = KOKORO_VOICES.file_name;
pub const KOKORO_G2P_ENCODER_FILE: &str = KOKORO_G2P_ENCODER.file_name;
pub const KOKORO_G2P_DECODER_FILE: &str = KOKORO_G2P_DECODER.file_name;
pub const PARAKEET_ENCODER_FILE: &str = PARAKEET_ENCODER.file_name;
pub const PARAKEET_DECODER_FILE: &str = PARAKEET_DECODER.file_name;
pub const PARAKEET_JOINER_FILE: &str = PARAKEET_JOINER.file_name;
pub const PARAKEET_TOKENS_FILE: &str = PARAKEET_TOKENS.file_name;
pub const SEPFORMER_FILE: &str = SEPFORMER.file_name;

// ── ONNX Runtime — microsoft/onnxruntime releases ────────────────────────────
// The shared `load-dynamic` inference dylib (built-in ORT TTS + Parakeet paths). The per-OS
// SELECTION + extraction lives in `ort.rs`; this holds the pinned dist URL + digest. Pins
// are 1.27.1 (a NEWER runtime serves the workspace's api-23 request; 1.24.2's loader
// deadlocks on the SepFormer graph) — except Intel macOS, which pins 1.23.2: Microsoft's
// LAST x86_64 macOS build, in any channel. That cap is why the workspace `ort` binds api-23
// (1.27 serves it too); requesting api-24 there would hand back a NULL API table.

/// The onnxruntime version this target's pinned dist installs — it's embedded in the macOS
/// dylib's `LC_ID_DYLIB` name and written to the sidecar version marker.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ONNXRUNTIME_VERSION: &str = "1.23.2";
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub const ONNXRUNTIME_VERSION: &str = "1.27.1";

/// Size (bytes) of the onnxruntime dist archive, for the up-front manifest total. Sized per
/// target: the Windows zips are ~77 MB, the macOS tgz ~32 MB, the Linux tgz ~8 MB.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 77_242_362;
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 78_590_093;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 8_828_892;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 7_812_402;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 11_676_322;
// macOS arm64 (and any target without its own dist above).
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 31_959_937;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-osx-arm64-1.27.1.tgz";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "e42b77a7281cc6e55141bf44fcfbac2c782b823a491bbb6ac33c781dd991f8a6";

// Intel macOS — Microsoft's LAST x86_64 macOS archive (1.24+ is arm64-only, and PyPI's
// wheels stop at the same release). Self-contained: system frameworks only, no Homebrew
// closure, so it drops into the app the way the arm64 dist does.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/onnxruntime-osx-x86_64-1.23.2.tgz";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-win-x64-1.27.1.zip";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "2e00414a63fdef0914cd5a5ede6c707844878e0c08e1b6693842f0451b2df2a1";

// Windows on ARM (native arm64) — Microsoft's official win-arm64 build.
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-win-arm64-1.27.1.zip";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "6e22c2061ba6400b42a59663d700c8694e4e8fe654cf452c4700c24237407ae1";

// Linux x86_64 — Microsoft's official linux-x64 build: a .tgz whose
// lib/libonnxruntime.so.1.27.1 is the dynamic runtime `ort` (load-dynamic) dlopens via
// ORT_DYLIB_PATH (the bare libonnxruntime.so is a symlink; see archive::extract_dylib_member).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-linux-x64-1.27.1.tgz";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "25b1ef1fea1acd210d63f8f24dc870ad6e077795ce1f54876252c6d3803c15af";

// Linux aarch64 — Microsoft's official linux-aarch64 build (same .tgz layout as linux-x64).
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.1/onnxruntime-linux-aarch64-1.27.1.tgz";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "33c67e33d1e25b816878366ea276589a024f71f000e7ff955c4b33224d639edd";

/// The onnxruntime-gpu version shipped for the CUDA path — DELIBERATELY decoupled from the CPU
/// [`ONNXRUNTIME_VERSION`] (1.27.1): onnxruntime-gpu ≥ 1.27 requires CUDA 13 (a newer driver than
/// Pascal-era cards run), so the GPU path stays on the LAST CUDA-12 line. The `CUDA_WHEELS`
/// `onnxruntime_gpu` URL MUST embed this version — enforced by
/// `cuda_pin_tests::cuda_wheels_are_consistent_and_complete`.
pub const CUDA_ONNXRUNTIME_VERSION: &str = "1.26.0";

// ── Windows CUDA GPU runtime — pinned PyPI wheels (each a zip), fetched on demand ──
// onnxruntime-gpu 1.26.0 (the LAST CUDA-12 line; 1.27 drops CUDA 12 for CUDA 13) + CUDA 12.6 +
// cuDNN 9.5.1.17 + curand 10.3.7.77. Kept on the CUDA-12 line so it runs on Pascal-era drivers
// (e.g. 560.x / CUDA 12.6). onnxruntime-gpu DECLARES curand as a CUDA dep, so it MUST be shipped or
// the provider fails to initialize (Win32 1114). (url, sha256) pairs; ~1.5 GB total.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const CUDA_WHEELS: &[(&str, &str)] = &[
    (
        "https://files.pythonhosted.org/packages/ef/26/a417b7a1cdbbf56a389bfcd399255be23f30e5721e3e519472fe8dde9c99/onnxruntime_gpu-1.26.0-cp311-cp311-win_amd64.whl",
        "cc5329aad02d9745cc3ae9cdb185bfa1aad242a7bf89b8c471280002ec40f98a",
    ),
    (
        "https://files.pythonhosted.org/packages/fa/76/4c80fa138333cc975743fd0687a745fccb30d167f906f13c1c7f9a85e5ea/nvidia_cuda_runtime_cu12-12.6.77-py3-none-win_amd64.whl",
        "86c58044c824bf3c173c49a2dbc7a6c8b53cb4e4dca50068be0bf64e9dab3f7f",
    ),
    (
        "https://files.pythonhosted.org/packages/84/f7/985e9bdbe3e0ac9298fcc8cfa51a392862a46a0ffaccbbd56939b62a9c83/nvidia_cublas_cu12-12.6.4.1-py3-none-win_amd64.whl",
        "9e4fa264f4d8a4eb0cdbd34beadc029f453b3bafae02401e999cf3d5a5af75f8",
    ),
    (
        "https://files.pythonhosted.org/packages/7d/ec/ce1629f1e478bb5ccd208986b5f9e0316a78538dd6ab1d0484f012f8e2a1/nvidia_cufft_cu12-11.3.3.83-py3-none-win_amd64.whl",
        "7a64a98ef2a7c47f905aaf8931b69a3a43f27c55530c698bb2ed7c75c0b42cb7",
    ),
    (
        "https://files.pythonhosted.org/packages/a9/a8/0cd0cec757bd4b4b4ef150fca62ec064db7d08a291dced835a0be7d2c147/nvidia_curand_cu12-10.3.7.77-py3-none-win_amd64.whl",
        "6d6d935ffba0f3d439b7cd968192ff068fafd9018dbf1b85b37261b13cfc9905",
    ),
    (
        "https://files.pythonhosted.org/packages/b6/b2/3f60d15f037fa5419d9d7f788b100ef33ea913ae5315c87ca6d6fa606c35/nvidia_cudnn_cu12-9.5.1.17-py3-none-win_amd64.whl",
        "d7af0f8a4f3b4b9dbb3122f2ef553b45694ed9c384d5a75bab197b8eefb79ab8",
    ),
    (
        "https://files.pythonhosted.org/packages/0c/f7/472414aee887d626373d0b2140a59ac4308e3eaed815060e5410fc83305a/nvidia_cuda_nvrtc_cu12-12.6.85-py3-none-win_amd64.whl",
        "a419e2c95e75b88b602f8bb66f82a6c5651e8475a509841c958486b1b71510bf",
    ),
    (
        "https://files.pythonhosted.org/packages/dd/7e/2eecb277d8a98184d881fb98a738363fd4f14577a4d2d7f8264266e82623/nvidia_nvjitlink_cu12-12.9.86-py3-none-win_amd64.whl",
        "cc6fcec260ca843c10e34c936921a1c426b351753587fdd638e8cff7b16bb9db",
    ),
];

/// Exact `Content-Length` (bytes) of each pinned wheel in [`CUDA_WHEELS`] above, SAME
/// order/length — sourced directly from the published file (never estimated), so the
/// byte-weighted progress bar `ort::ensure_cuda_runtime_with_progress` reports is real, not
/// a wheel-count fraction. Kept in sync with `CUDA_WHEELS` by
/// `cuda_pin_tests::cuda_wheels_are_consistent_and_complete`.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const CUDA_WHEEL_SIZES: &[u64] = &[
    226_539_455, // onnxruntime_gpu-1.26.0-cp311-cp311-win_amd64.whl
    891_773,     // nvidia_cuda_runtime_cu12-12.6.77-py3-none-win_amd64.whl
    434_535_301, // nvidia_cublas_cu12-12.6.4.1-py3-none-win_amd64.whl
    192_216_559, // nvidia_cufft_cu12-11.3.3.83-py3-none-win_amd64.whl
    55_783_873,  // nvidia_curand_cu12-10.3.7.77-py3-none-win_amd64.whl
    565_440_743, // nvidia_cudnn_cu12-9.5.1.17-py3-none-win_amd64.whl
    39_026_436,  // nvidia_cuda_nvrtc_cu12-12.6.85-py3-none-win_amd64.whl
    35_584_936,  // nvidia_nvjitlink_cu12-12.9.86-py3-none-win_amd64.whl
];

// ── Linux x86_64 CUDA GPU runtime — the SAME CUDA-12 version combo as Windows, as manylinux
// wheels (each a zip). onnxruntime-gpu 1.26.0 (cuda12) + CUDA 12.6 + cuDNN 9.5.1.17 + curand
// 10.3.7.77 (a DECLARED onnxruntime-gpu dep — must be shipped or the provider fails to init).
// (url, sha256) pairs; ~1.5 GB total. We pull every *.so out (archive::extract_all_sos).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const CUDA_WHEELS: &[(&str, &str)] = &[
    (
        "https://files.pythonhosted.org/packages/dc/0f/696b4f94a282952239ffed39db78cb17a00ad993acd929cfac010a09759b/onnxruntime_gpu-1.26.0-cp311-cp311-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl",
        "4fa231294c2643911d2a7d16469c4808b0bdcdc4b5f4063d3a53744ce25b683a",
    ),
    (
        "https://files.pythonhosted.org/packages/f0/62/65c05e161eeddbafeca24dc461f47de550d9fa8a7e04eb213e32b55cfd99/nvidia_cuda_runtime_cu12-12.6.77-py3-none-manylinux2014_x86_64.whl",
        "a84d15d5e1da416dd4774cb42edf5e954a3e60cc945698dc1d5be02321c44dc8",
    ),
    (
        "https://files.pythonhosted.org/packages/af/eb/ff4b8c503fa1f1796679dce648854d58751982426e4e4b37d6fce49d259c/nvidia_cublas_cu12-12.6.4.1-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
        "08ed2686e9875d01b58e3cb379c6896df8e76c75e0d4a7f7dace3d7b6d9ef8eb",
    ),
    (
        "https://files.pythonhosted.org/packages/1f/13/ee4e00f30e676b66ae65b4f08cb5bcbb8392c03f54f2d5413ea99a5d1c80/nvidia_cufft_cu12-11.3.3.83-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
        "4d2dd21ec0b88cf61b62e6b43564355e5222e4a3fb394cac0db101f2dd0d4f74",
    ),
    (
        "https://files.pythonhosted.org/packages/73/1b/44a01c4e70933637c93e6e1a8063d1e998b50213a6b65ac5a9169c47e98e/nvidia_curand_cu12-10.3.7.77-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.whl",
        "a42cd1344297f70b9e39a1e4f467a4e1c10f1da54ff7a85c12197f6c652c8bdf",
    ),
    (
        "https://files.pythonhosted.org/packages/2a/78/4535c9c7f859a64781e43c969a3a7e84c54634e319a996d43ef32ce46f83/nvidia_cudnn_cu12-9.5.1.17-py3-none-manylinux_2_28_x86_64.whl",
        "30ac3869f6db17d170e0e556dd6cc5eee02647abc31ca856634d5a40f82c15b2",
    ),
    (
        "https://files.pythonhosted.org/packages/f5/31/ffb400c5ae99daf09687aa6c42831c5d824f71c4851363ed2a4a1ac52bab/nvidia_cuda_nvrtc_cu12-12.6.85-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl",
        "800927308ccc5dd6246d3f61f7fcef2ed7ec4e59e199090d360d3293f78bd5a2",
    ),
    (
        "https://files.pythonhosted.org/packages/46/0c/c75bbfb967457a0b7670b8ad267bfc4fffdf341c074e0a80db06c24ccfd4/nvidia_nvjitlink_cu12-12.9.86-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl",
        "e3f1171dbdc83c5932a45f0f4c99180a70de9bd2718c1ab77d14104f6d7147f9",
    ),
];

/// Exact `Content-Length` (bytes) of each pinned wheel in the Linux [`CUDA_WHEELS`] above,
/// SAME order/length — see the Windows `CUDA_WHEEL_SIZES` doc comment for why these are
/// sourced exactly, never estimated.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const CUDA_WHEEL_SIZES: &[u64] = &[
    276_956_564, // onnxruntime_gpu-1.26.0-...-manylinux_2_27_x86_64...whl
    897_678,     // nvidia_cuda_runtime_cu12-12.6.77-...-manylinux2014_x86_64.whl
    393_138_322, // nvidia_cublas_cu12-12.6.4.1-...-manylinux2014_x86_64...whl
    193_118_695, // nvidia_cufft_cu12-11.3.3.83-...-manylinux2014_x86_64...whl
    56_279_010,  // nvidia_curand_cu12-10.3.7.77-...-manylinux2014_x86_64...whl
    570_988_386, // nvidia_cudnn_cu12-9.5.1.17-...-manylinux_2_28_x86_64.whl
    23_649_944,  // nvidia_cuda_nvrtc_cu12-12.6.85-...-manylinux2010_x86_64...whl
    39_748_338,  // nvidia_nvjitlink_cu12-12.9.86-...-manylinux2010_x86_64...whl
];

// ─────────────────────────────────────────────────────────────────────────────
// Library profiles — each downloaded project's LICENSE kept HERE, next to the very
// URLs/digests/sizes it covers, so a file can't drift away from its license. The
// `crate::libraries::catalog()` collector shapes these (plus the cfg-gated ONNX
// Runtime dist + CUDA wheels above) into the cross-platform list the UI's Libraries
// tab renders. A unit test (`crate::libraries` tests) asserts every model download is
// covered by a profile carrying a non-empty license — add a file without a profile
// and CI fails.
// ─────────────────────────────────────────────────────────────────────────────

/// A build target (OS × architecture) the app ships for. The SINGLE place the per-platform
/// rule is expressed as data: every [`Project`] declares which of these it applies to, and
/// `crate::libraries::catalog` filters to [`current_platform`] — so the Libraries tab on each
/// platform shows only what that platform actually downloads, with no scattered `#[cfg]` in
/// the collector. The variants are exactly the targets the app distributes; an unrecognized
/// target resolves to the closest generic one (no GPU/MLX extras).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    WindowsX64,
    WindowsArm64,
    LinuxX64,
    LinuxArm64,
    /// Apple Silicon — the only macOS target with the native MLX Audio path.
    MacArm64,
    /// Intel macOS — no MLX path; uses Microsoft's last x86_64 ONNX Runtime dist,
    /// pinned above at 1.23.2.
    MacX64,
}

impl Platform {
    /// Every target the app distributes — the applicability list for assets present on ALL
    /// platforms (the Kokoro / Parakeet model files). One source so "all platforms" can't
    /// silently fall out of sync with the enum.
    pub const ALL: &'static [Platform] = &[
        Platform::WindowsX64,
        Platform::WindowsArm64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::MacArm64,
        Platform::MacX64,
    ];

    /// Targets with a pinned ONNX Runtime dist. Mirrors `ort::onnxruntime_dist` returning
    /// `Some` — every supported target, Intel macOS on its own older pin.
    pub const WITH_ONNX_RUNTIME: &'static [Platform] = &[
        Platform::WindowsX64,
        Platform::WindowsArm64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::MacArm64,
        Platform::MacX64,
    ];

    /// Targets with the optional NVIDIA CUDA / cuDNN GPU runtime (x64 Windows + Linux only;
    /// never Windows-on-ARM or any Mac). Mirrors the `CUDA_WHEELS` cfg gate.
    pub const WITH_CUDA: &'static [Platform] = &[Platform::WindowsX64, Platform::LinuxX64];

    /// The lone target with the MLX Audio model sets.
    pub const APPLE_MLX: &'static [Platform] = &[Platform::MacArm64];

    /// Both macOS targets — assets the mac app uses regardless of arch (the SepFormer
    /// speaker-lock separator; the lock code is macOS-only, but not MLX-bound).
    pub const MACS: &'static [Platform] = &[Platform::MacArm64, Platform::MacX64];
}

/// The platform THIS build runs on — the ONE place the OS/arch `cfg` lives. Every other
/// per-platform decision is plain data filtered against this value.
pub const fn current_platform() -> Platform {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Platform::WindowsX64
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        Platform::WindowsArm64
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Platform::LinuxX64
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Platform::LinuxArm64
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Platform::MacArm64
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Platform::MacX64
    }
    // Any target the app doesn't ship: treat as a generic portable target — only the
    // all-platform model assets apply (the GPU/MLX lists exclude it, and the
    // file-assembly cfg gates below never materialize CUDA/ONNX files there anyway).
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
    )))]
    {
        Platform::LinuxX64
    }
}

/// Downloaded third-party project for the Libraries tab. `files` empty ⇒ platform-
/// selected assets assembled in the collector (ORT / CUDA / cuDNN).
#[derive(Debug, Clone, Copy)]
pub struct Project {
    pub name: &'static str,
    /// UI subtitle.
    pub usage: &'static str,
    pub homepage: &'static str,
    /// SPDX id when one exists; else vendor name (CUDA/cuDNN aren't SPDX).
    pub license: &'static str,
    pub license_url: &'static str,
    /// Catalog filter — sole per-platform rule source (see [`Platform`]).
    pub platforms: &'static [Platform],
    /// Empty ⇒ platform-selected files assembled in collector.
    pub files: &'static [Download],
}

impl Project {
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms.contains(&platform)
    }
}

/// Kokoro TTS voice model (Apache-2.0).
pub const KOKORO: Project = Project {
    name: "Kokoro",
    usage: "Text-to-speech voice model",
    homepage: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    platforms: Platform::ALL,
    files: &[KOKORO_ONNX, KOKORO_VOICES],
};

/// English grapheme-to-phoneme fallback used only after the contextual Kokoro lexicon misses.
pub const KOKORO_G2P: Project = Project {
    name: "Kokoro English G2P",
    usage: "Unknown-word pronunciation for the Kokoro text frontend",
    homepage: "https://huggingface.co/PeterReid/graphemes_to_phonemes_en_us",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    platforms: Platform::ALL,
    files: &[KOKORO_G2P_ENCODER, KOKORO_G2P_DECODER],
};

/// The platform wheel used by Misaki's eSpeak-backed Kokoro language frontends.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64")
))]
pub const KOKORO_ESPEAK_FRONTEND: Project = Project {
    name: "eSpeak NG",
    usage: "Spanish, French, Hindi, Italian, and Portuguese Kokoro pronunciation",
    homepage: "https://github.com/espeak-ng/espeak-ng",
    license: "GPL-3.0-or-later",
    license_url: "https://github.com/espeak-ng/espeak-ng/blob/master/COPYING",
    platforms: Platform::ALL,
    files: &[ESPEAKNG_LOADER],
};

/// Parakeet TDT 0.6b v3 STT — NVIDIA NeMo (CC-BY-4.0; ONNX by csukuangfj / sherpa-onnx).
pub const PARAKEET: Project = Project {
    name: "Parakeet",
    usage: "Multilingual speech-to-text model (NVIDIA NeMo Parakeet TDT 0.6b v3; ONNX by csukuangfj/sherpa-onnx)",
    homepage: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
    platforms: Platform::ALL,
    files: &[
        PARAKEET_ENCODER,
        PARAKEET_DECODER,
        PARAKEET_JOINER,
        PARAKEET_TOKENS,
    ],
};

/// SepFormer speech separator (Apache-2.0) — the int8 ONNX export of SpeechBrain's
/// sepformer-wsj02mix, published under the project's own HF org (the export predates the
/// public repo; the model card documents provenance). macOS only — the dictation
/// speaker-lock that consumes it is macOS code.
pub const SEPFORMER_PROJECT: Project = Project {
    name: "SepFormer",
    usage: "Speech separation for the dictation speaker-lock",
    homepage: "https://huggingface.co/dellusional/sepformer-wsj02mix-int8-onnx",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    platforms: Platform::MACS,
    files: &[SEPFORMER],
};

/// Boson-licensed Higgs tokenizer files attributed through OmniVoice's partition.
pub const OMNIVOICE_HIGGS_TOKENIZER: Project = Project {
    name: "Higgs Audio 2 tokenizer",
    usage: "Audio-token waveform decoder for OmniVoice",
    homepage: "https://huggingface.co/bosonai/higgs-audio-v2-tokenizer",
    license: "Boson Higgs Audio 2 Community License",
    license_url: "https://github.com/boson-ai/higgs-audio/blob/main/LICENSE",
    platforms: Platform::ALL,
    files: &[],
};

/// DontSpeak's CC-BY-NC OmniVoice backbone re-export, attributed through its partition.
pub const OMNIVOICE_BIDI_EXPORT: Project = Project {
    name: "OmniVoice bidirectional ONNX export",
    usage: "Bidirectional ONNX re-export of the OmniVoice diffusion backbone (plain SDPA forward, 4-D bool mask, no KV cache, embed_tokens dropped)",
    homepage: "https://huggingface.co/dellusional/OmniVoice-ONNX-bidirectional",
    license: "CC-BY-NC-4.0",
    license_url: "https://creativecommons.org/licenses/by-nc/4.0/",
    platforms: Platform::ALL,
    files: &[],
};

/// ONNX Runtime inference library (MIT). Files are platform-selected (the load-dynamic
/// dist archive, plus the GPU build wheel on Windows x64), assembled in the collector.
pub const ONNX_RUNTIME: Project = Project {
    name: "ONNX Runtime",
    usage: "Neural-network inference runtime (runs the STT/TTS models)",
    homepage: "https://github.com/microsoft/onnxruntime",
    license: "MIT",
    license_url: "https://github.com/microsoft/onnxruntime/blob/main/LICENSE",
    platforms: Platform::WITH_ONNX_RUNTIME,
    files: &[],
};

/// NVIDIA CUDA runtime libraries (optional GPU acceleration; NVIDIA CUDA Toolkit EULA).
/// Windows/Linux x64 (see `platforms`). Files (the cuda/cublas/cufft/nvrtc/nvjitlink wheels)
/// are assembled in the collector from the cfg-gated `CUDA_WHEELS`. The METADATA below
/// compiles on every target (plain strings) so the data-driven catalog can reference it
/// unconditionally and filter by `platforms`; only the file-assembly stays cfg-gated.
pub const NVIDIA_CUDA: Project = Project {
    name: "NVIDIA CUDA runtime",
    usage: "GPU acceleration libraries (optional)",
    homepage: "https://developer.nvidia.com/cuda-toolkit",
    license: "NVIDIA CUDA Toolkit EULA",
    license_url: "https://docs.nvidia.com/cuda/eula/index.html",
    platforms: Platform::WITH_CUDA,
    files: &[],
};

/// NVIDIA cuDNN (optional GPU acceleration; NVIDIA cuDNN SLA — separate, stricter terms
/// than the CUDA EULA). Windows/Linux x64 (see `platforms`); the cuDNN wheel from the
/// cfg-gated `CUDA_WHEELS`. Metadata compiles everywhere; only file-assembly is cfg-gated.
pub const NVIDIA_CUDNN: Project = Project {
    name: "NVIDIA cuDNN",
    usage: "GPU deep-learning primitives (optional)",
    homepage: "https://developer.nvidia.com/cudnn",
    license: "NVIDIA cuDNN SLA",
    license_url: "https://docs.nvidia.com/deeplearning/cudnn/sla/index.html",
    platforms: Platform::WITH_CUDA,
    files: &[],
};

// The CUDA wheel-set drift guard runs only where CUDA_WHEELS exists (x64 Windows/Linux); CI's
// full matrix covers the windows-2025 leg, so it executes there.
#[cfg(all(
    test,
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
mod cuda_pin_tests {
    use super::*;
    use crate::download::url_basename;

    /// The CUDA wheel set is INTERNALLY consistent and COMPLETE: the onnxruntime-gpu wheel matches
    /// the pinned [`CUDA_ONNXRUNTIME_VERSION`], every wheel is a CUDA-12 (`_cu12`) build (never a
    /// `cu13` wheel that would need a driver Pascal-era cards can't run), and every CUDA library
    /// onnxruntime-gpu links is actually shipped. This is the anti-drift guard: it FAILS if someone
    /// bumps the onnxruntime-gpu URL without the const, mixes in a cu13 wheel, or DROPS a required
    /// dependency — the exact class of bug that left `nvidia_curand_cu12` un-shipped and broke the
    /// GPU provider's init (Win32 1114) despite the driver + card being fine.
    #[test]
    fn cuda_wheels_are_consistent_and_complete() {
        let bases: Vec<&str> = CUDA_WHEELS.iter().map(|(u, _)| url_basename(u)).collect();

        // onnxruntime-gpu present, and pinned to CUDA_ONNXRUNTIME_VERSION (single source of truth).
        let ort = bases
            .iter()
            .find(|b| b.starts_with("onnxruntime_gpu"))
            .expect("an onnxruntime_gpu wheel in CUDA_WHEELS");
        assert!(
            ort.contains(&format!("onnxruntime_gpu-{CUDA_ONNXRUNTIME_VERSION}-")),
            "onnxruntime-gpu wheel `{ort}` does not match CUDA_ONNXRUNTIME_VERSION `{CUDA_ONNXRUNTIME_VERSION}`"
        );

        // The whole set is the CUDA-12 line — a stray cu13 wheel would need a newer driver.
        for b in &bases {
            assert!(
                !b.contains("cu13"),
                "unexpected CUDA-13 wheel `{b}` — the GPU path is CUDA-12 only (driver ceiling)"
            );
        }

        // Every CUDA library onnxruntime-gpu links MUST be shipped, or the provider fails to init
        // (Win32 1114) — this is exactly the curand gap that motivated the guard.
        for needed in [
            "nvidia_cuda_runtime",
            "nvidia_cublas",
            "nvidia_cufft",
            "nvidia_curand",
            "nvidia_cudnn",
            "nvidia_cuda_nvrtc",
            "nvidia_nvjitlink",
        ] {
            assert!(
                bases.iter().any(|b| b.starts_with(needed)),
                "CUDA wheel set is missing a `{needed}` wheel (onnxruntime-gpu links it)"
            );
        }

        // Every pin is a well-formed (https URL, 64-hex sha256) pair.
        for (u, sha) in CUDA_WHEELS {
            assert!(u.starts_with("https://"), "wheel URL is not https: {u}");
            assert!(
                sha.len() == 64 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
                "malformed sha256 for {u}"
            );
        }

        // `CUDA_WHEEL_SIZES` must stay SAME order/length as `CUDA_WHEELS` — the byte-weighted
        // progress bar zips the two lists positionally, so a drift here would silently
        // misattribute one wheel's size to another.
        assert_eq!(
            CUDA_WHEEL_SIZES.len(),
            CUDA_WHEELS.len(),
            "CUDA_WHEEL_SIZES must have one entry per CUDA_WHEELS wheel, same order"
        );
    }
}
