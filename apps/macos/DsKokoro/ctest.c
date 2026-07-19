// ABI check: dlopen libdskokoro.dylib and drive it like the Rust helper.
// Buffer results are BORROWED to a callback fired synchronously during the call (see
// include/dskokoro.h) — we copy them out there; nothing to free.
#include <dlfcn.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

typedef void (*pcm_cb)(void *, const float *, size_t, int32_t);

typedef int32_t (*init_fn)(const char *, int32_t);
typedef int32_t (*syn_fn)(const char *, const char *, float, void *, pcm_cb);
typedef void (*shutdown_fn)(void);

typedef struct { size_t n; int32_t sr; int calls; int has_pcm; } pcm_out;
static void on_pcm(void *ctx, const float *pcm, size_t n, int32_t sr) {
    pcm_out *o = (pcm_out *)ctx;
    o->n = n; o->sr = sr; o->calls += 1; o->has_pcm = pcm != NULL;
}

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "./.build/release/libdskokoro.dylib";
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }

    init_fn dsk_init = (init_fn)dlsym(h, "dsk_init");
    syn_fn dsk_syn = (syn_fn)dlsym(h, "dsk_synthesize_phonemes");
    shutdown_fn dsk_shutdown = (shutdown_fn)dlsym(h, "dsk_shutdown");
    if (!dsk_init || !dsk_syn || !dsk_shutdown) { printf("dlsym failed\n"); return 2; }

    int32_t r = dsk_init("", 0);
    printf("dsk_init = %d\n", r);
    if (r != 0) return 3;

    // The shim no longer owns a text frontend: it takes Kokoro IPA produced by ds-tts's shared
    // Rust G2P. Passing English prose here is unsupported and may produce nonsense rather than
    // exercising that contract. This is the IPA for "The shim dylib is speaking through the C ABI."
    pcm_out out = { 0, 0, 0, 0 };
    r = dsk_syn("ðə ʃˈɪm dˈɪlɪb ɪz spˈikɪŋ θɹu ðə sˈi ˈA bˈi ˈI.", "", 1.0f, &out, on_pcm);
    printf("dsk_synthesize_phonemes = %d  samples=%zu  sample_rate=%d  (%.2fs audio)\n",
           r, out.n, out.sr, out.sr > 0 ? (double)out.n / out.sr : 0.0);

    dsk_shutdown();
    if (r != 0) return r;
    if (out.calls != 1 || !out.has_pcm || out.n == 0 || out.sr != 24000) {
        fprintf(stderr,
                "invalid success callback: calls=%d pcm=%d samples=%zu sample_rate=%d\n",
                out.calls, out.has_pcm, out.n, out.sr);
        return 4;
    }
    return 0;
}
