# Speech-to-text pipeline

Built-in dictation: streaming FastConformer 1040 ms (Windows/Linux), Core ML Parakeet
(Apple Silicon), macOS System Speech. Same provider for Caps-Lock and always-listening.
The portable FastConformer emits lowercase text without punctuation; its encoder metadata
sets the live partial cadence.

## Ownership

Engine owns capture lifecycle and delivery. `ds-helper` owns the warm recognizer and
partial/final transcripts on stdout. Capture/inference run off UI and poll threads.

One logical helper listen session at a time (generations + early-cancel). Finalization
timeouts: 10 s Parakeet, 35 s System Speech. Wedged helper is killed and restarted.
Helper can start STT without loading TTS or opening an output device.

## Capture and delivery

Resample to 16 kHz + configured gain. Half-duplex CPAL callback → bounded lock-free
ring (no alloc/wait). Speaker-lock filter on finals when that mode is on.

Partials: newest-value only (slow UI gets latest, not backlog). Finals keep session
association until the engine finishes the action.

## Platform recovery

- **Linux** — reconnect Pulse/PipeWire echo-cancelled source every 500 ms after read
  failure; no UI terminal-failure state yet.
- **Windows** — reconnect WASAPI; resample device rate changes to published rate;
  recoverable error while reconnecting.
- **macOS System Speech** — phrase reset from shorter hypothesis or 0.65 s gap before
  replacement. Unchanged partials don't refresh the last-change clock (duplicates at
  boundary). Threshold stays above ordinary low-prefix revisions (≤ ~0.306 s observed).

## Verification

Covered: resampling/gain, cancel-before-listen, exclusive listen, finalization recovery,
provider change while always-listening, reconnect rate continuity, newest-only UI.
Hardware latency percentiles are release-platform work, not unit tests.
