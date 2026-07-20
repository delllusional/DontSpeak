// ABI check: dlopen libdontspeak_mlx.dylib and drive it like the Rust helper.
// Buffer results are BORROWED to a callback fired synchronously during the call (see
// include/dontspeak_mlx.h) — we copy them out there; nothing to free.
#include <dlfcn.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

typedef void (*pcm_cb)(void *, const float *, size_t, int32_t);

typedef int32_t (*init_fn)(const char *, const char *);
typedef int32_t (*syn_fn)(const char *, const char *, const char *, float, void *, pcm_cb);
typedef void (*shutdown_fn)(void);

typedef struct { size_t n; int32_t sr; int calls; int has_pcm; } pcm_out;
static void on_pcm(void *ctx, const float *pcm, size_t n, int32_t sr) {
    pcm_out *o = (pcm_out *)ctx;
    o->n = n; o->sr = sr; o->calls += 1; o->has_pcm = pcm != NULL;
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "./.build/release/libdontspeak_mlx.dylib";
    if (argc <= 2) {
        fprintf(stderr, "usage: ctest <dylib> <rust-managed-kokoro-model-dir>\n");
        return 2;
    }
    const char *model_dir = argv[2];
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }

    init_fn ds_mlx_tts_init = (init_fn)dlsym(h, "ds_mlx_tts_init");
    syn_fn ds_mlx_syn = (syn_fn)dlsym(h, "ds_mlx_tts_synthesize");
    shutdown_fn ds_mlx_shutdown = (shutdown_fn)dlsym(h, "ds_mlx_tts_shutdown");
    if (!ds_mlx_tts_init || !ds_mlx_syn || !ds_mlx_shutdown) { printf("dlsym failed\n"); return 2; }

    int32_t r = ds_mlx_tts_init("kokoro", model_dir);
    printf("ds_mlx_tts_init = %d\n", r);
    if (r != 0) return 3;

    // The shim no longer owns a text frontend: it takes Kokoro IPA produced by ds-tts's shared
    // Rust G2P. Passing English prose here is unsupported and may produce nonsense rather than
    // exercising that contract. This is the IPA for "The shim dylib is speaking through the C ABI."
    pcm_out out = { 0, 0, 0, 0 };
    r = ds_mlx_syn("ðə ʃˈɪm dˈɪlɪb ɪz spˈikɪŋ θɹu ðə sˈi ˈA bˈi ˈI.", "", "en", 1.0f, &out, on_pcm);
    printf("ds_mlx_tts_synthesize = %d  samples=%zu  sample_rate=%d  (%.2fs audio)\n",
           r, out.n, out.sr, out.sr > 0 ? (double)out.n / out.sr : 0.0);

    ds_mlx_shutdown();
    if (r != 0) return r;
    if (out.calls != 1 || !out.has_pcm || out.n == 0 || out.sr != 24000) {
        fprintf(stderr,
                "invalid success callback: calls=%d pcm=%d samples=%zu sample_rate=%d\n",
                out.calls, out.has_pcm, out.n, out.sr);
        return 4;
    }
    return 0;
}
