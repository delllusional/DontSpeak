// libsmkokoro — C ABI over FluidAudio's ANE Kokoro TTS, Parakeet STT, system STT, and
// diarization. Loaded at runtime by the DontSpeak helper via SMKOKORO_DYLIB_PATH (mirrors
// ORT_DYLIB_PATH). All functions BLOCK and return int32_t: 0 on success, non-zero on error
// (details on stderr).
//
// Buffer results (PCM / text / JSON) are NOT returned via an owned out-param the caller frees.
// Instead the call BORROWS the buffer to a completion callback `cb(ctx, …)` that it fires once,
// synchronously, before returning — but only on success (rc 0). The buffer is valid ONLY for
// the duration of that callback, so copy it out there; there is nothing to free. `ctx` is an
// opaque pointer the caller threads through to its callback. (This replaces the old
// `smk_free` / `smk_free_str` ownership-transfer dance.)
#ifndef SMKOKORO_H
#define SMKOKORO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// --- Borrowed-result callbacks ----------------------------------------------------------
// Fired once, synchronously, on the success path. The buffer is valid for the call only.

typedef void (*smk_pcm_cb)(void *ctx, const float *pcm, size_t len, int32_t sample_rate);
typedef void (*smk_str_cb)(void *ctx, const char *text);

// --- TTS (Kokoro, Core ML / ANE) --------------------------------------------------------

// Initialize the English Kokoro manager (downloads models on first use).
//   model_dir      : optional override dir for models ("" / NULL = ~/.cache/fluidaudio)
//   compute_units  : 0 default(ANE+GPU tail), 1 all-ANE, 2 cpu+GPU, 3 cpu-only, 4 ANE+GPU-tail
int32_t smk_init(const char *model_dir, int32_t compute_units);

// Synthesize text -> 24 kHz mono fp32 PCM, delivered to `cb` (sample_rate is 24000).
//   voice : "" / NULL uses the default English voice (af_heart)
int32_t smk_synthesize_text(const char *text, const char *voice, float speed,
                            void *ctx, smk_pcm_cb cb);

void smk_shutdown(void);

// --- ASR (Parakeet TDT, Core ML / ANE) — the apple-native STT backend ---

// Download (first use) + load Parakeet TDT v2 (English-only) models. "" / NULL = default cache.
int32_t smk_asr_init(const char *model_dir, int32_t compute_units);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text, delivered to `cb`.
int32_t smk_transcribe(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, smk_str_cb cb);

void smk_asr_shutdown(void);

// --- Streaming ASR (FluidAudio StreamingEouAsrManager, Core ML / ANE) ---
// Cache-aware streaming: start an utterance, push 16 kHz chunks, finish for the final text.

// Begin a new streaming utterance (loads the streaming model on first use from model_dir).
int32_t smk_asr_stream_start(const char *model_dir);

// Feed a 16 kHz mono chunk; `cb` receives the running hypothesis-so-far.
int32_t smk_asr_stream_push(const float *samples, size_t n, int32_t sample_rate,
                            void *ctx, smk_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t smk_asr_stream_finish(void *ctx, smk_str_cb cb);

// --- System STT (macOS 26 SpeechAnalyzer, en-US, on-device) — the `system` engine ---
// Status codes: 0 ready (model installed), 1 preparing (download needed), 2 locale unsupported, 3 macOS < 26.

// Current usability WITHOUT prompting (safe for the model-status poll).
int32_t smk_sys_available(void);

// ENABLE the engine: download the en-US on-device model if needed, blocking, then re-check.
// Returns the same status codes as smk_sys_available.
int32_t smk_sys_authorize(void);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text (on-device batch); `cb` receives the text.
// rc: 0 ok, 1 recognition error, 3 macOS < 26.
int32_t smk_sys_transcribe(const float *samples, size_t n, int32_t sample_rate,
                           void *ctx, smk_str_cb cb);

// --- Diarization (Pyannote + WeSpeaker, Core ML / ANE) — "who spoke when" ---

// Download (first use) + load the segmentation + embedding models. "" / NULL = default
// cache. clustering_threshold tunes speaker splitting (0.5-0.9, lower = more speakers);
// <= 0 uses FluidAudio's default (0.7).
int32_t smk_diar_init(const char *model_dir, float clustering_threshold);

// Diarize 16 kHz mono f32 PCM → UTF-8 JSON, delivered to `cb`:
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps each cluster id to its WeSpeaker embedding (for enrolled-name matching).
int32_t smk_diarize(const float *samples, size_t n, int32_t sample_rate,
                    void *ctx, smk_str_cb cb);

// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (enrollment), delivered to `cb`
// (sample_rate is irrelevant for an embedding). Requires smk_diar_init first.
int32_t smk_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                       void *ctx, smk_pcm_cb cb);

// Download just the diarization models if absent (for the Settings Download button).
int32_t smk_diar_download(void);

void smk_diar_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif  // SMKOKORO_H
