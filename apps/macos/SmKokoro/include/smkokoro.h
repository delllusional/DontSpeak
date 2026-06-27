// libsmkokoro — C ABI over FluidAudio's ANE Kokoro TTS.
// Loaded at runtime by the DontSpeak helper via SMKOKORO_DYLIB_PATH (mirrors ORT_DYLIB_PATH).
// All functions return 0 on success, non-zero on error (details on stderr).
#ifndef SMKOKORO_H
#define SMKOKORO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Initialize the English Kokoro manager (downloads models on first use).
//   model_dir      : optional override dir for models ("" / NULL = ~/.cache/fluidaudio)
//   compute_units  : 0 default(ANE+GPU tail), 1 all-ANE, 2 cpu+GPU, 3 cpu-only, 4 ANE+GPU-tail
int32_t smk_init(const char *model_dir, int32_t compute_units);

// Synthesize text -> 24 kHz mono fp32 PCM. Caller owns *out_pcm; free via smk_free.
//   voice : "" / NULL uses the default English voice (af_heart)
int32_t smk_synthesize_text(const char *text, const char *voice, float speed,
                            float **out_pcm, size_t *out_len, int32_t *out_sample_rate);

void smk_free(float *pcm);
void smk_shutdown(void);

// --- ASR (Parakeet TDT, Core ML / ANE) — the apple-native STT backend ---

// Download (first use) + load Parakeet TDT v2 (English-only) models. "" / NULL = default cache.
int32_t smk_asr_init(const char *model_dir, int32_t compute_units);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text. Caller frees *out_text via smk_free_str.
int32_t smk_transcribe(const float *samples, size_t n, int32_t sample_rate,
                       char **out_text);

void smk_free_str(char *text);
void smk_asr_shutdown(void);

// --- System STT (macOS 26 SpeechAnalyzer, en-US, on-device) — the `system` engine ---
// Status codes: 0 ready (model installed), 1 preparing (download needed), 2 locale unsupported, 3 macOS < 26.

// Current usability WITHOUT prompting (safe for the model-status poll).
int32_t smk_sys_available(void);

// ENABLE the engine: download the en-US on-device model if needed, blocking, then re-check.
// Returns the same status codes as smk_sys_available.
int32_t smk_sys_authorize(void);

// Transcribe 16 kHz mono f32 PCM → UTF-8 text (on-device batch). Caller frees *out_text
// via smk_free_str. rc: 0 ok, 1 recognition error, 3 macOS < 26.
int32_t smk_sys_transcribe(const float *samples, size_t n, int32_t sample_rate,
                           char **out_text);

// --- Diarization (Pyannote + WeSpeaker, Core ML / ANE) — "who spoke when" ---

// Download (first use) + load the segmentation + embedding models. "" / NULL = default
// cache. clustering_threshold tunes speaker splitting (0.5-0.9, lower = more speakers);
// <= 0 uses FluidAudio's default (0.7).
int32_t smk_diar_init(const char *model_dir, float clustering_threshold);

// Diarize 16 kHz mono f32 PCM → UTF-8 JSON
//   {"segments":[{"speaker","start","end"},...], "speakers":{"<id>":[..floats..]}}.
// `speakers` maps each cluster id to its WeSpeaker embedding (for enrolled-name matching).
// Caller frees *out_json via smk_free_str.
int32_t smk_diarize(const float *samples, size_t n, int32_t sample_rate, char **out_json);

// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (enrollment). Requires
// smk_diar_init first. Caller frees *out_floats via smk_free.
int32_t smk_diar_embed(const float *samples, size_t n, int32_t sample_rate,
                       float **out_floats, size_t *out_len);

// Download just the diarization models if absent (for the Settings Download button).
int32_t smk_diar_download(void);

void smk_diar_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif  // SMKOKORO_H
