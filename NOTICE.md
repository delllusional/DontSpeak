# Third-party notices

DontSpeak is distributed under the [MIT License](LICENSE). It builds on third-party
software and machine-learning models that carry their own licenses. This file records the
attributions those licenses require. DontSpeak's own source stays MIT-only. Kokoro's
multilingual frontend downloads and dynamically loads eSpeak NG; the application installers
do not contain that runtime. English dictionary misses use a checksum-pinned ONNX model. The Linux
build dynamically links a small number of LGPL-2.1(-or-later) system libraries; see "Linux build:
LGPL system libraries" below for the full disclosure and why this does not affect
DontSpeak's MIT licensing.

## Rust crates of note

- **voice-g2p** code (English grapheme-to-phoneme) — **MIT**. Its compiled payload embeds
  Misaki's `us_gold.json` and `us_silver.json` pronunciation dictionaries byte-for-byte;
  those data files are **Apache-2.0**. A copy of that license is shipped at
  `licenses/Apache-2.0.txt`; the crate's MIT notice is at `licenses/voice-g2p-MIT.txt`.
- **ONNX Runtime** Rust bindings (`ort`) — **MIT OR Apache-2.0**.
- **attohttpc** (HTTP client used for model downloads) — **MPL-2.0**. Used as an
  unmodified upstream dependency; its own source files remain under MPL-2.0.
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
- **tokenizers** (Hugging Face, used by `ds-tts` for the Chatterbox, Qwen, and OmniVoice
  text frontends; built with default features off, so optional native `onig` is not compiled)
  and its pure-Rust `esaxx-rs` dependency — **Apache-2.0**.

## Swift packages and adapted source

- **MLX Audio Swift** (built-in TTS: Kokoro, Chatterbox, Qwen3-TTS, OmniVoice; Parakeet
  STT; Sortformer diarization) — **MIT**.
  A copy of the pinned tag's license is shipped at `licenses/mlx-audio-swift-MIT.txt`.
  https://github.com/Blaizzy/mlx-audio-swift
- **MLX Swift** (Apple Silicon array and neural-network runtime) — **MIT**.
  A copy of the pinned tag's license is shipped at `licenses/mlx-swift-MIT.txt`.
  https://github.com/ml-explore/mlx-swift
- **MLX Swift LM** (shared model-loading and language-model layers) — **MIT**.
- **FluidAudio** (the optional `fluid` provider: Core ML / ANE Kokoro TTS, Parakeet STT, and
  speaker diarization on Apple Silicon; pinned to 0.15.5) — **Apache-2.0**.
  A copy of the license is shipped at `licenses/Apache-2.0.txt`.
  https://github.com/FluidInference/FluidAudio
- **EventSource** and **yyjson** (transitive networking and JSON support) — **MIT**.
- **Swift Transformers**, **Swift Hugging Face**, **Swift Xet**, **Swift Numerics**,
  **Swift Algorithms**, **Swift Collections**, **Swift Crypto**, **Swift ASN.1**,
  **Swift Certificates**, **Swift Configuration**, **Swift Distributed Tracing**,
  **Swift HTTP Types**, **Swift HTTP Structured Headers**, **Swift Jinja**,
  **Swift Log**, **Swift NIO** and its linked companion packages, **Swift Atomics**,
  **Swift Async Algorithms**, **Swift Service Context**, **Swift Service Lifecycle**,
  **Swift System**, and **Async HTTP Client** — **Apache-2.0**. The app bundle copies
  every available upstream `LICENSE` and `NOTICE` file from the resolved Swift package
  checkouts. SwiftSyntax is used only by a build-time macro target and is not shipped.
- The WeSpeaker feature-extraction implementation is adapted from **speech-swift** —
  **Apache-2.0**. https://github.com/soniqo/speech-swift

### Other permissive licenses in the dependency graph

Checked by the release-only `cargo deny` gate (`rust/deny.toml`); per-commit CI is clippy +
tests. Transitive permissive licenses (each crate ships its own `LICENSE`):

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
- **Unlicense** — `ksni`, the Linux StatusNotifierItem tray implementation.
- **Apache-2.0 WITH LLVM-exception** — `target-lexicon`, used transitively by the Linux
  GTK build's system-dependency discovery.

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

These are fetched to the user's machine on first use; DontSpeak does not redistribute
them, with one disclosed exception noted in the OmniVoice entry below. Each carries its
upstream license:

- **ONNX Runtime** (Microsoft) — **MIT**.
- **eSpeak NG**, fetched in the platform wheel published by `espeakng-loader` and loaded
  in-process for Kokoro's Spanish, French, Hindi, Italian, and Portuguese frontends —
  **GPL-3.0-or-later**. https://github.com/espeak-ng/espeak-ng
- **Kokoro-82M** TTS model (hexgrad; onnx-community FP32 export) — **Apache-2.0**.
  https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX
- **Chatterbox Multilingual** TTS model and default reference voice (Resemble AI,
  onnx-community FP16 language-model profile or mlx-community 8-bit conversion) — **MIT**.
  https://huggingface.co/onnx-community/chatterbox-multilingual-ONNX
- **Qwen3-TTS 12 Hz 0.6B CustomVoice** (Qwen), downloaded as either the onnx-community
  FP16 profile or the mlx-community 8-bit conversion — **Apache-2.0**.
  https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice
- **OmniVoice** TTS model (k2-fsa; onnx-community ONNX export, mlx-community BF16
  conversion). The model **weights** are **CC-BY-NC 4.0** (non-commercial); upstream's
  Apache-2.0 license covers the OmniVoice source code only, not the published weights.
  The `higgs_decoder.onnx` waveform decoder derives from upstream's `audio_tokenizer/`
  (Boson Higgs Audio 2) and carries the **Boson Higgs Audio 2 Community License**,
  which incorporates the Meta Llama 3 Community License (Higgs Audio 2 derives from
  Meta Llama 3). Copies of both agreements ship at
  `licenses/Boson-Higgs-Audio-2-Community-License.txt` and
  `licenses/Meta-Llama-3-Community-License.txt`. As the license requires, this
  product displays: "Built with Higgs Materials licensed from Boson AI USA, Inc.,
  Copyright Boson AI USA, Inc., All Rights Reserved and Meta Llama 3 licensed under
  the Meta Llama 3 Community License, Copyright Meta Platforms, Inc., All Right
  Reserved". Required attribution notices:

  "Meta Llama 3 is licensed under the Meta Llama 3 Community License, Copyright ©
  Meta Platforms, Inc. All Rights Reserved."

  "Boson Higgs Audio 2 is licensed under the Boson Community License, Copyright ©
  Boson AI USA, Inc. All Rights Reserved."

  Use of the Higgs decoder is subject to the Meta Llama 3 Acceptable Use Policy
  (https://llama.meta.com/llama3/use-policy/). Boson's license additionally requires
  an expanded license from Boson AI once a product exceeds 100,000 annual active
  users.
  The disclosed redistribution exception: the LLM backbone
  (`llm_backbone_fp32.onnx` + `.data`) is a bidirectional ONNX re-export of the
  OmniVoice diffusion backbone that this project itself publishes at
  https://huggingface.co/dellusional/OmniVoice-ONNX-bidirectional under the same
  **CC-BY-NC 4.0** terms, crediting k2-fsa/OmniVoice and stating the changes (plain
  SDPA forward, 4-D bool mask, no KV cache, embed_tokens dropped).
  https://huggingface.co/k2-fsa/OmniVoice
- **graphemes_to_phonemes_en_us** tiny BART model (Peter Reid), used for unknown English
  words in the Kokoro frontend — **Apache-2.0**. The upstream model card declares the license
  but documents no training record. The pinned repository's script suggests training from local
  Misaki dictionaries plus regular plurals mined from WikiText, but it does not establish the
  exact lineage of the published en-US weights and is committed with its British-dictionary flag
  enabled. The model's precise training provenance therefore remains unverified.
- **Parakeet TDT 0.6b v3** STT model (NVIDIA NeMo), on every platform — **CC-BY-4.0**.
  macOS uses the MLX conversion; Windows and Linux use the ONNX export by csukuangfj /
  sherpa-onnx. Attribution is required: "Parakeet TDT 0.6b v3 © NVIDIA, licensed under
  CC-BY-4.0; ONNX export © csukuangfj / sherpa-onnx."
  https://github.com/k2-fsa/sherpa-onnx
- **SepFormer speech separator** (the macOS dictation speaker-lock) — an int8 ONNX export
  of SpeechBrain's `sepformer-wsj02mix` model, published with provenance at
  https://huggingface.co/dellusional/sepformer-wsj02mix-int8-onnx. SpeechBrain,
  **Apache-2.0**. https://github.com/speechbrain/speechbrain
- **Streaming Sortformer 4-speaker v2.1** diarization model (NVIDIA), converted to MLX —
  **NVIDIA Open Model License**.
  https://huggingface.co/nvidia/diar_streaming_sortformer_4spk-v2.1
- **WeSpeaker VoxCeleb ResNet34-LM** speaker-embedding model, converted to MLX — **MIT**.
  https://huggingface.co/mlx-community/wespeaker-voxceleb-resnet34-LM
- The optional **FluidAudio `fluid` provider** (Apple Silicon, opt-in) fetches its own pinned
  Core ML model sets, published by FluidInference:
  - **Kokoro-82M** Core ML TTS chain and G2P/lexicon sub-models — **Apache-2.0**.
    https://huggingface.co/FluidInference/kokoro-82m-coreml
  - **Parakeet TDT 0.6b v2** Core ML STT model (NVIDIA NeMo; English only) — **CC-BY-4.0**.
    Attribution: "Parakeet TDT 0.6b v2 © NVIDIA, licensed under CC-BY-4.0."
    https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v2-coreml
  - **Parakeet realtime EOU 120M** Core ML streaming STT model (NVIDIA NeMo) —
    **NVIDIA Open Model License**.
    https://huggingface.co/FluidInference/parakeet-realtime-eou-120m-coreml
  - **Speaker diarization** Core ML set — pyannote segmentation and WeSpeaker v2 embedding —
    **CC-BY-4.0**.
    https://huggingface.co/FluidInference/speaker-diarization-coreml
- **NVIDIA CUDA** execution-provider runtime (Windows/Linux GPU path, x86_64) — redistributed
  by the user under NVIDIA's CUDA redistributable EULA.

## Optional external tools (process-invoked, never linked)

DontSpeak may shell out to a tool the user already has installed. Running a separate program
is not linking, so a copyleft tool in this list does not affect DontSpeak's MIT licensing —
and nothing here is bundled, redistributed, or required.

- **Speech Dispatcher** (`spd-say`) — **GPL-2.0-or-later**. A dormant Linux adapter can invoke
  it as an external process, but the current engine does not expose Linux `System` TTS.
  https://github.com/brailcom/speechd

Kokoro's English frontend keeps `voice-g2p`'s external-process fallback disabled and sends
unknown words to the BART model above. The other five eSpeak-backed languages call the
downloaded shared library directly; no `espeak-ng` executable is spawned.
