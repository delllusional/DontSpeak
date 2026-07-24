// libdontspeak_fluid -- C ABI over FluidAudio's Core ML / ANE stack: the `fluid` compute
// provider for TTS, STT, and diarization. This dylib is only built and bundled on Apple
// Silicon, so an Intel app exports none of these. See dontspeak_shim.h for the shared call
// contract. Rust owns G2P and pre-downloads every model; the shim loads offline
// (ModelHub.offlineMode).
#ifndef DONTSPEAK_FLUID_H
#define DONTSPEAK_FLUID_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- TTS (FluidAudio Core ML / ANE) ------------------------------------------------------

// Initialize FluidAudio's ANE Kokoro chain from a DontSpeak-populated local directory.
// compute_units is ABI-reserved (the recommended ANE preset is pinned this release); pass 0.
int32_t ds_fluid_tts_init(const char *model_dir, int32_t compute_units);

// Synthesize Rust-supplied Kokoro IPA phonemes to mono fp32 PCM, delivered to `cb`. `voice`
// is an ANE voice-pack id; `speed` is a Kokoro rate multiplier. This skips FluidAudio's own
// G2P so every Kokoro backend renders from identical phoneme chunks.
int32_t ds_fluid_tts_synthesize_phonemes(const char *phonemes, const char *voice, float speed,
                                         void *ctx, ds_shim_pcm_cb cb);

void ds_fluid_tts_shutdown(void);

// --- ASR (Parakeet TDT v2, FluidAudio Core ML / ANE) -- the `fluid` STT backend -----------

// Load Parakeet TDT v2 (English-only) from a required local model directory. FluidAudio's
// AsrModels.load(from:version:.v2) strips the last path component and re-appends the v2 repo
// folder, so pass the set directory itself. compute_units is ABI-reserved; pass 0.
int32_t ds_fluid_asr_init(const char *model_dir, int32_t compute_units);

// Transcribe 16 kHz mono f32 PCM -> UTF-8 text, delivered to `cb`. Not initialized -> rc 2.
int32_t ds_fluid_transcribe(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_str_cb cb);

void ds_fluid_asr_shutdown(void);

// Buffered ASR (StreamingEouAsrManager, 160 ms chunk): start an utterance, buffer 16 kHz
// chunks and periodically refresh the live hypothesis, then decode once more at finish.

// Begin a new streaming utterance (loads the streaming model on first use from model_dir).
int32_t ds_fluid_asr_stream_start(const char *model_dir);

// Feed a 16 kHz mono chunk; `cb` receives the current hypothesis. Not started -> rc 2.
int32_t ds_fluid_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                                 void *ctx, ds_shim_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t ds_fluid_asr_stream_finish(void *ctx, ds_shim_str_cb cb);

// Drop buffered utterance state while preserving the process-global warm model shared with
// ds_fluid_transcribe. ds_fluid_asr_shutdown owns model teardown.
void ds_fluid_asr_stream_shutdown(void);

// --- Diarization (pyannote + WeSpeaker, FluidAudio Core ML) -- the `fluid` diarizer -------
// DiarizerManager is not an actor, so the shim serializes every call. Emits the SAME JSON
// contract as ds_mlx_diarize.

// Load pyannote_segmentation.mlmodelc + wespeaker_v2.mlmodelc from a required local set
// directory. clustering_threshold tunes speaker separation (0.1-0.9); <= 0 uses 0.7.
int32_t ds_fluid_diar_init(const char *model_dir, float clustering_threshold);

// Diarize 16 kHz mono f32 PCM -> UTF-8 JSON, delivered to `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps each cluster id to its WeSpeaker embedding (for enrolled-name matching).
int32_t ds_fluid_diarize(const float *samples, size_t n, int32_t sample_rate,
                         void *ctx, ds_shim_str_cb cb);

// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (enrollment), delivered to `cb`
// (sample_rate is irrelevant for an embedding). Requires ds_fluid_diar_init first (rc 2).
int32_t ds_fluid_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_pcm_cb cb);

void ds_fluid_diar_shutdown(void);

// Route this dylib's diagnostics to the host log; NULL restores stderr.
void ds_fluid_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_FLUID_H
