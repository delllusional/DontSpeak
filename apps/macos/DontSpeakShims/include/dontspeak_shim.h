// Shared C ABI contract for the three DontSpeak macOS shim dylibs (libdontspeak_sys,
// libdontspeak_mlx, libdontspeak_fluid). Each family declares its own entry points in
// dontspeak_sys.h / dontspeak_mlx.h / dontspeak_fluid.h and includes this file.
//
// The Rust helper dlopens each family by its own path variable (DONTSPEAK_SYS_DYLIB_PATH,
// DONTSPEAK_MLX_DYLIB_PATH, DONTSPEAK_FLUID_DYLIB_PATH -- mirroring ORT_DYLIB_PATH). All
// functions BLOCK and return int32_t: 0 on success, non-zero on error; details go to the
// registered ds_shim_log_cb, or stderr when none is registered.
//
// Buffer results (PCM / text / JSON) are NOT returned via an owned out-param the caller frees.
// Instead the call BORROWS the buffer to a completion callback `cb(ctx, ...)` that it fires
// once, synchronously, before returning -- but only on success (rc 0). The buffer is valid
// ONLY for the duration of that callback, so copy it out there; there is nothing to free.
// `ctx` is an opaque pointer the caller threads through to its callback.
#ifndef DONTSPEAK_SHIM_H
#define DONTSPEAK_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// --- Borrowed-result callbacks ----------------------------------------------------------
// Fired once, synchronously, on the success path. The buffer is valid for the call only.

typedef void (*ds_shim_pcm_cb)(void *ctx, const float *pcm, size_t len, int32_t sample_rate);
typedef void (*ds_shim_str_cb)(void *ctx, const char *text);

// --- Diagnostic sink --------------------------------------------------------------------
// THIS FILE IS THE CANONICAL LEVEL TABLE. The four integers are hand-mirrored in three
// places -- here, each Swift target's *LogLevel enum, and ds-model's `forward` -- and no
// build check can catch drift between them, so change them here first.

#define DS_SHIM_LOG_DEBUG 0
#define DS_SHIM_LOG_INFO  1
#define DS_SHIM_LOG_WARN  2
#define DS_SHIM_LOG_ERROR 3

// Fires SYNCHRONOUSLY from arbitrary shim threads (Core ML, MLX, Swift concurrency);
// `message` is NUL-terminated UTF-8 valid only for the call, so copy it out there.
// Registered once per process immediately after dlopen; NULL restores the stderr fallback.
// The shim releases its own lock before invoking this, so a sink may fire just after being
// deregistered. No `ctx`: the sink is process-global, so a context pointer would have to
// outlive the dylib for no benefit.
typedef void (*ds_shim_log_cb)(int32_t level, const char *message);

#ifdef __cplusplus
}
#endif

#endif  // DONTSPEAK_SHIM_H
