# AEC — Full-Duplex TTS/STT (Acoustic Echo Cancellation)

Status: **macOS + Windows implemented** (`ds-aec/src/macos.rs` — a `VoiceProcessingIO`
AudioUnit; `ds-aec/src/windows.rs` — WASAPI Communications-category capture, see
`FULL-DUPLEX-PORT.md`); Linux falls back to the `src/stub.rs` no-op (half-duplex).
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

## 2. Current state (what we change)

Strict half-duplex gating, already in place:

- `rust/crates/dontspeakd/src/listener.rs` — `Listener::tick(tts_busy)` calls
  `gate_off()`, which **drops the cpal `Capture`** (`self.capture.take()`,
  ~`:130`) when TTS is busy. `tts_busy` is computed in
  `rust/crates/dontspeakd/src/lib.rs` (~`:662`) from `ttsq.is_busy()`
  (`ttsq.rs:227`).
- `rust/crates/dontspeakd/src/ttsq.rs` — the inverse gate: the worker (~`:268`)
  checks `ds_platform::mic_active()` and holds/drops narration (up to
  `MIC_WAIT_MAX`) while the mic is live.
- `rust/crates/dontspeakd/src/lib.rs` (~`:234`) — `spawn_mic_barge_watcher`
  bargges on the **idle→active edge** of `mic_active()`.

Audio paths (all separate today):

- **Capture (STT):** `rust/crates/ds-stt/src/parakeet.rs` — `Capture` opens
  **cpal** `default_input_device()` at the device-native rate/format, downmixes
  to mono f32 by channel-averaging, accumulates; `resample_to_16k()` (rubato,
  builds a **fresh** resampler per call, `:292/:299`) on stop → Parakeet.
  `Capture` is `!Send` (cpal stream) so it's created/consumed on one thread.
- **Playback (TTS):** `rust/crates/ds-tts/src/play.rs` — **macOS one-shot:
  `afplay` subprocess** ; **non-macOS: `rodio`**. Kokoro synth emits **24 kHz
  mono f32** (`rust/crates/ds-tts/src/vocab.rs` `SAMPLE_RATE = 24_000`).
- **The warm path** the engine actually uses:
  `rust/crates/ds-tts/src/bin/ds_helper/serve.rs` `serve()` — **one process, one
  thread**: a persistent **rodio** sink for TTS (even on macOS — afplay is only
  the one-shot fallback), and STT via `run_listen()` opening a cpal `Capture`.
  TTS and STT are **mutually exclusive** there: one `Job` (Speak | Listen | …) at
  a time, and the loop **blocks** in `player.sleep_until_end()` (~`:583`) during
  TTS.

The core problem for AEC: **capture and playback go through different libraries
on different clocks** (and the macOS one-shot path is even a different process).
A userspace canceller would have to time-align them itself. We avoid that by
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

## 5. macOS design (implemented first)

### 5.1 New crate `ds-aec`

A small crate owning the platform duplex-audio primitive. Public surface
(platform-agnostic; macOS impl first, a stub elsewhere returning "unsupported"):

```rust
// rust/crates/ds-aec/src/lib.rs
pub struct DuplexAudio { /* platform impl */ }

impl DuplexAudio {
    /// Open the echo-cancelled duplex unit (mic capture + speaker render).
    pub fn open() -> Result<Self, String>;
    /// The unit's negotiated capture sample rate (VPIO picks it; ~48 kHz).
    pub fn capture_rate(&self) -> u32;
    /// Push TTS PCM to be rendered — the *far-end reference* the AEC subtracts
    /// from capture. Caller supplies 24 kHz mono f32; the impl resamples to the
    /// unit rate with a LONG-LIVED streaming resampler. Non-blocking (writes to
    /// the lock-free render ring).
    pub fn render_push(&self, pcm_24k: &[f32]);
    /// Drain echo-cancelled mono f32 captured since last call (at `capture_rate()`),
    /// like `Capture::drain_new`. Feed through a long-lived 16 kHz resampler.
    pub fn capture_drain(&self) -> Vec<f32>;
    /// Whether the render ring still has unplayed samples (is TTS still sounding).
    pub fn render_pending(&self) -> bool;
    /// Drop queued render audio immediately (barge-in / stop).
    pub fn render_clear(&self);
}
```

The macOS impl wraps a single **`coreaudio::audio_unit::AudioUnit`** of
`IOType::VoiceProcessingIO` (`coreaudio-rs 0.14.2`, pinned in `ds-aec/Cargo.toml`):

- Construct `AudioUnit::new_uninitialized(IOType::VoiceProcessingIO)`.
- `set_property(kAudioOutputUnitProperty_EnableIO, …)` on the **input** element
  (`Scope::Input`, `Element::Input` = bus 1) = 1 and the **output** element
  (`Scope::Output`, `Element::Output` = bus 0) = 1.
- `set_property(kAudioUnitProperty_StreamFormat, …)` mono f32 non-interleaved on
  capture (`Scope::Output, Element::Input`) and render
  (`Scope::Input, Element::Output`); then `initialize()`. **Read back
  `sample_rate()`** — VPIO is opinionated and may force its own (treat ~48 kHz as
  negotiated, not chosen).
- `set_render_callback` drains the playback ring → speaker.
- `set_input_callback` copies AEC-cleaned mic frames → capture ring. **M1 must
  confirm both callbacks coexist on one VPIO unit** (they set different
  properties in `coreaudio-rs 0.14.2`, but this is the linchpin of the design).
- `start()`.

**Realtime safety.** The render/input callbacks run on the CoreAudio **realtime
thread**, not the helper thread. They must NOT take a lock that the helper thread
can hold → use a **lock-free SPSC ring** (`ringbuf::HeapRb`), one per direction
(playback: helper produces / RT consumes; capture: RT produces / helper
consumes). No `Mutex<VecDeque>` on the audio path — that's an RT-safety/priority-
inversion violation. The resamplers (24k→unit on render, unit→16k on capture) run
on the **helper** thread, not in the callbacks.

C constants come from **`objc2-audio-toolbox 0.3`** (0.14.x dropped
`coreaudio::sys`). Cargo (added in M1):

```toml
coreaudio-rs = "0.14.2"
objc2-audio-toolbox = { version = "0.3", default-features = false,
    features = ["std", "AudioOutputUnit", "AudioUnitProperties"] }
ringbuf = "0.5"
```

### 5.2 macOS gotchas (budget for these)

- **VPIO forces mono + ~48 kHz**; accept the unit's negotiated format, resample
  our 24 kHz render up and the capture down to 16 kHz — with **persistent**
  streaming resamplers (NOT per-call `resample_to_16k`, which allocates a fresh
  rubato resampler every invocation — fine for one-shot stop today, wrong for a
  continuous drain loop).
- **Expected baseline gain drop** when voice processing is on (Apple says so;
  disabling AGC doesn't fix it). May need a small make-up gain before Parakeet —
  but note make-up gain also amplifies residual echo (see §5.3 self-barge risk).
- **Ducking** of other audio (`AUVoiceIOOtherAudioDuckingConfiguration`,
  macOS 14+) — tune if it ducks too hard.
- **Split devices** (AirPods mic → MacBook speakers) cause aggregate-device
  channel mismatches; detect + fall back to half-duplex.
- **Instantiate VPIO only when full-duplex is wanted** (Mozilla's pattern) — it
  changes channels/gain/ducking for the whole session.
- The existing macOS-26 CoreAudio teardown abort (why `play.rs` uses afplay and
  the helper `_exit`s) applies here too: keep the `AudioUnit` (which is `!Send`,
  like the cpal stream today) on the helper's playback thread and `_exit` on quit
  rather than dropping it.

### 5.3 The `mic_active()` hazard (BLOCKER — must be handled)

`ds_platform::mic_active()` (`rust/crates/ds-platform/src/lib.rs` ~`:177`, macOS =
CoreAudio `kAudioDevicePropertyDeviceIsRunningSomewhere`) reads **true whenever
any input stream is live**. With an always-on VPIO unit it is **true for the
helper's entire lifetime**, which breaks BOTH `mic_active()`-keyed gates:

1. **TTS hold-gate** in `ttsq.rs` (~`:268`): permanently-true ⇒ all narration
   dropped, every reply delayed `MIC_WAIT_MAX` then played anyway — defeating the
   whole point of full-duplex.
2. **Mic-barge watcher** in `lib.rs` (~`:234`): the idle→active edge fires once
   at VPIO open and `prev` sticks true forever ⇒ no further barge, `resume()`
   never fires.

**Therefore: in full-duplex mode, BOTH `mic_active()`-based mechanisms must be
bypassed.** The TTS worker must not hold on `mic_active()`, and barge must be
driven instead from the **AEC-cleaned `capture_drain` energy** (the `listen.rs`
`Endpointer`/`frame_rms`). Likewise `listener.rs::gate_off` must become a no-op
when AEC is active. This is gated by the `full_duplex` config (§7) so the
half-duplex code paths are untouched when AEC is off.

### 5.4 Self-barge-from-echo risk

AEC is *suppression*, not perfect cancellation; residual echo remains (worse with
§5.2 make-up gain). If the barge endpointer is fed the cleaned `capture_drain`
naively, residual TTS echo can falsely trip the VAD and self-barge the reply.
Mitigations (required for M3): feed the endpointer from `capture_drain`; require
energy **sustained for N ms** before barging; calibrate a residual-echo floor
(raise the VAD threshold while `render_pending()`); confirm via the existing
stopword/trailing-silence logic in `listen.rs` before acting.

### 5.5 Integration into the warm helper (milestoned)

`serve()` today is mutually exclusive (one `Job` at a time, blocking in
`sleep_until_end()`). Full-duplex keeps the VPIO unit **always open and
capturing** on the helper thread (its callbacks run on the RT thread,
independent of whatever job the helper loop is in). Rolled out so the primary
platform never breaks:

- **M1 — `ds-aec` core + probe (no engine change). ✅ DONE.** Built the crate
  (`DuplexAudio` over a single VPIO `AudioUnit`, lock-free `ringbuf` rings,
  streaming `LinearResampler`) + `ds-aec-probe`. On-device run confirmed: VPIO
  opens at 48 kHz; **both render and input callbacks coexist on one unit and
  fire** (the linchpin risk — resolved); captured RMS of our own 0.3-amplitude
  tone is suppressed to ~0.001–0.016 (≈0.0003 room floor when quiet) — ~20–40 dB
  of echo cancellation. Fully self-contained, no engine wiring.
- **M2 — drop-in duplex unit, behaviour unchanged. ✅ DONE.** macOS only, behind
  the `full_duplex` flag: the helper routes the rodio output sink **and** the cpal
  `Capture` through one `DuplexAudio` when `DONTSPEAK_FULL_DUPLEX` is set. The VPIO
  captures continuously from `open()`, but the helper **ignores `capture_drain`
  except during a Listen job**, so user-visible behaviour stays half-duplex while
  the AEC path runs end to end. The engine (`tts.rs` + `ds-config`) sets the env
  **only when `full_duplex` AND `stt == parakeet` AND TTS is on (Kokoro)**
  (`full_duplex_wanted`) and restarts the warm helper
  when that resolves differently (`set_full_duplex_pref` /
  `restart_if_full_duplex_stale`), scoping AEC to the Parakeet path; the `claude_code`
  path is untouched. With the flag unset the rodio+cpal path is byte-identical to before.
  Builds clean; 39 ds-config + 34 dontspeakd tests pass. Remaining: expose
  `full_duplex` in the app UI / MCP `set_config`, and on-device verify.
- **M3 — full-duplex COEXIST (dictate while the voice speaks). ✅ DONE** (verified
  on-device — dictation captured cleanly with TTS playing, no echo bleed).

  The implicit **voice-barge-from-echo** design (a `BargeDetector` watching the
  cleaned capture, a `BARGE` protocol line, `take_barged()`/`voice_barge()`) was
  **dropped** — it was redundant once true coexist landed. Instead the user dictates
  OVER the reply and explicitly cancels the voice with a Caps long-press. What
  shipped:
  - ✅ **Concurrent speak + listen over a stdout demux.** The helper runs a
    dedicated `concurrent_listen_loop` thread that drains the echo-cancelled VPIO
    capture and emits `PARTIAL`/`FINAL`/`LDONE` WHILE the playback thread renders
    TTS (`DONE`). The engine (`tts.rs`) owns a persistent stdout reader that demuxes
    every line into a speak slot (`DONE`/`STATS`/`ERR`) vs a listen slot
    (`LDONE`/`PARTIAL`/`FINAL`/`STTSTATS`/`STTERR`), so a `speak` and a `listen` are
    in flight at once. A listen ends with the `lstop` op (not `stop`), so ending
    dictation never cancels a concurrent reply.
  - ✅ **Engine doesn't barge on a dictation tap** in full-duplex (`start_recording`
    skips `q.clear()`/`barge_in`), so the reply keeps playing while you dictate.
  - ✅ **Bypass the `mic_active()` gates** in full-duplex (§5.3): the queue worker
    skips its `mic_active()` reply-hold and `spawn_mic_barge_watcher` stands down
    (both keyed off `TtsManager::full_duplex_active()`), since the VPIO mic is always
    live and would otherwise gate/barge spuriously.
  - ✅ **Caps gesture model (full-duplex):** idle tap → start dictation (voice keeps
    playing); idle long-press → cancel the voice + dictate; dictating short-press →
    submit (single press, no confirm tap); dictating long-press → discard (voice
    keeps playing). The submit is keyed off the steady `!down` state, not the release
    edge, so a fast tap can't lose it. Pure `long_press_action()` is unit-tested.
  - ✅ **Menu-bar pill:** recording (orange) overrides speaking (purple) while you
    dictate, driven by the engine DICTATION state (not the always-on VPIO mic).
  - ✅ **Dictation panel** appears the moment recording starts (gated on the
    `dictation.local_stt` flag), showing only the transcript text.
  - ✅ **Verified on-device:** with TTS rendering through VPIO, a Caps-tap dictation
    produced clean partials of the user's voice (RMS ~0.08) while the reply played —
    AEC kept the playback out of the mic (user-confirmed, no bleed).

  Fixed during first live attempt — **render-ring overflow ("choking")**: the VPIO
  render ring was 2 s, but the helper synthesizes a whole reply up front and Kokoro
  outpaces real time, so everything past 2 s was dropped → choppy/truncated
  playback. Enlarged the render ring to 90 s (`RENDER_CAP`; capture ring stays 2 s).
  Also fixed — **render chopping under load**: ORT's spinning thread-pool starved
  the VPIO RT render thread; running synth on the CoreML execution provider in
  full-duplex keeps cores free and the render smooth.
  Also note: the app narrates Claude's replies via the streaming MessageDisplay
  pipeline (same Kokoro voice), which collides with a test helper's playback —
  silence it (`narrate=false`, or `tts_engine=off`, or `stop_speak`) while testing
  the helper directly.

### 5.6 What changes, by file

- **new** `rust/crates/ds-aec/{Cargo.toml,src/lib.rs,src/macos.rs,src/bin/probe.rs}`
  — the duplex primitive + probe (M1).
- `rust/Cargo.toml` — add `coreaudio-rs`, `objc2-audio-toolbox`, `ringbuf` to
  `[workspace.dependencies]`; add `crates/ds-aec` to `members`.
- `rust/crates/ds-tts/Cargo.toml` — depend on `ds-aec` (M2).
- `rust/crates/ds-tts/src/bin/ds_helper/` — macOS: route render + capture
  through `DuplexAudio` when `DONTSPEAK_FULL_DUPLEX` is set (M2); the concurrent
  `concurrent_listen_loop` thread drains the echo-cancelled mic while TTS renders,
  terminating with `LDONE` so the engine can demux it from speak output (M3).
- `rust/crates/dontspeakd/src/{lib.rs,tts.rs}` — `full_duplex_wanted(cfg)` =
  `full_duplex && stt==Parakeet && tts_on(Kokoro)` drives `DONTSPEAK_FULL_DUPLEX`
  on the warm helper in `start()`; `set_full_duplex_pref` +
  `restart_if_full_duplex_stale` restart a running helper when that resolved mode
  changes (mirror `set_provider`'s stop+start). This is what scopes AEC to the
  Parakeet+Kokoro path (M2).
- `rust/crates/dontspeakd/src/listener.rs` — `gate_off` no-op when AEC active;
  `rust/crates/dontspeakd/src/lib.rs` + `ttsq.rs` — bypass the two `mic_active()`
  gates in full-duplex (§5.3) (M3).
- `rust/crates/ds-config/src/lib.rs` — add the `full_duplex` setting. Four
  coordinated edits: (a) `#[serde(default)] pub full_duplex: bool` on
  `VoiceConfig` (~`:563`); (b) the field in `impl Default for VoiceConfig`
  (~`:617`); (c) a `vo.insert("full_duplex".into(), json!(...))` line in
  `merge_settings` (~`:740`) so the GUI write path persists it; (d) a
  `ConfigChange` entry in `changes_since` (~`:570`) so toggling hot-reloads the
  warm helper (SIGHUP, ~`:352`) instead of needing a restart. Default **off**.

## 6. Windows design (after macOS)

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

## 7. Linux design (after Windows)

Server-side, app-transparent — both PulseAudio `module-echo-cancel` and PipeWire
`libpipewire-module-echo-cancel` run the **WebRTC** canceller and expose a
cancelled virtual source.

- Ship a config drop-in (PulseAudio `module-echo-cancel aec_method=webrtc` /
  PipeWire `libspa-aec-webrtc`) + docs; the app records the named cancelled
  source. Zero DSP code.
- For determinism regardless of the user's server config, optionally link the
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
