# Always-listening mode

Opt-in alternative to Caps-Lock PTT. `listen_mode = "always"` in `config.toml`
(default `record_submit`); exclusive; hot-reloads.

## Behavior

**Start word** (default `computer`) opens the dictation pill with live transcript.
**Submit** / **cancel** (defaults `submit` / `cancel`) paste+Enter or discard. Mic
closes during Kokoro speech and reopens when the queue is quiet. Uses Kokoro + Parakeet.

Design:

1. **Half-duplex** — mic closed while TTS busy.
2. **Stop word + trailing silence** — submit/cancel only when the word is the **final
   token** *and* followed by `submit_confirm_ms` silence. Fuzzy match (small Levenshtein)
   for live STT noise. Pause-bracketed = command; continuous speech keeps the word as
   content.

## Config

`listen_mode`, `hands_free.{start,submit,cancel}`, `submit_confirm_ms`,
`endpoint_silence_ms`. Defaults preserve record-submit mode.

## Implementation

- **Endpointer** — RMS VAD → `SpeechOnset` / `SegmentClosed` (`crate::listen`).
- **`TurnLogic`** — idle until start word; segments append until submit/cancel arms
  confirm timer; fires paste-once or treats word as content if speech resumes first.
- **Engine poll glue** (`crate::listener`) — gate mic on TTS; drain → endpointer →
  Parakeet helper → `TurnLogic` → same confirm pill / injector as Caps path. Armed
  submit always pastes into focused window (no silent refuse).
