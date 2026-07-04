// Phase STT-1 ABI check: dlopen libsmkokoro.dylib, load a 16 kHz mono int16 WAV,
// and transcribe it through the C ABI (smk_asr_init + smk_transcribe).
// The transcript is BORROWED to a callback fired synchronously during the call (see
// include/smkokoro.h) — we copy it out there; nothing to free.
#include <dlfcn.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef void (*str_cb)(void *, const char *);

typedef int32_t (*asr_init_fn)(const char *, int32_t);
typedef int32_t (*transcribe_fn)(const float *, size_t, int32_t, void *, str_cb);

// Copies the borrowed transcript out into a fixed buffer for the duration of the callback.
static void on_str(void *ctx, const char *text) {
    char *dst = (char *)ctx;
    dst[0] = '\0';
    if (text) { strncpy(dst, text, 4095); dst[4095] = '\0'; }
}

// Minimal WAV loader: scan for the "data" chunk, read int16 PCM → float [-1,1].
static float *load_wav_16k(const char *path, size_t *out_n) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *buf = malloc(sz);
    fread(buf, 1, sz, f);
    fclose(f);
    long i = 12;  // past "RIFF"<size>"WAVE"
    while (i + 8 <= sz) {
        uint32_t csz = buf[i+4] | (buf[i+5]<<8) | (buf[i+6]<<16) | ((uint32_t)buf[i+7]<<24);
        if (memcmp(buf + i, "data", 4) == 0) {
            int16_t *pcm = (int16_t *)(buf + i + 8);
            size_t n = csz / 2;
            float *out = malloc(n * sizeof(float));
            for (size_t k = 0; k < n; k++) out[k] = pcm[k] / 32768.0f;
            *out_n = n;
            free(buf);
            return out;
        }
        i += 8 + csz + (csz & 1);
    }
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    const char *dylib = argc > 1 ? argv[1] : "./.build/arm64-apple-macosx/release/libsmkokoro.dylib";
    const char *wav = argc > 2 ? argv[2] : "warm16k.wav";

    void *h = dlopen(dylib, RTLD_NOW | RTLD_LOCAL);
    if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
    asr_init_fn smk_asr_init = (asr_init_fn)dlsym(h, "smk_asr_init");
    transcribe_fn smk_transcribe = (transcribe_fn)dlsym(h, "smk_transcribe");
    if (!smk_asr_init || !smk_transcribe) { printf("dlsym failed\n"); return 2; }

    size_t n = 0;
    float *samples = load_wav_16k(wav, &n);
    if (!samples) { printf("could not load %s\n", wav); return 3; }
    printf("loaded %s: %zu samples (%.2fs @16k)\n", wav, n, n / 16000.0);

    int32_t r = smk_asr_init("", 0);
    printf("smk_asr_init = %d (downloads Parakeet on first run)\n", r);
    if (r != 0) return 4;

    char text[4096] = {0};
    r = smk_transcribe(samples, n, 16000, text, on_str);
    printf("smk_transcribe = %d\n", r);
    if (r == 0) printf("TRANSCRIPT: %s\n", text);
    free(samples);
    return r;
}
