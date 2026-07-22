// libdontspeak_mlx — C ABI over MLX Audio TTS, Parakeet STT, system STT, and
// diarization. Loaded at runtime by the DontSpeak helper via DONTSPEAK_MLX_DYLIB_PATH (mirrors
// ORT_DYLIB_PATH). All functions BLOCK and return int32_t: 0 on success, non-zero on error
// (details on stderr).
//
// Buffer results (PCM / text / JSON) are NOT returned via an owned out-param the caller frees.
// Instead the call BORROWS the buffer to a completion callback `cb(ctx, …)` that it fires once,
// synchronously, before returning — but only on success (rc 0). The buffer is valid ONLY for
// the duration of that callback, so copy it out there; there is nothing to free. `ctx` is an
// opaque pointer the caller threads through to its callback.
#ifndef DONTSPEAK_MLX_H
#define DONTSPEAK_MLX_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// --- Borrowed-result callbacks ----------------------------------------------------------
// Fired once, synchronously, on the success path. The buffer is valid for the call only.

typedef void (*ds_mlx_pcm_cb)(void *ctx, const float *pcm, size_t len, int32_t sample_rate);
typedef void (*ds_mlx_str_cb)(void *ctx, const char *text);

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
                            float speed, const char *params_json, void *ctx, ds_mlx_pcm_cb cb);

void ds_mlx_tts_shutdown(void);

// --- ASR (Parakeet TDT, MLX) — the MLX STT backend -----------------------------

// Load Parakeet TDT v2 (English-only) from a required local model directory.
int32_t ds_mlx_asr_init(const char *model_dir, int32_t compute_units);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text, delivered to `cb`.
int32_t ds_mlx_transcribe(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_mlx_str_cb cb);

void ds_mlx_asr_shutdown(void);

// --- Buffered ASR (MLX Parakeet) ---------------------------------------------------------
// Start an utterance, buffer 16 kHz chunks, periodically refresh the live hypothesis,
// then decode once more at finish.

// Begin a new streaming utterance (loads the streaming model on first use from model_dir).
int32_t ds_mlx_asr_stream_start(const char *model_dir);

// Feed a 16 kHz mono chunk; `cb` receives the current hypothesis (refreshed about once
// per second of newly buffered audio).
int32_t ds_mlx_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_mlx_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t ds_mlx_asr_stream_finish(void *ctx, ds_mlx_str_cb cb);

// Drop buffered utterance state while preserving the process-global warm model shared
// with ds_mlx_transcribe. ds_mlx_asr_shutdown owns model teardown.
void ds_mlx_asr_stream_shutdown(void);

// --- System STT (Apple on-device recognition, en-US) — the `system` engine ---
// macOS 26+: SpeechAnalyzer (no authorization; the on-device model is the only gate).
// macOS 14–25: legacy SFSpeechRecognizer (needs Speech-Recognition authorization).
// Status codes: 0 ready, 1 preparing (26+: model download needed; <26: permission not
// requested yet), 2 no on-device recognition for the locale, 3 macOS too old,
// 4 permission denied (<26 only).

// Current usability WITHOUT prompting (safe for the model-status poll).
int32_t ds_mlx_sys_available(void);

// ENABLE the engine, blocking: 26+ downloads the en-US on-device model if needed; <26
// requests Speech-Recognition authorization (the one-time TCC prompt). Then re-checks;
// returns the same status codes as ds_mlx_sys_available.
int32_t ds_mlx_sys_authorize(void);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text (on-device batch); `cb` receives the text.
// rc: 0 ok, 1 recognition error.
int32_t ds_mlx_sys_transcribe(const float *samples, size_t n, int32_t sample_rate,
                           void *ctx, ds_mlx_str_cb cb);

// Streaming system STT — mirrors the ds_mlx_asr_stream_* trio above.

// Begin a new streaming utterance (tears down any prior session). rc 0 ok.
int32_t ds_mlx_sys_stream_start(void);

// Feed a 16 kHz mono chunk; `cb` receives the running hypothesis-so-far.
int32_t ds_mlx_sys_stream_push(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, ds_mlx_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t ds_mlx_sys_stream_finish(void *ctx, ds_mlx_str_cb cb);

// --- Diarization (Sortformer + WeSpeaker, MLX) — "who spoke when" ----------------------

// Load the segmentation + embedding models from the required local model root.
// activity_threshold tunes speaker detection (0.1-0.9); <= 0 uses 0.5.
int32_t ds_mlx_diar_init(const char *model_dir, float activity_threshold);

// Diarize 16 kHz mono f32 PCM → UTF-8 JSON, delivered to `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps each cluster id to its WeSpeaker embedding (for enrolled-name matching).
int32_t ds_mlx_diarize(const float *samples, size_t n, int32_t sample_rate,
                    void *ctx, ds_mlx_str_cb cb);

// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (enrollment), delivered to `cb`
// (sample_rate is irrelevant for an embedding). Requires ds_mlx_diar_init first.
int32_t ds_mlx_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, ds_mlx_pcm_cb cb);

void ds_mlx_diar_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_MLX_H
