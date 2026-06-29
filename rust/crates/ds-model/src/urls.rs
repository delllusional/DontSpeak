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

// ── Parakeet STT: cache-aware STREAMING FastConformer transducer (80ms, int8) ──
// NeMo `stt_en_fastconformer_hybrid_large_streaming_80ms`, transducer branch, exported by
// csukuangfj (sherpa-onnx). This REPLACED the old whole-buffer transcribe-rs Parakeet TDT model
// (see `docs/STREAMING-STT-PLAN.md`); the `built_in` STT engine + config tokens keep the
// "parakeet" name. The `ds-stt::streaming` runner loads all four flat in one dir (~137 MB total).
// 80ms is the lowest-latency sherpa variant (~12 encoder steps/sec) — picked so the ONNX path
// (Windows/Linux, and the macOS `ort_cpu` fallback) updates the live overlay far more often than
// the old 480ms variant; cadence is read from the encoder metadata, so just swapping the files
// retunes it. Same encoder family/size — only the chunk geometry differs.

pub const PARAKEET_ENCODER: Download = Download {
    file_name: "encoder.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8/resolve/main/encoder.int8.onnx",
    sha256: "8b982c67b45e3b735d3fee49a4fea525b3149b450ba437f4c8a933f4aa6744c0",
    size_bytes: 131_507_579,
};

pub const PARAKEET_DECODER: Download = Download {
    file_name: "decoder.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8/resolve/main/decoder.int8.onnx",
    sha256: "76eec598da07c204747a859a723b99077ef0bbdc19ef4b3f51eb43275662475d",
    size_bytes: 3_955_863,
};

pub const PARAKEET_JOINER: Download = Download {
    file_name: "joiner.int8.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8/resolve/main/joiner.int8.onnx",
    sha256: "67f4291dc170fd06b3695a8511f5199fd5965ee3c77cfbd59afc10e145f173f2",
    size_bytes: 1_408_182,
};

pub const PARAKEET_TOKENS: Download = Download {
    file_name: "tokens.txt",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8/resolve/main/tokens.txt",
    sha256: "618dc110fc2213886b52e063ff42329bbdf37a266ca7705184090fa5f39f3131",
    size_bytes: 11_896,
};

// On-disk file-name aliases — kept as standalone consts because they are part of the
// crate's public API (`ds_model::KOKORO_ONNX_FILE`, …), consumed by callers that
// resolve a path without needing the full `Download`.
pub const KOKORO_ONNX_FILE: &str = KOKORO_ONNX.file_name;
pub const KOKORO_VOICES_FILE: &str = KOKORO_VOICES.file_name;
pub const PARAKEET_ENCODER_FILE: &str = PARAKEET_ENCODER.file_name;
pub const PARAKEET_DECODER_FILE: &str = PARAKEET_DECODER.file_name;
pub const PARAKEET_JOINER_FILE: &str = PARAKEET_JOINER.file_name;
pub const PARAKEET_TOKENS_FILE: &str = PARAKEET_TOKENS.file_name;

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

// ── Windows installer prerequisite runtimes ──────────────────────────────────
// The two Microsoft FRAMEWORK runtimes the unpackaged WinUI app needs at launch: the
// .NET Desktop Runtime (the app is framework-dependent `net10.0-windows`) and the Windows
// App Runtime (WinUI/WinAppSDK). The Windows installer downloads these from Microsoft's
// stable aka.ms permalinks and installs them SILENTLY — no winget (winget isn't on PATH in
// the elevated installer context, so the old `winget install` path silently no-op'd and
// left a non-launching app behind). They live HERE so urls.rs stays the ONE registry of
// everything the app fetches; `apps/windows/installer/dontspeak.iss` reads them at the
// download-page step via `ds-helper --print-manifest dotnet|winapp` (so the installer
// hardcodes no URLs). These are permalinks to the LATEST servicing build, so unlike the
// model blobs they are NOT sha-pinned (the bytes roll forward).
//
// WINDOWS_APP_RUNTIME_VERSION must match the `Microsoft.WindowsAppSDK` <PackageReference>
// in apps/windows/winui/DontSpeak.WinUI.csproj (what the app links against). The aka.ms URL
// shape is .../windowsappsdk/{major.minor}/{full}/windowsappruntimeinstall-{arch}.exe.

/// Windows App Runtime version the app links against — keep in sync with the csproj
/// `Microsoft.WindowsAppSDK` PackageReference.
#[cfg(target_os = "windows")]
pub const WINDOWS_APP_RUNTIME_VERSION: &str = "2.2.0";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const DOTNET_DESKTOP_RUNTIME_URL: &str =
    "https://aka.ms/dotnet/10.0/windowsdesktop-runtime-win-x64.exe";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const DOTNET_DESKTOP_RUNTIME_URL: &str =
    "https://aka.ms/dotnet/10.0/windowsdesktop-runtime-win-arm64.exe";

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub const WINDOWS_APP_RUNTIME_URL: &str =
    "https://aka.ms/windowsappsdk/2.2/2.2.0/windowsappruntimeinstall-x64.exe";
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub const WINDOWS_APP_RUNTIME_URL: &str =
    "https://aka.ms/windowsappsdk/2.2/2.2.0/windowsappruntimeinstall-arm64.exe";

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
/// target resolves to the closest generic one (no GPU/Apple-native extras).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    WindowsX64,
    WindowsArm64,
    LinuxX64,
    /// Apple Silicon — the only macOS target with the Core ML / ANE (FluidAudio) path and a
    /// bundled ONNX Runtime dist.
    MacArm64,
    /// Intel macOS — no Apple-native (FluidAudio) path and no ONNX Runtime dist (see
    /// `ort::onnxruntime_dist`); only the portable model assets apply.
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
        Platform::MacArm64,
        Platform::MacX64,
    ];

    /// Targets with a bundled ONNX Runtime dist — everywhere except Intel macOS (and Linux
    /// arm, which the app doesn't ship). Mirrors `ort::onnxruntime_dist` returning `Some`.
    pub const WITH_ONNX_RUNTIME: &'static [Platform] = &[
        Platform::WindowsX64,
        Platform::WindowsArm64,
        Platform::LinuxX64,
        Platform::MacArm64,
    ];

    /// Targets with the optional NVIDIA CUDA / cuDNN GPU runtime (x64 Windows + Linux only;
    /// never Windows-on-ARM or any Mac). Mirrors the `CUDA_WHEELS` cfg gate.
    pub const WITH_CUDA: &'static [Platform] = &[Platform::WindowsX64, Platform::LinuxX64];

    /// The lone target with the Apple-native Core ML / ANE (FluidAudio) model sets.
    pub const APPLE_NATIVE: &'static [Platform] = &[Platform::MacArm64];
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
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Platform::MacArm64
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Platform::MacX64
    }
    // Any target the app doesn't ship (e.g. linux-arm64): treat as a generic portable target —
    // only the all-platform model assets apply (the GPU/Apple-native lists exclude it, and the
    // file-assembly cfg gates below never materialize CUDA/ONNX files there anyway).
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
    )))]
    {
        Platform::LinuxX64
    }
}

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
    /// The platforms this project applies to — the catalog shows it only on these. The
    /// single source of the per-platform rule (see [`Platform`]).
    pub platforms: &'static [Platform],
    /// The downloads this project contributes (empty ⇒ platform-selected, see above).
    pub files: &'static [Download],
}

impl Project {
    /// Whether this project is shown on `platform` (its applicability list contains it).
    pub fn runs_on(&self, platform: Platform) -> bool {
        self.platforms.contains(&platform)
    }
}

/// Kokoro TTS voice model (Apache-2.0).
pub const KOKORO: Project = Project {
    name: "Kokoro",
    usage: "Text-to-speech voice model",
    homepage: "https://github.com/thewh1teagle/kokoro-onnx",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    platforms: Platform::ALL,
    files: &[KOKORO_ONNX, KOKORO_VOICES],
};

/// Parakeet STT model — NVIDIA NeMo cache-aware streaming FastConformer transducer
/// (CC-BY-4.0; ONNX streaming export by csukuangfj / sherpa-onnx).
pub const PARAKEET: Project = Project {
    name: "FastConformer",
    usage: "Speech-to-text model (NVIDIA NeMo; streaming ONNX export by csukuangfj/sherpa-onnx)",
    homepage: "https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/stt_en_fastconformer_hybrid_large_streaming_80ms",
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
/// are assembled in the collector from the cfg-gated [`CUDA_WHEELS`]. The METADATA below
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
/// cfg-gated [`CUDA_WHEELS`]. Metadata compiles everywhere; only file-assembly is cfg-gated.
pub const NVIDIA_CUDNN: Project = Project {
    name: "NVIDIA cuDNN",
    usage: "GPU deep-learning primitives (optional)",
    homepage: "https://developer.nvidia.com/cudnn",
    license: "NVIDIA cuDNN SLA",
    license_url: "https://docs.nvidia.com/deeplearning/cudnn/sla/index.html",
    platforms: Platform::WITH_CUDA,
    files: &[],
};
