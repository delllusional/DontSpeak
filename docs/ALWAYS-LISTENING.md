# Always-listening mode (hands-free voice loop)

A second voice mode alongside the default **record-and-submit** (Caps-Lock
tap-to-talk) flow, mirroring the Claude app's split of *dictation/PTT* vs a
continuous *voice mode*.

It is opt-in via config and hot-reloaded — set `listen_mode = "always"` in
DontSpeak's `config.toml` (default `record_submit`). Record-and-submit stays
exactly as it is; always-listening is purely additive. The two modes are exclusive
(Caps-Lock PTT is bypassed while in `always` mode).

## How it works

Speak, and the engine transcribes into the Claude Code prompt as you go; say a
**stopword** (default `submit`) and it sends. Kokoro speaks the reply; while it
speaks the mic is closed; when it goes quiet the mic reopens. Just the two models —
Kokoro (TTS) + Parakeet (STT) — and a thin pipe.

Two design decisions shape it:

1. **Half-duplex, no echo cancellation.** While the TTS queue is busy the mic is
   closed ("listen only when not playing"). We lose mid-sentence barge-in; that is
   an accepted trade. (Muting the mic during TTS is the standard local/edge
   simplification.) AEC / full-duplex has since shipped separately (`ds-aec`, gated
   by the `full_duplex` config — see `AEC.md`), but always-listening itself still
   runs this half-duplex gate; folding it onto the AEC path is a later upgrade.

2. **Stopword + trailing-silence confirmation.** Submission fires only when the
   configured stopword is the **final token** of an utterance **and** is followed by
   a confirmation window of continued silence. So "I want to submit the message to a
   client" never fires (the word is mid-sentence), whereas saying "submit" and then
   going quiet sends. With no stopword spoken it keeps listening forever. (This is
   Dragon's "pause-bracketing" — words run together = dictation, bracketed by pauses
   = command. A distinctive multi-syllable stopword is safest against collisions.)

## Config (the `dontspeak` block)

| Setting | Default | Meaning |
| --- | --- | --- |
| `listen_mode` | `record_submit` | `record_submit` or `always` |
| `hands_free.submit` | `submit` | the spoken stopword that submits (the `[hands_free]` table also holds `start`/`cancel`) |
| `submit_confirm_ms` | `1000` | continued silence after the stopword before sending; if speech resumes inside it, the word was content, not a command |
| `endpoint_silence_ms` | `700` | trailing silence that closes an utterance (500–700 ms is the mainstream end-of-turn range) |

All are `#[serde(default)]` and fail-open, so an unset block behaves as today. A
`listen_mode` change is picked up by the engine's hot-reload (it starts/stops the
listener live).

(The `[hands_free]` `start` wake word — default `computer` — is shelved pending better
on-device STT: the plumbing works, but the start word is only as reliable as the mangled
transcription of it. The `submit`/`cancel` stopwords are matched exactly and stay reliable.)

## Implementation

Three layers; the bottom two are pure and unit-tested (`crate::listen`), the top is
thin glue on the engine's poll thread (`crate::listener`):

- **Endpointer** (audio → segment events) [pure] — feed it per-frame energy + dt;
  it emits `SpeechOnset` and `SegmentClosed`. VAD is energy (RMS) based.
- **Turn logic** (text + timing → actions) [pure] — consumes `SegmentClosed`,
  `SpeechOnset`, and ticks; emits `Paste(text)` (type a chunk live) and `Submit`
  (press Enter). A non-stopword segment pastes immediately; a stopword-terminated
  segment pastes the pre-stopword content, holds the stopword, and arms the
  `submit_confirm_ms` timer (elapses → Submit; new speech first → the word was
  content, paste it and carry on).
- **Engine integration** [`listener.rs`] — on the 30 ms poll tick: if the TTS queue
  is busy, keep the mic closed (play-gate), else drain new mic samples, step the
  Endpointer, and on `SegmentClosed` resample + transcribe (the Parakeet model,
  via the warm helper), feed the text to the Turn logic, and execute the action via
  the platform (`type_text` / Enter), focus-gated like the record-and-submit paste.

## Possible later upgrades

Acoustic echo cancellation / true barge-in — the AEC/full-duplex layer now exists
(`ds-aec`, gated by `full_duplex`; see `AEC.md`), so this becomes wiring always-listening
onto it rather than net-new work; Silero VAD (~2 MB ONNX on the shared
`ort` runtime) for robustness; a pre-roll leading buffer;
and a GUI control (config + hot-reload drives it for now).
