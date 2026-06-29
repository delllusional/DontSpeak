# Cache-aware streaming STT — implementation plan (Tier B)

Status: **DONE / shipped** (branch `fluidaudio-streaming`). The cheap win (adaptive preview back-off)
shipped earlier on `main`. This document is now a record of the *real* fix that followed: process each
audio frame through the encoder **exactly once**, with a cache, instead of re-encoding the whole
open tail on every preview tick.

### Progress
- [x] **Validated reference** (`scripts/streaming-stt/`): Python onnxruntime+kaldi-native-fbank
      prototype reproduces the oracle transcript exactly. Feature config + tensor contract nailed.
- [x] **No config flag** — streaming REPLACED the offline engine outright (unconditional); there is
      no `stt_streaming` toggle.
- [x] **Streaming runner** `ds-stt::streaming` (pure Rust over `ort` + `kaldi-native-fbank`):
      cache-aware encoder + LSTM decoder + joiner greedy decode. Oracle Rust test passes
      (DONTSPEAK_STREAMING_MODEL_DIR) — exact transcript, each frame encoded once. **Cross-platform
      by construction** (shared `ds-stt`, no C build deps).
- [x] **Model download** (`ds-model`): registry entry (url+digest+size) + spec/presence/dir +
      prefetch for the 480ms int8 model (`ds-model/src/spec.rs`, `urls.rs`).
- [x] **Wire into `listen.rs`**: streams chunks through `StreamingModel` unconditionally (both
      callers route to `try_streaming`), replacing the committed/preview re-encode loop; a
      whole-buffer one-shot over the same model stays as fallback.
- [x] **macOS CoreML/ANE** (Phase 3): shipped via FluidAudio's native streaming Core ML
      (`StreamingEouAsrManager`) behind the `ane` provider (`CoremlStreamer`), NOT the ORT CoreML
      EP — see `docs/STREAMING-STT-MACOS-PLAN.md` (Mac-side build + EOU model download still pending).
- [x] **Per-platform builds** — streaming is now the only engine (no flag, no default to flip).

## 1. Why

The old STT path (`ds-stt/parakeet.rs` → `transcribe-rs` `ParakeetModel`) ran the FastConformer
**encoder over the whole buffer on every call**. `transcribe-rs` flagged that model
`supports_streaming: false`; its `encode()` had no cache input tensors. The decoder side already
threaded TDT predictor state (`input_states_1/2`), so only the **encoder** was non-incremental. That
path has since been REPLACED by the cache-aware streaming runner (§4); `transcribe-rs` is now used
only for its `EnergyVad`.

Measured impact (from the `STT listen` debug trace we added): on a 45 s dictation the live overlay
spent **`preview_ms` ≈ 27 s** re-encoding the growing tail — ~4× the real transcript. The back-off
cut that to ~35% of audio-time, but the fundamental tradeoff (snappy overlay vs. wasted compute)
can't be removed without a streaming encoder. This is the documented "redundant buffered inference"
anti-pattern; the fix is cache-aware streaming (encoder keeps `cache_last_channel` /
`cache_last_time`, each chunk encoded once).

## 2. What "done" means

- Live partials produced by feeding fixed audio chunks (e.g. 160 ms) through a **streaming encoder
  session** that carries cache tensors; no whole-tail re-encode.
- `preview_ms` collapses toward ~0 (there is no separate preview pass — partials fall out of the
  same single-pass decode). `wall_ms ≈ audio_ms`, `final_ms` small (already true).
- Overlay latency (TTFB to first partial) ≤ ~200 ms; final latency unchanged or better.
- **Cross-platform by construction**: the runner lives in shared `ds-stt` Rust (compiled into the
  helper on Windows/Linux/macOS). Plan covers the ONNX path (CPU + CUDA EP) AND the macOS
  CoreML/ANE path, with a clean offline fallback everywhere.

## 3. Feasibility / de-risking

- NeMo exports cache-aware streaming with `export-config cache_support=True`, yielding an encoder
  with 3 extra inputs / 3 extra outputs (`cache_last_channel`, `cache_last_time`, lengths), split
  into **encoder + decoder(LSTM) + joiner** — the same decomposition `transcribe-rs` already uses
  for offline Parakeet (encoder + decoder_joint). So the decode/joiner machinery is largely
  reusable; the new part is the cached encoder loop.
- **Reference implementation exists**: `sherpa-onnx` runs NeMo cache-aware FastConformer streaming
  natively over ONNX Runtime, cross-platform CPU. Its `scripts/nemo/...` export + C++/Python runner
  are the blueprint for cache init, chunk size, and right-context handling.
- **Model**: a *streaming* checkpoint is required — the current `parakeet-tdt-0.6b-v2` offline model
  cannot be made streaming by re-export alone. Candidates: `nvidia/stt_en_fastconformer_hybrid_
  large_streaming_multi` (multi-latency, proven with sherpa-onnx) or `nvidia/nemotron-3.5-asr-
  streaming-0.6b`. Accuracy/behavior will differ from today's model — must A/B.

### Known risks (call out before building)
- NeMo cache-aware ONNX export has historically been finicky (open NeMo issues: export failures;
  "high latency with ONNX runtime"). Mitigation: prefer a model sherpa-onnx already ships a working
  export for; reuse their export script verbatim.
- `transcribe-rs` 0.3.11 won't drive a cached encoder (Parakeet flagged non-streaming). We will
  **not** fork it; instead add a small dedicated streaming runner in `ds-stt` using `ort` directly
  (we already depend on `ort` load-dynamic) — encoder+decoder+joiner sessions + cache state. This
  keeps the offline path on `transcribe-rs` untouched as the fallback.
- macOS CoreML streaming is the hardest leg (stateful encoder on ANE). Two options in §5.

## 4. Architecture

New module `ds-stt/src/streaming.rs` (a single file; shared, platform-agnostic core):
- `StreamingEncoder` — owns the 3 `ort` sessions + cache tensors (`cache_last_channel`,
  `cache_last_time`, `cache_last_channel_len`). `reset()` zeroes the cache; `push(chunk_16k) ->
  Vec<EncoderFrame>` runs one encoder step and returns new encoded frames.
- `StreamingDecoder` — TDT/RNNT greedy decode threading predictor state across frames (reuse the
  existing decoder_joint logic; port from `transcribe-rs`'s decode or call its pieces).
- `StreamingSession` — glue: `feed(&[f32]) -> PartialUpdate { text, is_stable }`, `finalize() ->
  String`. Emits the full current hypothesis on change; flicker is mitigated only by withholding an
  unstable audio tail (`StreamSession`'s `TAIL_MARGIN_16K`) — **local-agreement** was not implemented.

Wiring into `listen.rs` (`transcribe_loop`): replace the "drain → accumulate → re-`segment_text`
the tail every tick" loop with "drain → `session.feed(chunk)` → emit returned partial". The VAD
boundary detector stays for endpointing/`final_ms` accounting; committed/preview split disappears
(one pass). The `STTSTATS` trace stays (preview_ms → ~0 proves success).

## 5. Cross-platform rollout (phased)

- **Phase 0 — spike (1 model, CPU only):** vendor sherpa-onnx's export for one streaming model;
  stand up `StreamingEncoder` over `ort` CPU; prove a hard-coded WAV streams correctly and matches
  offline text within tolerance. Gate everything else on this.
- **Phase 1 — ONNX path (Win/Linux/macOS CPU + CUDA EP):** integrate `StreamingSession` into
  `listen.rs` behind a config flag (`stt_streaming = true`, default OFF). CUDA EP reuses the
  existing `stt_wants_cuda()` bootstrap. Validate with the trace on all three OSes.
- **Phase 2 — model assets + download:** add the streaming model to `ds-model` (spec + URLs +
  on-demand download), parallel to the current Parakeet assets. Keep the offline model as fallback.
- **Phase 3 — macOS CoreML/ANE:** decide:
  - (a) **Unify on ONNX**: run the streaming ONNX encoder via ORT's CoreML EP on macOS too, retiring
    the bespoke `coreml.rs` streaming need. Simpler, one runner; ANE coverage depends on ORT CoreML
    EP op support for the cached encoder.
  - (b) **Keep CoreML offline as fallback**: stream via ONNX-CPU on macOS, leave `coreml.rs` for the
    non-streaming path. Lowest risk, leaves ANE perf on the table for streaming.
  Recommendation: try (a); fall back to (b) if the CoreML EP chokes on the cached graph.
- **Phase 4 — make streaming the default** once A/B (accuracy + latency via the trace) is at parity,
  flip `stt_streaming` default ON; keep the offline path as the automatic fallback on load failure.

## 6. Fallback & safety

- There is no `stt_streaming` flag — streaming is unconditional. Any streaming load/inference error
  fail-quiets to a whole-buffer one-shot over the SAME streaming model (`ParakeetTranscriber` feeds
  the whole buffer through `StreamingModel`, then finalizes) — no dictation regression, same
  fail-open philosophy as today.
- That whole-buffer path is the guaranteed baseline on every platform; there is no separate
  `transcribe-rs` model asset.

## 7. Validation (uses the trace we already built)

Per-OS, compare `STT listen` lines before/after on identical phrases:
- `preview_ms` → ~0 (success signal), `wall_ms ≈ audio_ms`, `final_ms` ≤ today.
- TTFB (first PARTIAL) ≤ ~200 ms; transcript WER parity vs offline on a fixed sample set.
- Report P50/P95 per milestone (TTFB / partial cadence / final / RTF), per the STT-latency
  best-practice metrics.

## 8. Effort (rough)

Multi-day. Phase 0–1 the bulk (new runner + decode port + integration); Phase 3 (CoreML) the
biggest unknown. Each phase independently shippable behind the OFF-by-default flag.

## References
- sherpa-onnx — NeMo cache-aware streaming FastConformer (export scripts + ONNX runner):
  https://github.com/k2-fsa/sherpa-onnx (issues #790, #2177, #2918)
- NeMo ONNX export with `cache_support=True`:
  https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/models.html
- Streaming model candidates: `nvidia/stt_en_fastconformer_hybrid_large_streaming_multi`,
  `nvidia/nemotron-3.5-asr-streaming-0.6b`
- NVIDIA cache-aware streaming overview:
  https://huggingface.co/blog/nvidia/nemotron-speech-asr-scaling-voice-agents
- STT latency metrics (TTFB/partials/finals/RTF): https://www.gladia.io/blog/measuring-latency-in-stt
