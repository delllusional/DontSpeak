// libdontspeak_mlx -- C ABI over MLX Audio: built-in MLX TTS, Parakeet STT, and Sortformer
// diarization. Apple Silicon only. See dontspeak_shim.h for the shared call contract.
#ifndef DONTSPEAK_MLX_H
#define DONTSPEAK_MLX_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- TTS (MLX) ---------------------------------------------------------------------------

// Initialize the selected model from a DontSpeak-populated local directory.
// Supported model names: "kokoro", "chatterbox", "qwen", "omnivoice".
int32_t ds_mlx_tts_init(const char *model, const char *model_dir);

// Synthesize model-ready text to mono fp32 PCM. Kokoro text is IPA; Qwen text is plain text.
// `speed` is Kokoro-only. `params_json` carries resolved Rust-registry settings;
// unsupported keys and malformed or NULL payloads never fail synthesis.
//
// The suffix versions the call ABI because the loader resolves by name only. Bump it
// on signature changes so version skew fails lookup instead of corrupting arguments.
int32_t ds_mlx_tts_synthesize2(const char *text, const char *voice, const char *language,
                            float speed, const char *params_json, void *ctx, ds_shim_pcm_cb cb);

void ds_mlx_tts_shutdown(void);

// --- ASR (Parakeet TDT, MLX) -- the MLX STT backend -----------------------------

// Load Parakeet TDT v2 (English-only) from a required local model directory.
int32_t ds_mlx_asr_init(const char *model_dir, int32_t compute_units);

// Transcribe 16 kHz mono f32 PCM -> UTF-8 text, delivered to `cb`.
int32_t ds_mlx_transcribe(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_shim_str_cb cb);

void ds_mlx_asr_shutdown(void);

// --- Buffered ASR (MLX Parakeet) ---------------------------------------------------------
// Start an utterance, buffer 16 kHz chunks, periodically refresh the live hypothesis,
// then decode once more at finish.

// Begin a new streaming utterance (loads the streaming model on first use from model_dir).
int32_t ds_mlx_asr_stream_start(const char *model_dir);

// Feed a 16 kHz mono chunk; `cb` receives the current hypothesis (refreshed about once
// per second of newly buffered audio).
int32_t ds_mlx_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t ds_mlx_asr_stream_finish(void *ctx, ds_shim_str_cb cb);

// Drop buffered utterance state while preserving the process-global warm model shared
// with ds_mlx_transcribe. ds_mlx_asr_shutdown owns model teardown.
void ds_mlx_asr_stream_shutdown(void);

// --- Diarization (Sortformer + WeSpeaker, MLX) -- "who spoke when" ----------------------

// Load the segmentation + embedding models from the required local model root.
// activity_threshold tunes speaker detection (0.1-0.9); <= 0 uses 0.5.
int32_t ds_mlx_diar_init(const char *model_dir, float activity_threshold);

// Diarize 16 kHz mono f32 PCM -> UTF-8 JSON, delivered to `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps each cluster id to its WeSpeaker embedding (for enrolled-name matching).
int32_t ds_mlx_diarize(const float *samples, size_t n, int32_t sample_rate,
                    void *ctx, ds_shim_str_cb cb);

// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (enrollment), delivered to `cb`
// (sample_rate is irrelevant for an embedding). Requires ds_mlx_diar_init first.
int32_t ds_mlx_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_shim_pcm_cb cb);

void ds_mlx_diar_shutdown(void);

// Route this dylib's diagnostics to the host log; NULL restores stderr.
void ds_mlx_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_MLX_H
