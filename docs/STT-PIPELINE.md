# Speech-to-text pipeline

Built-in dictation: Parakeet TDT 0.6b v3 everywhere — ONNX over `ort` (Windows/Linux),
MLX Audio (Apple Silicon) — plus macOS System Speech. Same provider for Caps-Lock and
always-listening. The model detects its own language among 25 European ones; nothing in the
config selects it.

The ONNX model is full-context (no encoder cache), so the portable path decodes a whole
speech segment at each pause the VAD endpointer finds, and force-splits a pause-free
monologue at `boundary::MAX_SEGMENT_SECS`. That bound is what keeps decode cost flat: a
re-decoded open tail grows with dictation length. Features are log-mel fbank at the
encoder's declared `feat_dim`, normalized per bin when the export declares `per_feature` —
without that normalization every segment decodes blank.

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
  failure.
- **Windows** — reconnect WASAPI; resample device rate changes to published rate;
  recoverable error while reconnecting.
- **macOS System Speech** — phrase reset from shorter hypothesis or 0.65 s gap before
  replacement. Unchanged partials don't refresh the last-change clock (duplicates at
  boundary). Threshold stays above ordinary low-prefix revisions (≤ ~0.306 s observed).

## Verification

Covered: resampling/gain, cancel-before-listen, exclusive listen, finalization recovery,
provider change while always-listening, reconnect rate continuity, newest-only UI.
