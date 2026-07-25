// libdontspeak_mlx -- C ABI over MLX Audio: built-in TTS, Parakeet STT, Sortformer
// diarization. Apple Silicon only. Shared call contract: dontspeak_shim.h.
#ifndef DONTSPEAK_MLX_H
#define DONTSPEAK_MLX_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- TTS (MLX) ---------------------------------------------------------------------------

// Init from DontSpeak-populated local directory.
// model: "kokoro", "chatterbox", "qwen", "omnivoice".
int32_t ds_mlx_tts_init(const char *model, const char *model_dir);

// Model-ready text → mono fp32 PCM. Kokoro text is IPA; Qwen is plain. `speed` is
// Kokoro-only. `params_json` carries Rust-registry settings; unsupported/malformed/NULL
// never fail synthesis.
//
// Suffix versions the call ABI (loader resolves by name only). Bump on signature change
// so skew fails lookup instead of corrupting arguments.
int32_t ds_mlx_tts_synthesize2(const char *text, const char *voice, const char *language,
                            float speed, const char *params_json, void *ctx, ds_shim_pcm_cb cb);

void ds_mlx_tts_shutdown(void);

// --- ASR (Parakeet TDT, MLX) -------------------------------------------------------------

// Load Parakeet TDT v2 (English-only) from required local model directory.
int32_t ds_mlx_asr_init(const char *model_dir, int32_t compute_units);

// 16 kHz mono f32 → UTF-8 via `cb`.
int32_t ds_mlx_transcribe(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_shim_str_cb cb);

void ds_mlx_asr_shutdown(void);

// --- Buffered ASR (MLX Parakeet) ---------------------------------------------------------
// Start utterance, buffer 16 kHz chunks, refresh live hypothesis, final decode at finish.

int32_t ds_mlx_asr_stream_start(const char *model_dir);

// Feed 16 kHz mono chunk; `cb` gets hypothesis (refreshed ~once per second of new audio).
int32_t ds_mlx_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_shim_str_cb cb);

// Flush stream; `cb` gets final transcript.
int32_t ds_mlx_asr_stream_finish(void *ctx, ds_shim_str_cb cb);

// Drop stream state; warm model shared with ds_mlx_transcribe stays. Teardown via
// ds_mlx_asr_shutdown.
void ds_mlx_asr_stream_shutdown(void);

// --- Diarization (Sortformer + WeSpeaker, MLX) -------------------------------------------

// Load segmentation + embedding from required local model root.
// activity_threshold 0.1-0.9; <= 0 uses 0.5.
int32_t ds_mlx_diar_init(const char *model_dir, float activity_threshold);

// 16 kHz mono f32 → UTF-8 JSON via `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps cluster id → WeSpeaker embedding (enrolled-name matching).
int32_t ds_mlx_diarize(const float *samples, size_t n, int32_t sample_rate,
                    void *ctx, ds_shim_str_cb cb);

// WeSpeaker voiceprint from 16 kHz mono f32 (enrollment). Needs diar_init.
int32_t ds_mlx_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_shim_pcm_cb cb);

void ds_mlx_diar_shutdown(void);

// Route diagnostics to host log; NULL restores stderr.
void ds_mlx_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_MLX_H
