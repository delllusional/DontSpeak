// Phase 1 ABI check: dlopen libsmkokoro.dylib and drive it like the Rust helper will.
#include <dlfcn.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>

typedef int32_t (*init_fn)(const char *, int32_t);
typedef int32_t (*syn_fn)(const char *, const char *, float, float **, size_t *, int32_t *);
typedef void (*free_fn)(float *);
typedef void (*shutdown_fn)(void);

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "./.build/release/libsmkokoro.dylib";
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }

    init_fn smk_init = (init_fn)dlsym(h, "smk_init");
    syn_fn smk_syn = (syn_fn)dlsym(h, "smk_synthesize_text");
    free_fn smk_free = (free_fn)dlsym(h, "smk_free");
    shutdown_fn smk_shutdown = (shutdown_fn)dlsym(h, "smk_shutdown");
    if (!smk_init || !smk_syn || !smk_free || !smk_shutdown) { printf("dlsym failed\n"); return 2; }

    int32_t r = smk_init("", 0);
    printf("smk_init = %d\n", r);
    if (r != 0) return 3;

    float *pcm = NULL; size_t n = 0; int32_t sr = 0;
    r = smk_syn("The shim dylib is speaking through the C ABI.", "", 1.0f, &pcm, &n, &sr);
    printf("smk_synthesize_text = %d  samples=%zu  sample_rate=%d  (%.2fs audio)\n",
           r, n, sr, sr > 0 ? (double)n / sr : 0.0);
    if (r == 0 && pcm) smk_free(pcm);

    smk_shutdown();
    return r;
}
