# AEC — Full-Duplex TTS/STT (Acoustic Echo Cancellation)

DontSpeak's Caps-Lock dictation is half-duplex by default: the mic closes whenever
TTS is playing, so the voice can't be interrupted mid-reply. Full-duplex COEXIST —
dictate *while* the voice keeps speaking, with the recognizer hearing only the user
— is shipped on macOS, Windows, and Linux, each using that platform's native
acoustic echo cancellation (AEC) to strip the played-back signal out of the mic
before it reaches the recognizer. Paths below are relative to the repo root; the
Rust workspace is under `rust/`.

## Scope and gating

Full-duplex applies only to the built-in Parakeet STT path (`HelperStt`) — the one
place DontSpeak itself opens the mic and plays TTS. The `claude_code` dictation path
is untouched: Claude Code records the mic itself, so there is nothing for AEC to
cancel there. The engine enables AEC (passing `DONTSPEAK_FULL_DUPLEX` to the warm
helper) only when `full_duplex` is on, STT is Parakeet, and TTS is on with Kokoro —
with TTS off there's no echo to cancel, so opening the platform AEC unit would only
cost mic gain for nothing. The engine restarts the warm helper whenever that
resolved mode changes.

## Why native OS AEC

An echo canceller adaptively models the speaker→air→mic path, comparing the
far-end reference (what's played) against the near-end capture (the mic)
time-aligned on a common clock; drift or a wrong delay estimate keeps the filter
from converging. Native OS AEC owns both the render and capture streams in one
clock domain, so that alignment is free — which is why each platform prefers its
native path over a userspace canceller that would have to feed both streams,
estimate delay, resample, and track drift itself. An in-process WebRTC audio-processing
fallback is possible, but DontSpeak does not currently bundle or implement one.

## Per-platform approach

| Platform | Mechanism | What changes |
|---|---|---|
| macOS | Voice-Processing I/O AudioUnit (`kAudioUnitSubType_VoiceProcessingIO`) — one unit renders TTS *and* captures the mic, with AEC built in | Both playback and capture move onto the VPIO unit |
| Windows | Mic opened in WASAPI's **Communications** category (`IAudioClient2::SetClientProperties`); the OS supplies the render loopback reference plus Win11 Voice Clarity | Capture only — TTS stays on rodio |
| Linux | PulseAudio `module-echo-cancel` / PipeWire `libspa-aec-webrtc` (WebRTC under the hood) expose an echo-cancelled virtual source | Capture only — TTS stays on rodio |

macOS is the crux: VPIO must own both directions in one AudioUnit, so it replaces
both the playback path and the cpal capture, where Windows and Linux only swap the
capture side.

## macOS: Voice-Processing I/O

The `ds-aec` crate wraps a single `VoiceProcessingIO` AudioUnit behind a
`DuplexAudio` type (`open`, `render_push`, `capture_drain`, ...). The render and
capture callbacks run on CoreAudio's realtime thread, so each direction is a
lock-free SPSC ring; resampling between the unit's negotiated rate and 16 kHz
happens on the helper thread, never in the callback.

An always-on VPIO unit makes `ds_platform::is_mic_active()` read true for the
helper's entire lifetime, which would defeat the TTS hold-gate and the mic-barge
watcher if left as-is — so in full-duplex mode both are bypassed, and barge
detection instead runs off the AEC-cleaned capture stream's energy, with a
sustained-energy threshold and a calibrated residual-echo floor so leaked echo
can't trip a false self-barge.

With COEXIST shipped, the helper runs a concurrent listen loop alongside the
playback thread: a Caps tap dictates while the voice keeps speaking (no barge-in —
VPIO's AEC keeps playback out of the mic), and only a long-press stops the voice.

## Windows: Communications-category capture

Capture opens via `IAudioClient2::SetClientProperties` with
`AudioCategory_Communications` before `Initialize` (not `RAW`, which opts out of
processing), which engages the OS's capture-side AEC APO and Win11 Voice Clarity
using a render-endpoint loopback it manages itself. This replaces the cpal input on
Windows only; TTS is untouched.

## Linux: server-side echo cancellation

PulseAudio's `module-echo-cancel` and PipeWire's `libpipewire-module-echo-cancel`
both run a WebRTC canceller and expose a cancelled virtual source. `ds-aec`
ships the config drop-in and opens that named source through the PulseAudio simple
API, which also covers PipeWire via `pipewire-pulse`. Capture-side only — rodio
keeps rendering TTS normally.

## Rollout and fallback

A config flag, `full_duplex` (default off), gates all of the above; with it off,
behavior is exactly today's half-duplex gate. If the platform's `DuplexAudio::open()`
fails — no VPIO, a split input/output device pair, an unsupported OS — DontSpeak
falls back to the existing cpal + rodio/afplay half-duplex path. AEC is a layer on
top of half-duplex, never a hard dependency.

## Capture gain for half-duplex (`capture_gain`)

Full-duplex VPIO runs its own AGC, normalizing mic level for free; the half-duplex
path has no such thing, and mic levels vary widely across machines, which can make
Parakeet loop a token on too-quiet audio. `capture_gain` defaults to `"auto"`:
each utterance is peak-normalized to a target level (with a noise-floor gate so
silence is never amplified), clamped to a 0.5–15× range; it can also be set to a
fixed numeric multiplier. Because dictation is push-to-talk rather than a live
stream, this normalizes the whole captured buffer once at transcribe time — a
simpler fit than a streaming adaptive AGC.

## Testing

`ds-aec`'s pure logic (ring wiring, resamplers) is unit-tested; the platform AEC
units are exercised via `ds-aec-probe` and on-device checks: confirm captured RMS
during playback matches the no-playback floor, then confirm Parakeet transcribes
speech spoken over an in-progress reply without self-barging on residual echo.
