// Shared C ABI for the three DontSpeak macOS shim dylibs (libdontspeak_sys,
// libdontspeak_mlx, libdontspeak_fluid). Family entry points live in
// dontspeak_sys.h / dontspeak_mlx.h / dontspeak_fluid.h.
//
// Rust dlopens each via DONTSPEAK_{SYS,MLX,FLUID}_DYLIB_PATH (mirrors ORT_DYLIB_PATH).
// All functions BLOCK; int32_t 0 = success, non-zero = error (details via
// ds_shim_log_cb, else stderr).
//
// Buffer results (PCM/text/JSON) are borrowed to a completion callback
// `cb(ctx, ...)` fired once, synchronously, on success only (rc 0). Valid only
// for the duration of that callback — copy out there; nothing to free.
// `ctx` is an opaque pointer the caller threads through.
#ifndef DONTSPEAK_SHIM_H
#define DONTSPEAK_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// --- Borrowed-result callbacks ----------------------------------------------------------
// Fired once, synchronously, on the success path. Buffer valid for the call only.

typedef void (*ds_shim_pcm_cb)(void *ctx, const float *pcm, size_t len, int32_t sample_rate);
typedef void (*ds_shim_str_cb)(void *ctx, const char *text);

// --- Diagnostic sink --------------------------------------------------------------------
// CANONICAL LEVEL TABLE. Hand-mirrored in each Swift *LogLevel and ds-model `forward`;
// no build check catches drift — change here first.

#define DS_SHIM_LOG_DEBUG 0
#define DS_SHIM_LOG_INFO  1
#define DS_SHIM_LOG_WARN  2
#define DS_SHIM_LOG_ERROR 3

// SYNCHRONOUS from arbitrary shim threads; `message` is NUL UTF-8 valid for the call only.
// Register once after dlopen; NULL restores stderr. Shim unlocks before invoke (may fire
// just after deregister). No `ctx`: process-global sink.
typedef void (*ds_shim_log_cb)(int32_t level, const char *message);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_SHIM_H
