# Always-listening mode (hands-free voice loop)

A second voice mode alongside the default **record-and-submit** (Caps-Lock
tap-to-talk) flow, mirroring the Claude app's split of *dictation/PTT* vs a
continuous *voice mode*. It's opt-in and hot-reloaded via `listen_mode = "always"`
in `config.toml` (default `record_submit`); the two modes are exclusive, and
turning it on doesn't change record-and-submit at all.

## How it works

Say the **start word** (default `computer`) to open the dictation **pill** and
begin capturing; speak, and the pill shows the accumulated transcript live. Say
**submit** (default `submit`) to paste the whole capture + Enter, or **cancel**
(default `cancel`) to discard it — either way the pill closes. Kokoro speaks the
reply; the mic closes while it speaks and reopens when it goes quiet. Just the two
models — Kokoro (TTS) + Parakeet (STT) — and a thin pipe between them.

Two design decisions shape it:

1. **Half-duplex gating.** While the TTS queue is busy the mic stays closed
   ("listen only when not playing") — the standard local/edge simplification, and
   the same gate the AEC/full-duplex layer (`ds-aec`, see `AEC.md`) is designed to
   eventually sit underneath.
2. **Stop word + trailing-silence confirmation.** Submit/cancel fire only when the
   configured word is the **final token** of an utterance *and* is followed by a
   confirmation window of silence — so "I want to submit the message to a client"
   never fires, while saying "submit" and going quiet pastes-and-sends. Matching is
   fuzzy (small Levenshtein tolerance) since it runs on live STT output. This is
   Dragon's "pause-bracketing": words run together = dictation, bracketed by pauses
   = command.

## Config

The `dontspeak` block adds `listen_mode`, `hands_free.{start,submit,cancel}` for
the three trigger words, and two timing knobs: `submit_confirm_ms` (silence after
submit/cancel before it fires) and `endpoint_silence_ms` (trailing silence that
closes an utterance). All fields default sensibly and fail open, so an unset block
behaves exactly like today.

## Implementation

Three layers, bottom two pure and unit-tested (`crate::listen`), top layer thin
glue on the engine's poll thread (`crate::listener`):

- **Endpointer** — energy-based (RMS) VAD; turns per-frame audio into
  `SpeechOnset` / `SegmentClosed` events.
- **`TurnLogic`** — turns text + timing into actions. Idle until the start word
  opens the pill; further segments append to the live buffer until a
  submit/cancel-terminated segment arms the confirm timer on that word, which
  either fires the action (paste-once, never incremental) or, if new speech comes
  first, treats the word as content and keeps accumulating.
- **Engine integration** — on each poll tick: mic stays closed while TTS is
  playing, otherwise drains mic samples through the Endpointer, transcribes closed
  segments via the warm Parakeet helper, feeds `TurnLogic`, mirrors its state into
  the same confirm pill the Caps-Lock PTT path uses, and pastes/discards through
  the same injector — the stop-word confirm (like the Caps confirm tap) is itself
  the deliberate gate, so once armed a submit always pastes into whatever is
  focused, never silently refused.

## Later upgrades

Wiring always-listening onto the AEC/full-duplex layer for true barge-in; Silero
VAD for more robust endpointing; a pre-roll leading buffer; a GUI control for the
mode toggle.
