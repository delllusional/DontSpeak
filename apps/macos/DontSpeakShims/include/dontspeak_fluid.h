// libdontspeak_fluid -- C ABI over FluidAudio Core ML / ANE: fluid compute provider
// for TTS, STT, diarization. Apple Silicon only. Shared call contract: dontspeak_shim.h.
// Rust owns G2P + downloads; shim loads offline (ModelHub.offlineMode).
#ifndef DONTSPEAK_FLUID_H
#define DONTSPEAK_FLUID_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- TTS (FluidAudio Core ML / ANE) ------------------------------------------------------

// ANE Kokoro from a DontSpeak-populated local directory.
// compute_units ABI-reserved (ANE preset pinned); pass 0.
int32_t ds_fluid_tts_init(const char *model_dir, int32_t compute_units);

// Rust-supplied Kokoro IPA → mono fp32 PCM via `cb`. Skips FluidAudio G2P so every
// Kokoro backend renders identical phoneme chunks.
int32_t ds_fluid_tts_synthesize_phonemes(const char *phonemes, const char *voice, float speed,
                                         void *ctx, ds_shim_pcm_cb cb);

void ds_fluid_tts_shutdown(void);

// --- ASR (Parakeet TDT v2, FluidAudio Core ML / ANE) -- fluid STT ------------------------

// Load Parakeet TDT v2 (English-only). AsrModels.load strips the last path component and
// re-appends the v2 repo folder — pass the set directory. compute_units ABI-reserved; pass 0.
int32_t ds_fluid_asr_init(const char *model_dir, int32_t compute_units);

// 16 kHz mono f32 → UTF-8 via `cb`. Not initialized → rc 2.
int32_t ds_fluid_transcribe(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_str_cb cb);

void ds_fluid_asr_shutdown(void);

// Buffered ASR (StreamingEouAsrManager, 160 ms chunk): start, push chunks with live
// hypothesis, finish with final decode.

int32_t ds_fluid_asr_stream_start(const char *model_dir);

// Feed 16 kHz mono chunk; `cb` gets current hypothesis. Not started → rc 2.
int32_t ds_fluid_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                                 void *ctx, ds_shim_str_cb cb);

// Flush stream; `cb` gets final transcript.
int32_t ds_fluid_asr_stream_finish(void *ctx, ds_shim_str_cb cb);

// Drop stream state; warm model shared with ds_fluid_transcribe stays. Teardown via
// ds_fluid_asr_shutdown.
void ds_fluid_asr_stream_shutdown(void);

// --- Diarization (pyannote + WeSpeaker, FluidAudio Core ML) -- fluid diarizer ------------
// DiarizerManager is not an actor — shim serializes every call. Same JSON as ds_mlx_diarize.

// Load pyannote_segmentation.mlmodelc + wespeaker_v2.mlmodelc from set directory.
// clustering_threshold 0.1-0.9; <= 0 uses 0.7.
int32_t ds_fluid_diar_init(const char *model_dir, float clustering_threshold);

// 16 kHz mono f32 → UTF-8 JSON via `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps cluster id → WeSpeaker embedding (enrolled-name matching).
int32_t ds_fluid_diarize(const float *samples, size_t n, int32_t sample_rate,
                         void *ctx, ds_shim_str_cb cb);

// WeSpeaker voiceprint from 16 kHz mono f32 (enrollment). Needs diar_init (rc 2).
int32_t ds_fluid_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_pcm_cb cb);

void ds_fluid_diar_shutdown(void);

// Route diagnostics to host log; NULL restores stderr.
void ds_fluid_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_FLUID_H
