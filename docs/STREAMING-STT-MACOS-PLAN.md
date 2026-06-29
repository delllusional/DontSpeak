# macOS streaming STT (FluidAudio) — plan

Status: **IN PROGRESS** (branch `fluidaudio-streaming`). Extends the cache-aware streaming win to
the macOS Apple-ML engine, with MAXIMUM reuse of the ONNX streaming plumbing.

## Why
The ONNX (`ort_cpu`/`ort_cuda`) "parakeet" engine now streams (each frame encoded once). The macOS
**apple-native (ANE / Core ML, via FluidAudio)** engine does NOT: the shim
(`apps/macos/SmKokoro/Sources/smkokoro/shim.swift`) builds a batch `AsrManager` and exposes one
whole-buffer C call `smk_transcribe(pcm,n)→text`, driven by the offline `transcribe_loop` — so it
still re-encodes the open tail per preview tick (only softened by the adaptive back-off).

FluidAudio supports streaming natively (`StreamingAsrManager` + the `parakeet-realtime-eou-120m`
Core ML model, configurable chunk size + built-in end-of-utterance), so the fix is to *use* it —
not re-export a model.

## Reuse-first architecture (the core of this change)
Everything except the per-backend inference is shared between ONNX and FluidAudio:

- **`trait ds_stt::StreamingStt`** (object-safe): `accept_16k(&[f32]) -> hypothesis`,
  `finalize() -> final`, `transcribe_ms()`. The ONLY backend-specific surface.
- **`ds_stt::StreamSession`** — SHARED plumbing wrapping a `Box<dyn StreamingStt>`: device-rate →
  16 kHz resample (one-shot, tail withheld until stable), `audio_ms` accounting. Both backends get
  clean 16 kHz; neither resamples.
- **Helper `try_streaming`** (`ds_helper/listen.rs`) — SHARED loop: drain → `session.accept` →
  emit `PARTIAL` on change → on stop `finalize` → emit the SAME `STTSTATS` (`preview_ms=0
  streaming=1 …`) + `FINAL`. Picks the backend by provider: `ort_*` → ONNX, `ane` → Core ML.

Backends:
- **ONNX**: `OnnxStreamer { StreamingModel, StreamingState }` impls `StreamingStt` (the existing
  cache-aware encoder/decoder/joiner; resampling lifted out to `StreamSession`).
- **Core ML** (macOS): `CoremlStreamer` impls `StreamingStt` over new shim FFI.

## Layers to change
1. **ds-stt** (cross-platform): add `StreamingStt` + `StreamSession`; refactor `StreamingModel` to
   `accept_16k` (resampling moved to `StreamSession`); add `OnnxStreamer`. `ParakeetTranscriber`
   (whole-buffer) now feeds `accept_16k` directly. — ✅ **done + ONNX oracle green on Windows**.
2. **Swift shim** (`shim.swift`, macOS): added `smk_asr_stream_start(modelDir)` /
   `smk_asr_stream_push(pcm,n)→partial` / `smk_asr_stream_finish()→final` over FluidAudio's
   `StreamingEouAsrManager` (`loadModels`/`process`/`finish`/`reset`). Kept `smk_transcribe`. —
   ✅ **written, MAC-TODO: build + confirm `process()` partial behaviour / model path**.
3. **ds-stt::coreml** (macOS): `CoremlStreamer` binds the three symbols + impls `StreamingStt`. —
   ✅ **written, compiles only on macOS (cfg-gated)**.
4. **helper**: `try_streaming` builds the backend by provider (`ort_*`→ONNX, `ane`→Core ML),
   caches it, runs the shared `StreamSession`. — ✅ **done**.
5. **ds-model**: add the streaming EOU Core ML model to the macOS download (`coreml_repo.rs`). —
   ⏳ **MAC-TODO**: needs the `parakeet-realtime-eou-120m-coreml` repo's exact `.mlmodelc` file
   list + a pinned revision (inspect on HF/Mac); then a `CoremlRepo` entry + download trigger.
   **Open gap:** `coreml_repo.rs` today has only `PARAKEET_COREML` (non-streaming
   `parakeet-tdt-0.6b-v2-coreml`), and the shim's `smk_asr_stream_start` is currently handed THAT
   v2 model dir — so the streaming start runs against the wrong (non-EOU) model until this lands.

## What's verified vs. pending
- ✅ Verified on Windows: the reuse refactor (`StreamingStt`/`StreamSession`/`OnnxStreamer`), the
  provider-routed cached backend in the helper, ONNX oracle + full test suite, fmt + clippy.
- ⏳ Needs a Mac: build the Swift shim streaming functions, confirm the `StreamingEouAsrManager`
  API specifics (esp. whether `process()` yields partials or only `finish()`/EOU does), wire the
  EOU model download (step 5), and dictate-test on the ANE.

## Caveats
- Exact `StreamingAsrManager` Swift signatures need confirming from FluidAudio source ON A MAC.
- The EOU model is English-only **120M** (vs v2 0.6B) — smaller/faster; A/B accuracy.
- Steps 2–4 need a **Mac to build + test**; this branch lands the Rust-side reuse + the Mac code
  cfg-gated, verified only for the ONNX path here.

## References
- FluidAudio: https://github.com/FluidInference/FluidAudio
- Streaming EOU model: `FluidInference/parakeet-realtime-eou-120m-coreml`
