// libdontspeak_sys -- C ABI over Apple on-device system speech recognition (en-US),
// the `system` STT engine. Dependency-free; ships on every macOS arch.
// Shared call contract: dontspeak_shim.h.
#ifndef DONTSPEAK_SYS_H
#define DONTSPEAK_SYS_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- System STT (Apple on-device recognition, en-US) -------------------------------------
// 26+: SpeechAnalyzer (no authorization; on-device model is the only gate).
// 14-25: SFSpeechRecognizer (needs Speech-Recognition authorization).
// Status: 0 ready, 1 preparing (26+: model download needed; <26: permission not
// requested), 2 no on-device locale, 3 macOS too old, 4 permission denied (<26 only).

// Usability WITHOUT prompting (safe for model-status poll).
int32_t ds_sys_available(void);

// ENABLE, blocking: 26+ downloads en-US model if needed; <26 requests Speech-Recognition
// TCC. Returns same status codes as ds_sys_available.
int32_t ds_sys_authorize(void);

// 16 kHz mono f32 → UTF-8 (on-device batch) via `cb`. rc: 0 ok, 1 recognition error.
int32_t ds_sys_transcribe(const float *samples, size_t n, int32_t sample_rate,
                          void *ctx, ds_shim_str_cb cb);

// Streaming system STT — mirrors ds_mlx_asr_stream_* / ds_fluid_asr_stream_*.

// Begin streaming utterance (tears down any prior session). rc 0 ok.
int32_t ds_sys_stream_start(void);

// Feed 16 kHz mono chunk; `cb` gets running hypothesis.
int32_t ds_sys_stream_push(const float *samples, size_t n, int32_t sample_rate,
                           void *ctx, ds_shim_str_cb cb);

// Flush stream; `cb` gets final transcript.
int32_t ds_sys_stream_finish(void *ctx, ds_shim_str_cb cb);

// Route diagnostics to host log; NULL restores stderr.
void ds_sys_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_SYS_H
