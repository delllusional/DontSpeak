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
    /// On-disk name (saved flat under `model_dir()`); also the installer's prefetch key.
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

// ── Kokoro TTS — thewh1teagle/kokoro-onnx release `model-files-v1.0` ──────────

pub const KOKORO_ONNX: Download = Download {
    file_name: "kokoro-v1.0.onnx",
    url: "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx",
    sha256: "7d5df8ecf7d4b1878015a32686053fd0eebe2bc377234608764cc0ef3636a6c5",
    size_bytes: 325_532_387,
};

pub const KOKORO_VOICES: Download = Download {
    file_name: "voices-v1.0.bin",
    url: "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin",
    sha256: "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
    size_bytes: 28_214_398,
};

// ── Parakeet TDT 0.6b v2 (English) STT — istupakov/parakeet-tdt-0.6b-v2-onnx ──
// `transcribe-rs` expects all four files flat in ONE dir under exactly these names.

pub const PARAKEET_ENCODER: Download = Download {
    file_name: "encoder-model.int8.onnx",
    url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/encoder-model.int8.onnx",
    sha256: "3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155",
    size_bytes: 652_184_014,
};

pub const PARAKEET_DECODER: Download = Download {
    file_name: "decoder_joint-model.int8.onnx",
    url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/decoder_joint-model.int8.onnx",
    sha256: "a449f49acd68979d418651dd2dcb737cc0f1bf0225e009e29ee326354edbf7d3",
    size_bytes: 8_998_286,
};

pub const PARAKEET_PREPROC: Download = Download {
    file_name: "nemo128.onnx",
    url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/nemo128.onnx",
    sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f",
    size_bytes: 139_764,
};

pub const PARAKEET_VOCAB: Download = Download {
    file_name: "vocab.txt",
    url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/vocab.txt",
    sha256: "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d",
    size_bytes: 9_384,
};

// On-disk file-name aliases — kept as standalone consts because they are part of the
// crate's public API (`ds_model::KOKORO_ONNX_FILE`, …), consumed by callers that
// resolve a path without needing the full `Download`.
pub const KOKORO_ONNX_FILE: &str = KOKORO_ONNX.file_name;
pub const KOKORO_VOICES_FILE: &str = KOKORO_VOICES.file_name;
pub const PARAKEET_ENCODER_FILE: &str = PARAKEET_ENCODER.file_name;
pub const PARAKEET_DECODER_FILE: &str = PARAKEET_DECODER.file_name;
pub const PARAKEET_PREPROC_FILE: &str = PARAKEET_PREPROC.file_name;
pub const PARAKEET_VOCAB_FILE: &str = PARAKEET_VOCAB.file_name;

// ── ONNX Runtime 1.27.0 — microsoft/onnxruntime releases ─────────────────────
// The shared `load-dynamic` inference dylib (Kokoro + Parakeet ONNX paths). The per-OS
// SELECTION + extraction lives in `ort.rs`; this holds the pinned dist URL + digest. Pins
// are 1.27.0 (a NEWER runtime serves the workspace's api-24 request; 1.24.2's loader
// deadlocks on the SepFormer graph). No dist for Intel macOS / Linux.

/// The onnxruntime version the workspace `ort` pin (api-24) needs at runtime.
pub const ONNXRUNTIME_VERSION: &str = "1.27.0";

/// Size (bytes) of the onnxruntime dist archive, for the up-front manifest total. The
/// win-arm64 zip is larger than the osx-arm64 / win-x64 dists, and the linux-x64 tgz is much
/// smaller, so each is sized separately.
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 78_593_089;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 8_831_605;
#[cfg(not(any(
    all(target_os = "windows", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
pub const ONNXRUNTIME_DIST_SIZE_BYTES: u64 = 31_604_221;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-osx-arm64-1.27.0.tgz";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-x64-1.27.0.zip";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc";

// Windows on ARM (native arm64) — Microsoft's official win-arm64 build.
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-arm64-1.27.0.zip";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "a32f2650575b3c20df462e337519fd1cc4105356130d11dba9771c6f374d952f";

// Linux x86_64 — Microsoft's official linux-x64 build: a .tgz whose
// lib/libonnxruntime.so.1.27.0 is the dynamic runtime `ort` (load-dynamic) dlopens via
// ORT_DYLIB_PATH (the bare libonnxruntime.so is a symlink; see archive::extract_dylib_member).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-x64-1.27.0.tgz";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ONNXRUNTIME_DIST_SHA256: &str =
    "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f";

// ── Windows CUDA GPU runtime — pinned PyPI wheels (each a zip), fetched on demand ──
// EXACT version combo, validated together on Pascal (newer combos drop Pascal / fail the
// cuDNN RNN path). (url, sha256) pairs; ~1.4 GB total.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const CUDA_WHEELS: &[(&str, &str)] = &[
    (
        "https://files.pythonhosted.org/packages/d2/07/036825cbe30f91ea8574a18a759beccd0ea31b7b71e17f6a9ee9304b51d2/onnxruntime_gpu-1.24.4-cp311-cp311-win_amd64.whl",
        "1a799a16e5f1ff4d6a9e5f72d750849ab0fe534da8d323ae4a5d8d8bb7daeca8",
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

// ── Linux x86_64 CUDA GPU runtime — the SAME Pascal-validated version combo as Windows, as
// manylinux wheels (each a zip). onnxruntime-gpu 1.24.4 (cuda12) + CUDA 12.6 + cuDNN 9.5.1.17.
// (url, sha256) pairs; ~1.4 GB total. We pull every *.so out (archive::extract_all_sos).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const CUDA_WHEELS: &[(&str, &str)] = &[
    (
        "https://files.pythonhosted.org/packages/9f/13/e080d758f2b60f71abe518c707135fb121d6a3019e0761ead89b5283ac3d/onnxruntime_gpu-1.24.4-cp311-cp311-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl",
        "c2a698659271c28220b3f56fe9b63f70eae3b3c36afa544201bf750b929a36dc",
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

// ─────────────────────────────────────────────────────────────────────────────
// Library profiles — each downloaded project's LICENSE kept HERE, next to the very
// URLs/digests/sizes it covers, so a file can't drift away from its license. The
// `crate::libraries::catalog()` collector shapes these (plus the cfg-gated ONNX
// Runtime dist + CUDA wheels above) into the cross-platform list the UI's Libraries
// tab renders. A unit test (`crate::libraries` tests) asserts every model download is
// covered by a profile carrying a non-empty license — add a file without a profile
// and CI fails.
// ─────────────────────────────────────────────────────────────────────────────

/// A third-party project the app downloads at runtime: what it's for, its license,
/// and the files it pulls. Pure data; `crate::libraries` collects it for the UI. The
/// `files` list is empty for projects whose files are platform-selected (ONNX Runtime,
/// CUDA, cuDNN) — those are assembled per-platform in the collector.
#[derive(Debug, Clone, Copy)]
pub struct Project {
    /// Display name.
    pub name: &'static str,
    /// One-line "what we use it for" (the UI subtitle).
    pub usage: &'static str,
    /// Project homepage / model card.
    pub homepage: &'static str,
    /// SPDX id where one exists (Apache-2.0, MIT, CC-BY-4.0); else the vendor license
    /// NAME (NVIDIA's CUDA/cuDNN agreements aren't SPDX).
    pub license: &'static str,
    /// Canonical URL of the license text.
    pub license_url: &'static str,
    /// The downloads this project contributes (empty ⇒ platform-selected, see above).
    pub files: &'static [Download],
}

/// Kokoro TTS voice model (Apache-2.0).
pub const KOKORO: Project = Project {
    name: "Kokoro",
    usage: "Text-to-speech voice model",
    homepage: "https://github.com/thewh1teagle/kokoro-onnx",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    files: &[KOKORO_ONNX, KOKORO_VOICES],
};

/// Parakeet TDT 0.6B v2 STT model (NVIDIA, CC-BY-4.0; ONNX export by istupakov).
pub const PARAKEET: Project = Project {
    name: "Parakeet TDT 0.6B v2",
    usage: "Speech-to-text model (NVIDIA; ONNX export by istupakov)",
    homepage: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
    files: &[
        PARAKEET_ENCODER,
        PARAKEET_DECODER,
        PARAKEET_PREPROC,
        PARAKEET_VOCAB,
    ],
};

/// ONNX Runtime inference library (MIT). Files are platform-selected (the load-dynamic
/// dist archive, plus the GPU build wheel on Windows x64), assembled in the collector.
pub const ONNX_RUNTIME: Project = Project {
    name: "ONNX Runtime",
    usage: "Neural-network inference runtime (runs the STT/TTS models)",
    homepage: "https://github.com/microsoft/onnxruntime",
    license: "MIT",
    license_url: "https://github.com/microsoft/onnxruntime/blob/main/LICENSE",
    files: &[],
};

/// NVIDIA CUDA runtime libraries (optional GPU acceleration; NVIDIA CUDA Toolkit EULA).
/// Windows/Linux x64. Files (the cuda/cublas/cufft/nvrtc/nvjitlink wheels) assembled in
/// the collector from [`CUDA_WHEELS`].
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub const NVIDIA_CUDA: Project = Project {
    name: "NVIDIA CUDA runtime",
    usage: "GPU acceleration libraries (optional)",
    homepage: "https://developer.nvidia.com/cuda-toolkit",
    license: "NVIDIA CUDA Toolkit EULA",
    license_url: "https://docs.nvidia.com/cuda/eula/index.html",
    files: &[],
};

/// NVIDIA cuDNN (optional GPU acceleration; NVIDIA cuDNN SLA — separate, stricter terms
/// than the CUDA EULA). Windows/Linux x64; the cuDNN wheel from [`CUDA_WHEELS`].
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub const NVIDIA_CUDNN: Project = Project {
    name: "NVIDIA cuDNN",
    usage: "GPU deep-learning primitives (optional)",
    homepage: "https://developer.nvidia.com/cudnn",
    license: "NVIDIA cuDNN SLA",
    license_url: "https://docs.nvidia.com/deeplearning/cudnn/sla/index.html",
    files: &[],
};
