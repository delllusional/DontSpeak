# AEC — full-duplex TTS/STT

Default Caps dictation is half-duplex (mic closed during TTS). Full-duplex lets the
user dictate while speech continues: native OS acoustic echo cancellation strips
playback from the mic before the recognizer.

## Scope

Only built-in Parakeet (`HelperStt`) with Kokoro TTS. `claude_code` STT is untouched
(Claude owns the mic). Engine sets `DONTSPEAK_FULL_DUPLEX` on the helper when
`full_duplex` is on and both engines qualify; restarts helper when that resolution
changes. TTS off → no AEC (no echo to cancel).

## Why native OS AEC

AEC needs far-end (playback) and near-end (mic) on one clock. OS AEC owns both streams;
userspace would estimate delay, resample, and track drift. WebRTC in-process fallback
is possible but not implemented.

## Per platform

| Platform | Mechanism | Scope |
|---|---|---|
| macOS | Voice-Processing I/O (`kAudioUnitSubType_VoiceProcessingIO`) | Playback **and** capture on one unit |
| Windows | WASAPI Communications category (`IAudioClient2::SetClientProperties`) + Voice Clarity | Capture only; TTS stays on rodio |
| Linux | Pulse/PipeWire `module-echo-cancel` (WebRTC) virtual source | Capture only; TTS on rodio |

## macOS

`ds-aec` `DuplexAudio` wraps one VPIO unit; realtime callbacks use lock-free SPSC rings;
resample to 16 kHz off the RT thread. Always-on VPIO makes `is_mic_active()` true for
helper lifetime → full-duplex bypasses TTS hold-gate and mic-barge watcher; barge uses
AEC-cleaned energy + residual-echo floor. Caps tap dictates while voice continues; only
long-press stops speech (no barge-in).

## Windows / Linux

Windows: Communications category before `Initialize` (not RAW). Linux: open named
echo-cancelled source via Pulse simple API (`pipewire-pulse` compatible).

## Rollout

`full_duplex` default off. `DuplexAudio::open()` failure → cpal + rodio/afplay half-duplex.

## `capture_gain` (half-duplex)

VPIO has AGC; half-duplex does not. Default `"auto"`: peak-normalize each utterance
(noise-floor gate, clamp 0.5–15×) or fixed multiplier. Applied once at transcribe time
(PTT, not streaming AGC).

## Testing

Unit: rings/resamplers. On-device: captured RMS during playback ≈ silence floor; Parakeet
transcribes over in-progress reply without self-barge. `ds-aec-probe` for platform units.
