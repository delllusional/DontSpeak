# AEC — full-duplex TTS/STT

Default Caps dictation pauses TTS while the mic is open, and this pause-and-resume
policy applies in both duplex modes. Full-duplex keeps playback and capture available
at the same time for always-listening dictation: native OS acoustic echo cancellation
strips playback from the mic before the recognizer.

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
helper lifetime → full-duplex bypasses the generic mic-activity hold gate and foreign-mic
barge watcher; always-listening barge uses AEC-cleaned energy + residual-echo floor.
Caps PTT has an explicit policy independent of those gates: the start tap pauses the TTS
queue, the stop tap resumes it, and a long-press clears it without resuming.

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
always-listening transcribes over an in-progress reply without self-barge.
`ds-aec-probe` for platform units.
