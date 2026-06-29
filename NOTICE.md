# Third-party notices

DontSpeak is distributed under the [MIT License](LICENSE). It builds on third-party
software and machine-learning models that carry their own licenses. This file records the
attributions those licenses require. Nothing here is strong copyleft (no GPL/LGPL/AGPL is
linked or bundled), so DontSpeak itself remains MIT.

## Bundled in this repository

- **SepFormer speech separator** — `apps/macos/models/sepformer_int8.onnx` is an int8 ONNX
  export of SpeechBrain's `sepformer-wsj02mix` model. SpeechBrain, **Apache-2.0**.
  https://github.com/speechbrain/speechbrain

## Rust crates of note

- **voice-g2p** (English grapheme-to-phoneme, embeds the misaki dictionary) — **MIT**.
- **ONNX Runtime** Rust bindings (`ort`) — **MIT OR Apache-2.0**.
- **attohttpc** (HTTP client used for model downloads) — **MPL-2.0**. Used as an
  unmodified upstream dependency; its own source files remain under MPL-2.0.

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
- **pyannote** speaker-segmentation model — **MIT**.
- **WeSpeaker** speaker-embedding model — **Apache-2.0**.
- **FluidAudio** (Apple Neural Engine inference for Kokoro/Parakeet/diarization) —
  **Apache-2.0**. https://github.com/FluidInference/FluidAudio
- **NVIDIA CUDA** execution-provider runtime (Windows GPU path) — redistributed by the user
  under NVIDIA's CUDA redistributable EULA.

## Optional external tool

- **espeak-ng** (non-English Kokoro pronunciation) is **GPLv3**. DontSpeak invokes it only as
  a separate external process when present — it is never linked, bundled, or shipped — so it
  does not affect DontSpeak's MIT licensing.
