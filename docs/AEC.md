# AEC — Full-Duplex TTS/STT (Acoustic Echo Cancellation)

Status: **macOS + Windows + Linux implemented** (`ds-aec/src/macos.rs` — a `VoiceProcessingIO`
AudioUnit; `ds-aec/src/windows.rs` — WASAPI Communications-category capture, see
`FULL-DUPLEX-PORT.md`; `ds-aec/src/linux.rs` — a PulseAudio/PipeWire `module-echo-cancel`
capture-side source); the `src/stub.rs` no-op is only for other platforms (half-duplex).
Implementation order was macOS first, then Windows, then Linux. Paths below are relative to
the repo root; the Rust workspace is under `rust/`.

## 1. Goal

Today DontSpeak never lets TTS and STT collide because it is **strictly
half-duplex**: the mic is *closed* whenever TTS is playing. That works, but it
means the voice is deaf while it speaks — you cannot interrupt it by voice, and
"always listening" pauses during every reply.

The goal of this work is **full-duplex**: keep the mic open *while* TTS plays,
without the recogniser hearing the TTS. That requires **acoustic echo
cancellation** — removing the played-back (far-end) signal from the captured
(near-end) mic signal. The payoff is **coexist**: dictate / interrupt by Caps tap
*while* the voice is still speaking, with the recogniser hearing only you.

Non-goal: replacing the half-duplex gate. The gate stays as the **fallback** when
AEC is unavailable or disabled (§8). AEC is a layer on top, never a hard
dependency.

**Scope — Parakeet STT only.** Full-duplex/AEC applies *only* to the local
Parakeet path (`HelperStt`), the one place WE open the mic (the helper's `listen`
job) and play TTS. The **`claude_code` (Claude-Code-dictation) path stays exactly as
today**: it does not capture the mic in our process — Claude Code records — so opening a
VPIO unit there would wrongly seize the mic *and* the default output device and break that
dictation, with nothing to cancel anyway. Therefore the engine enables AEC
(passes `DONTSPEAK_FULL_DUPLEX` to the warm helper) **only when `full_duplex` is on
AND STT is Parakeet AND TTS is on (Kokoro)** — full-duplex only earns its keep
when we are both listening *and* speaking; with TTS off there is no echo to cancel,
so opening VPIO would seize the output device and take the mic gain hit for
nothing. The engine **restarts the warm helper when that resolved mode changes**
(the same restart pattern `TtsManager::set_provider` already uses). The helper code is safe either way — `run_listen` is only ever called by
the Parakeet path, and with the flag unset the rodio/cpal half-duplex path is
byte-identical to today.

## 2. Current state

Strict half-duplex gating is the baseline AEC layers on top of: `listener.rs`
`Listener::tick(tts_busy)` drops the cpal `Capture` while TTS is busy; `ttsq.rs`'s worker
checks `ds_platform::mic_active()` and holds narration while the mic is live;
`lib.rs::spawn_mic_barge_watcher` barges on the idle→active edge of `mic_active()`. Capture
(cpal `default_input_device()` → mono f32 → rubato resample to 16k → Parakeet) and playback
(Kokoro 24 kHz mono → rodio sink, or macOS `afplay` one-shot fallback) go through
**different libraries on different clocks**, and the warm helper (`ds_helper/serve.rs`)
runs them **mutually exclusive** (one `Job` at a time, blocking in `sleep_until_end()`).
That cross-clock split is exactly what makes a userspace canceller hard — we avoid it by
using the OS where it owns both streams.

## 3. The one constraint that dictates the design

AEC builds an adaptive filter of the speaker→air→mic echo path; to converge it
must compare the **far-end reference** (what we play) against the **near-end
capture** (the mic) **time-aligned**, on a common clock. If they drift or the
delay estimate is wrong, the filter never converges and echo leaks through.

- **Native OS AEC owns both streams** in one engine/clock domain → alignment is
  free. This is why we prefer native per platform.
- **Userspace WebRTC APM** makes *us* feed both streams + a delay estimate +
  resample to a common rate + handle clock drift. It's the cross-platform
  fallback, not the first choice.

## 4. Strategy per platform

| Platform | Approach | TTS path change | Capture path change | Effort |
|---|---|---|---|---|
| **macOS** | Native **Voice-Processing I/O** AudioUnit (`kAudioUnitSubType_VoiceProcessingIO`) — ONE unit renders TTS *and* captures mic; AEC built in | **Yes** — render through the unit instead of afplay/rodio | **Yes** — capture from the unit instead of cpal | **High** |
| **Windows** | Native: open mic capture in **Communications category** (`IAudioClient2::SetClientProperties`, `AudioCategory_Communications`, not RAW). OS supplies the render loopback reference + Win11 Voice Clarity | None | Swap cpal capture → WASAPI communications capture | Low–Med |
| **Linux** | Server-side `module-echo-cancel` / PipeWire `libspa-aec-webrtc` (WebRTC under the hood). App records the cancelled virtual source | None | Record the named cancelled source | Low (config) |
| **All (fallback)** | **`webrtc-audio-processing`** (tonarino, `bundled`): feed `process_render_frame(TTS)` + `process_capture_frame(mic)`, track delay | Tap render | In-process APM before Parakeet | Med |

Note the asymmetry: **Windows & Linux are capture-side only and don't touch the
TTS path.** **macOS is the crux** because native VPIO must own *both* streams in
one AudioUnit, replacing both afplay/rodio and the cpal capture.

## 5. macOS design

### 5.1 Crate `ds-aec`

A small crate owning the platform duplex-audio primitive. The macOS impl wraps a single
`coreaudio::audio_unit::AudioUnit` of `IOType::VoiceProcessingIO` exposing a `DuplexAudio`
type: `open()`, `capture_rate()`, `render_push(pcm_24k)` (far-end reference, non-blocking
into a lock-free render ring), `capture_drain()` (echo-cancelled mono f32 since last call),
`render_pending()`, `render_clear()`. The render/input callbacks run on the CoreAudio
**realtime thread**, so each direction uses a lock-free SPSC ring (`ringbuf::HeapRb`) — no
`Mutex` on the audio path — and the 24k↔unit↔16k streaming resamplers run on the **helper**
thread, never in the callbacks. VPIO is opinionated (forces mono, ~48 kHz): read back the
negotiated `sample_rate()` rather than assuming it. Open VPIO only when full-duplex is
wanted (it changes channels/gain/ducking session-wide); on split devices (e.g. AirPods mic
→ MacBook speakers) detect the aggregate mismatch and fall back to half-duplex. Per the
macOS-26 CoreAudio teardown abort, keep the `!Send` `AudioUnit` on the helper's playback
thread and `_exit` on quit rather than dropping it.

### 5.3 The `mic_active()` hazard (rationale)

`ds_platform::mic_active()` (macOS = CoreAudio
`kAudioDevicePropertyDeviceIsRunningSomewhere`) reads **true whenever any input stream is
live**. With an always-on VPIO unit it is **true for the helper's entire lifetime**, which
breaks BOTH `mic_active()`-keyed gates:

1. **TTS hold-gate** in `ttsq.rs`: permanently-true ⇒ all narration dropped, every reply
   delayed `MIC_WAIT_MAX` then played anyway — defeating the whole point of full-duplex.
2. **Mic-barge watcher** in `lib.rs`: the idle→active edge fires once at VPIO open and
   `prev` sticks true forever ⇒ no further barge, `resume()` never fires.

**Therefore: in full-duplex mode, BOTH `mic_active()`-based mechanisms must be
bypassed.** The TTS worker must not hold on `mic_active()`, and barge must be
driven instead from the **AEC-cleaned `capture_drain` energy** (the `listen.rs`
`Endpointer`/`frame_rms`). Likewise `listener.rs::gate_off` must become a no-op
when AEC is active. This is gated by the `full_duplex` config (§8) so the
half-duplex code paths are untouched when AEC is off.

### 5.4 Self-barge-from-echo risk

AEC is *suppression*, not perfect cancellation; residual echo remains (worse with any
make-up gain). If the barge endpointer is fed the cleaned `capture_drain` naively, residual
TTS echo can falsely trip the VAD and self-barge the reply. Mitigations: feed the
endpointer from `capture_drain`; require energy **sustained for N ms** before barging;
calibrate a residual-echo floor (raise the VAD threshold while `render_pending()`); confirm
via the existing stopword/trailing-silence logic in `listen.rs` before acting.

### 5.5 Coexist (shipped)

Full-duplex COEXIST is verified on-device — dictate while the voice speaks, no echo bleed.
The helper runs a `concurrent_listen_loop` thread that drains the echo-cancelled VPIO
capture and emits `PARTIAL`/`FINAL`/`LDONE` while the playback thread renders TTS; the
engine (`tts.rs`) demuxes the helper's stdout into a speak slot vs a listen slot so a
`speak` and a `listen` are in flight at once (a listen ends via `lstop`, never cancelling a
concurrent reply). The Caps gesture model: idle tap → dictate while voice plays; idle
long-press → cancel voice + dictate; dictating short-press → submit; dictating long-press →
discard. The two `mic_active()` gates are bypassed in full-duplex (§5.3, keyed off
`full_duplex_active()`).

## 6. Windows design

Capture-side only; TTS (rodio) untouched.

- Add a Windows capture backend (new module in `ds-stt`, or an `ds-aec` windows
  impl) that opens the mic with the **`windows`**/**`wasapi`** crate and calls
  `IAudioClient2::SetClientProperties` with
  `AudioClientProperties { eCategory: AudioCategory_Communications, Options:
  AUDCLNT_STREAMOPTIONS_NONE }` **before `Initialize`** (NOT `RAW`, which opts
  *out*). The OS then engages the capture-side AEC APO + Win11 Voice Clarity,
  using a render-endpoint loopback as the reference it manages itself.
- `cpal` cannot do this (no `SetClientProperties`), so this replaces the cpal
  input on Windows only. Drop the half-duplex gate when AEC is active.
- Quality depends on the endpoint's installed APO, so keep the WebRTC APM (§7)
  as the Windows fallback.

## 7. Linux design (implemented)

Server-side, app-transparent — both PulseAudio `module-echo-cancel` and PipeWire
`libpipewire-module-echo-cancel` run the **WebRTC** canceller and expose a
cancelled virtual source.

- **Shipped** (`ds-aec/src/linux.rs`): a config drop-in (PulseAudio
  `module-echo-cancel aec_method=webrtc` / PipeWire `libspa-aec-webrtc`) + docs;
  the backend opens the named cancelled source via the PulseAudio simple API (which
  also covers PipeWire through `pipewire-pulse`). Capture-side only — rodio keeps
  rendering TTS normally (`owns_render() == false`). Zero DSP code.
- **Still future:** for determinism regardless of the user's server config, optionally link the
  in-process **`webrtc-audio-processing`** (tonarino, `bundled` → needs
  clang/meson/ninja) and feed `process_render_frame` (TTS tap) +
  `process_capture_frame` (mic), maintaining a `set_stream_delay` estimate. This
  doubles as the universal fallback on any platform where the native path is
  missing or low-quality.

## 8. Rollout / safety

- A config flag (`full_duplex`, default **off**) gates all of this; with it off,
  behaviour is exactly today's half-duplex gate and the `mic_active()` mechanisms
  are untouched.
- `DuplexAudio::open()` failure (no VPIO, split devices, unsupported OS) →
  **fall back to the existing cpal + rodio/afplay half-duplex path**. AEC is
  never a hard dependency.
- Per-platform: macOS native VPIO; Windows native communications; Linux native
  server module or in-process APM; WebRTC APM as the everywhere fallback.

## 9. Testing

- `ds-aec` pure pieces (ring wiring, streaming resamplers) unit-tested; the
  device unit (VPIO) is **not** unit-tested (real side effect) — exercised by
  `ds-aec-probe` and on-device.
- On-device verify (macOS): play a known tone while capturing; confirm captured
  RMS during playback is near the no-playback floor (echo suppressed). Then the
  real test: TTS a long reply, speak "stop" over it, confirm Parakeet
  transcribes "stop" (not the TTS) and barge fires without self-barging on
  residual echo.

## 10. Capture gain — half-duplex AGC (`capture_gain`)

**Problem.** Full-duplex VPIO runs its own AGC, so the mic level is normalized for
free. The half-duplex path (raw cpal → Parakeet) has no such thing, and **mic
levels vary 10×+ across machines** (built-in vs external, OS input gain, distance).
A too-quiet buffer makes Parakeet **loop a token** (e.g. "fox" → "foam foam foam") —
the classic low-SNR failure. So a single fixed make-up gain can't be right for every
machine: what's perfect on one clips or undershoots on another.

**Decision — `capture_gain` is `"auto"` or a number; default `"auto"`.**
- `"auto"` (default): per-utterance **peak-normalize to a target** (~0.9 full-scale)
  with a **noise-floor gate** (peak < 0.02 ⇒ leave it alone, never amplify silence),
  clamped to 0.5–15×. Machine- AND mode-independent, zero per-machine setup — it gives
  the half-duplex path the level-consistency VPIO's AGC provides in full-duplex. In
  full-duplex it's ~a no-op (VPIO already normalized).
- a number (0.5–20): fixed manual multiplier, for when you want to pin it.
- Applied to the **whole buffer at transcribe time** (`auto` must measure the full
  buffer's peak), so the helper accumulates RAW and gains in `apply_gain`. See
  `ds-tts/.../ds_helper/listen.rs::auto_gain`, the `CaptureGain` enum in
  `ds-config`, and the `set_config` schema/parse in `ds-tools` / the `dontspeak`
  binary's `mcp` module.

**Why one-shot normalization and NOT a library.** Audited the Rust options
(2026-06): `sonora` / `sonora-agc2` (a **pure-Rust** WebRTC port — AEC3 + NS + AGC2,
BSD-3, 16 kHz mono, but early-stage v0.1.0) and `webrtc-audio-processing` (mature
C++-backed wrapper — the same lib §4/§8 names as the everywhere AEC fallback). Both
are **streaming adaptive** AGCs built for live duplex audio; dictation is
**push-to-talk**, so we hold the entire buffer at submit time and a one-shot
peak-to-target is both simpler and a better fit — no dependency, no 0.1.0 risk.

**When the library DOES pay off.** Cross-platform full-duplex (Windows/Linux live
AEC, §6–§7) is where an APM earns its place — there you need real-time AEC + NS + AGC
on a live stream. At that point **`sonora` is the one to track**: pure Rust means no
C++ build pain across three platforms, and it would fold the AEC fallback *and* AGC
into one BSD dependency, retiring this hand-rolled `auto_gain`. Not worth betting on a
0.1.0 today. Refs: github.com/dignifiedquire/sonora, crates.io/crates/sonora-agc2,
crates.io/crates/webrtc-audio-processing.
