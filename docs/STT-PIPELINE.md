# Speech-to-text pipeline

This document describes the built-in dictation path: streaming FastConformer on
Windows/Linux, Core ML Parakeet on Apple Silicon, and macOS System Speech. The selected
provider is used for both Caps-Lock dictation and always-listening mode.

## Flow and ownership

The engine owns capture lifecycle and delivery. `ds-helper` owns the warm local
recognizer, and delivers partial and final transcripts over its stdout protocol. Capture,
inference, and always-listening work run off the app UI and engine poll threads.

Only one logical helper listen session may run at a time. Sessions use generations and an
early-cancel flag, so a stop that arrives before a worker begins cannot be lost. After a
stop, finalization is bounded: 10 seconds for Parakeet and 35 seconds for System Speech.
A wedged helper is terminated and restarted rather than leaving dictation unavailable.

The helper can start for STT without loading TTS or opening an output device. It keeps only
the selected streaming backend resident, avoiding a second steady-state recognizer.

## Capture and transcript delivery

Streaming capture is incrementally resampled to 16 kHz and applies configured gain. The
half-duplex CPAL callback writes to a bounded lock-free ring, so it neither allocates nor
waits for the consumer. The speaker-lock fallback is applied to finals when that hidden mode
is enabled.

Partials are newest-value updates, not an unbounded queue: a slow platform UI receives the
latest state rather than replaying stale transcript work. Final transcripts keep their
session association until the engine completes the dictation action.

## Platform recovery

- Linux reconnects the PulseAudio/PipeWire echo-cancelled source after a read failure.
  Reconnect retries every 500 ms; it currently has no UI-facing terminal-failure state.
- Windows reconnects WASAPI capture and resamples a changed device rate back to the stable
  published rate. Its capture state exposes a recoverable error while reconnecting.
- macOS detects a System Speech phrase reset from either a shorter hypothesis or a 0.65-second
  gap before an unrelated replacement phrase. Hardware telemetry showed that System Speech can
  repeat an unchanged partial at the boundary, so duplicates do not refresh the last-change
  clock. Measuring from the last actual text change preserves the threshold's separation from
  ordinary low-prefix revisions, which were observed at gaps up to 0.306 seconds.

## Verification boundaries

Regression coverage protects incremental resampling and gain, cancellation before listen
startup, exclusive listen ownership, bounded finalization recovery, provider changes while
always-listening, capture reconnect-rate continuity, and newest-only UI delivery. Hardware
latency measurements remain release-platform work; unit tests do not claim timing percentiles.
