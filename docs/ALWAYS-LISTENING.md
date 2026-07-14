# Always-listening mode (hands-free voice loop)

An opt-in alternative to Caps-Lock record-and-submit. Set
`listen_mode = "always"` in `config.toml` (default `record_submit`); the modes are
exclusive and hot-reload.

## How it works

Say the **start word** (default `computer`) to open the dictation pill. It shows the
live transcript. **Submit** (default `submit`) pastes it and presses Enter; **cancel**
(default `cancel`) discards it. The mic closes during Kokoro speech and reopens when
the queue is quiet. The mode uses Kokoro (TTS) and Parakeet (STT).

Two design decisions shape it:

1. **Half-duplex gating.** The mic stays closed while the TTS queue is busy. A future
   AEC/full-duplex layer can replace this gate.
2. **Stop word + trailing-silence confirmation.** Submit/cancel fire only when the
   configured word is the **final token** of an utterance *and* is followed by a
   confirmation window of silence — so "I want to submit the message to a client"
   never fires, while saying "submit" and going quiet pastes-and-sends. Matching is
   fuzzy (small Levenshtein tolerance) since it runs on live STT output. This is
   Dragon's "pause-bracketing": words run together = dictation, bracketed by pauses
   = command.

## Config

The `dontspeak` block adds `listen_mode`, `hands_free.{start,submit,cancel}`, plus
`submit_confirm_ms` (silence before submit/cancel) and `endpoint_silence_ms` (silence
that closes an utterance). Defaults preserve the normal mode.

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
