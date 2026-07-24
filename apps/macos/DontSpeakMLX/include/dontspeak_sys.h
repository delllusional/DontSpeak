// libdontspeak_sys -- C ABI over Apple's on-device system speech recognition (en-US), the
// `system` STT engine. Dependency-free, so this dylib ships on every macOS arch.
// See dontspeak_shim.h for the shared call contract.
#ifndef DONTSPEAK_SYS_H
#define DONTSPEAK_SYS_H

#include "dontspeak_shim.h"

#ifdef __cplusplus
extern "C" {
#endif

// --- System STT (Apple on-device recognition, en-US) -------------------------------------
// macOS 26+: SpeechAnalyzer (no authorization; the on-device model is the only gate).
// macOS 14-25: legacy SFSpeechRecognizer (needs Speech-Recognition authorization).
// Status codes: 0 ready, 1 preparing (26+: model download needed; <26: permission not
// requested yet), 2 no on-device recognition for the locale, 3 macOS too old,
// 4 permission denied (<26 only).

// Current usability WITHOUT prompting (safe for the model-status poll).
int32_t ds_sys_available(void);

// ENABLE the engine, blocking: 26+ downloads the en-US on-device model if needed; <26
// requests Speech-Recognition authorization (the one-time TCC prompt). Then re-checks;
// returns the same status codes as ds_sys_available.
int32_t ds_sys_authorize(void);

// Transcribe 16 kHz mono f32 PCM -> UTF-8 text (on-device batch); `cb` receives the text.
// rc: 0 ok, 1 recognition error.
int32_t ds_sys_transcribe(const float *samples, size_t n, int32_t sample_rate,
                          void *ctx, ds_shim_str_cb cb);

// Streaming system STT -- mirrors the ds_mlx_asr_stream_* / ds_fluid_asr_stream_* trios.

// Begin a new streaming utterance (tears down any prior session). rc 0 ok.
int32_t ds_sys_stream_start(void);

// Feed a 16 kHz mono chunk; `cb` receives the running hypothesis-so-far.
int32_t ds_sys_stream_push(const float *samples, size_t n, int32_t sample_rate,
                           void *ctx, ds_shim_str_cb cb);

// Flush the stream; `cb` receives the final transcript.
int32_t ds_sys_stream_finish(void *ctx, ds_shim_str_cb cb);

// Route this dylib's diagnostics to the host log; NULL restores stderr.
void ds_sys_set_log_cb(ds_shim_log_cb cb);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_SYS_H
