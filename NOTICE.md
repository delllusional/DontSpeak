# Third-party notices

DontSpeak is distributed under the [MIT License](LICENSE). It builds on third-party
software and machine-learning models that carry their own licenses. This file records the
attributions those licenses require. DontSpeak's own source stays MIT-only. No GPL or AGPL
code is linked or bundled anywhere (the one GPLv3 tool, espeak-ng, is invoked only as a
separate external process — see "Optional external tool" below). The Linux build does
dynamically link a small number of LGPL-2.1(-or-later) system libraries; see "Linux build:
LGPL system libraries" below for the full disclosure and why this does not affect
DontSpeak's MIT licensing.

## Rust crates of note

- **voice-g2p** (English grapheme-to-phoneme, embeds the misaki dictionary) — **MIT**.
- **ONNX Runtime** Rust bindings (`ort`) — **MIT OR Apache-2.0**.
- **attohttpc** (HTTP client used for model downloads) — **MPL-2.0**. Used as an
  unmodified upstream dependency; its own source files remain under MPL-2.0.
- **grapheme_to_phoneme** (the neural out-of-vocabulary phoneme predictor called from
  `ds-tts`'s G2P fallback path, `rust/crates/ds-tts/src/g2p.rs`) and its **arpabet**
  dependency tree (`arpabet`, `arpabet_cmudict`, `arpabet_parser`, `arpabet_types`) —
  **BSD-4-Clause**, © Brandon Thomas. Per the license's advertising clause, DontSpeak
  includes this required acknowledgement: "This product includes software developed by
  Brandon Thomas (bt@brand.io, echelon@gmail.com)."
- **symphonia** and its bundled format/codec sub-crates (`symphonia-bundle-flac`,
  `symphonia-bundle-mp3`, `symphonia-codec-aac`, `symphonia-codec-pcm`,
  `symphonia-codec-vorbis`, `symphonia-core`, `symphonia-format-isomp4`,
  `symphonia-format-ogg`, `symphonia-format-riff`, `symphonia-metadata`,
  `symphonia-utils-xiph`) — pulled in transitively via `rodio`'s `symphonia-aiff`
  feature (enabled in `ds-tts` and `ds-helper` for the warm-playback sink) — **MPL-2.0**,
  same license family as `attohttpc` above. Used as unmodified upstream dependencies;
  their own source files remain under MPL-2.0.
- **option-ext** (a transitive dependency of `directories`, used by `ds-config` for
  OS data-dir resolution) — **MPL-2.0**. Used unmodified.

### Other permissive licenses in the dependency graph

Enforced per commit by `cargo deny check licenses` (`rust/deny.toml`). These are common,
OSI-approved permissive licenses carried by ordinary transitive dependencies across the
Rust ecosystem — listed here for completeness, not because any of them require special
handling beyond the standard permissive-license notice preservation already satisfied by
each crate's own packaged `LICENSE` file:

- **BSD-2-Clause** — e.g. `mach2`, `zerocopy`.
- **BSD-3-Clause** — e.g. `encoding_rs`, `subtle`, `num_enum`.
- **ISC** — e.g. `ring`, `rustls`, `rustls-webpki`, `inotify`.
- **Zlib** — e.g. `miniz_oxide`, `bytemuck`, the `objc2-*` Apple-framework bindings.
- **0BSD** — `adler2`.
- **CC0-1.0** — `notify`.
- **Unicode-3.0** — the `icu_*` / `zerovec`/`yoke` Unicode data crates pulled in
  transitively for text normalization.
- **BSL-1.0** (Boost Software License) — `clipboard-win`, `error-code`, `ryu`.
- **CDLA-Permissive-2.0** — `webpki-roots` (Mozilla's bundled root certificate data).

Note: `r-efi` declares `MIT OR Apache-2.0 OR LGPL-2.1-or-later` — DontSpeak satisfies
this via the MIT alternative, so no LGPL terms are actually invoked (unlike the four
system libraries below, which are genuinely LGPL and dynamically linked, not
Cargo-graph dependencies at all).

## Linux build: LGPL system libraries (dynamically linked)

On Linux only, DontSpeak dynamically links the following LGPL-licensed system libraries,
resolved by the normal dynamic linker against the `.so` files provided by the user's
system package manager. DontSpeak does not statically link, vendor, modify, or bundle any
of their source or object code:

- **GTK4** (`libgtk-4.so`), used by the Linux host app (`apps/linux/gtk`) —
  **LGPL-2.1-or-later**.
- **libadwaita** (`libadwaita-1.so`), used by the Linux host app (`apps/linux/gtk`) —
  **LGPL-2.1-or-later**.
- **ALSA** (`libasound.so`), used via `cpal`'s Linux audio backend in the engine's
  microphone-capture crate (`rust/crates/ds-stt`) — **LGPL-2.1**.
- **PulseAudio** (`libpulse.so`, `libpulse-simple.so`), used via `libpulse-binding` /
  `libpulse-simple-binding` in the engine's Linux echo-cancellation backend
  (`rust/crates/ds-aec`) — **LGPL-2.1**.

The LGPL explicitly permits dynamic linking of LGPL libraries into a differently-licensed
application without requiring that application to become LGPL, provided the LGPL
components remain independently replaceable and their licensing is disclosed — which this
section does. DontSpeak's own source remains MIT; only the four system libraries listed
above carry LGPL-2.1(-or-later). This applies to the Linux build only: the macOS host's
echo-cancellation backend uses the Voice-Processing I/O AudioUnit and the Windows host uses
WASAPI, both OS-provided APIs with no LGPL involvement.

## Native libraries and models downloaded at runtime

These are fetched to the user's machine on first use; DontSpeak does not redistribute them.
Each carries its upstream license:

- **ONNX Runtime** (Microsoft) — **MIT**.
- **Kokoro-82M** TTS model (hexgrad) — **Apache-2.0**.
- **Parakeet TDT 0.6b v2** STT model (NVIDIA), the macOS Core ML / ANE path — **CC-BY-4.0**.
  Attribution is required: "Parakeet TDT 0.6b v2 © NVIDIA, licensed under CC-BY-4.0."
- **stt_en_fastconformer_hybrid_large_streaming_80ms** STT model (NVIDIA NeMo), the
  cross-platform ONNX path — **CC-BY-4.0**. The streaming ONNX export is by csukuangfj /
  sherpa-onnx. Attribution is required: "stt_en_fastconformer_hybrid_large_streaming_80ms
  © NVIDIA, licensed under CC-BY-4.0; ONNX export © csukuangfj / sherpa-onnx."
  https://github.com/k2-fsa/sherpa-onnx
- **SepFormer speech separator** (the macOS dictation speaker-lock) — an int8 ONNX export
  of SpeechBrain's `sepformer-wsj02mix` model, published with provenance at
  https://huggingface.co/dellusional/sepformer-wsj02mix-int8-onnx. SpeechBrain,
  **Apache-2.0**. https://github.com/speechbrain/speechbrain
- **pyannote** speaker-segmentation model — **MIT**.
- **WeSpeaker** speaker-embedding model — **Apache-2.0**.
- **FluidAudio** (Apple Neural Engine inference for Kokoro/Parakeet/diarization) —
  **Apache-2.0**. https://github.com/FluidInference/FluidAudio
- **NVIDIA CUDA** execution-provider runtime (Windows/Linux GPU path, x86_64) — redistributed
  by the user under NVIDIA's CUDA redistributable EULA.

## Optional external tool

- **espeak-ng** (non-English Kokoro pronunciation) is **GPLv3**. DontSpeak invokes it only as
  a separate external process when present — it is never linked, bundled, or shipped — so it
  does not affect DontSpeak's MIT licensing.
