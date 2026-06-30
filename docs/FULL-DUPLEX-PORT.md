# Full-duplex coexist — residual plan

> **Status:** Cross-platform full-duplex coexist (dictate / interrupt by Caps tap
> *while* the TTS voice is still speaking, with the recogniser hearing only you)
> **shipped on macOS, Windows, and Linux**, each using its native AEC: VPIO on
> macOS, WASAPI Communications-category capture on Windows, and
> PipeWire/PulseAudio `module-echo-cancel` on Linux. The engine demux, gesture
> state machine, helper protocol, config/MCP surface, and the `owns_render()`
> `DuplexAudio` generalization are all in place; WinUI and the GTK host have UI
> parity (tray pill + dictation overlay). See `docs/AEC.md` for the architecture.

The only forward-looking work left is a **selectable WebRTC-APM AEC backend**,
primarily on macOS (with an optional in-process Linux variant). Everything below
is about that one residual.

---

## Selectable WebRTC-APM backend on macOS

Keep **VPIO** the macOS default — best quality, hardware-tuned, and it owns render
+ capture on ONE clock so far-end/near-end are aligned for free (no delay work).
But expose **WebRTC APM** as an alternative AEC backend, chosen like the
execution-provider setting (`set_provider` style; the two cancellers are mutually
exclusive — never run both at once). It plugs into the same `ds-aec`
`DuplexAudio` contract with `owns_render() = true` (it controls its own
render-reference tap), so the helper and engine stay untouched.

Why it's wanted:

- **De-risks the ports** — running the WebRTC backend on a Mac exercises the exact
  capture-side/frame-processor shape the native Win/Linux backends use (rodio
  keeps rendering; the AEC is a frame processor fed `process_render_frame` from a
  TTS tap + `process_capture_frame` from the mic, with a `set_stream_delay`
  estimate). Shaking out delay/drift alignment here hardens the shared path.
- **Tunable** where VPIO is a black box: explicit AEC aggressiveness, noise
  suppression, clean AGC-off, high-pass — an escape hatch if VPIO's voice-comm
  coloring ever caps Parakeet accuracy (we already had to disable VPIO's AGC).
- **Render decoupling**: TTS stays on the smooth rodio path and is merely *tapped*
  as the reference, instead of routed through VPIO's realtime render thread —
  sidesteps the RT-render-starvation chop class of bug (the one CoreML papered
  over) and stops seizing the output device.
- **A real full-duplex fallback** when `VPIO::open()` fails (split devices, odd
  audio config) instead of dropping to half-duplex.

The cost VPIO avoids and WebRTC must pay: **delay + clock-drift alignment.** On
macOS the reference is the PCM you synthesized (you have it), but time-aligning it
to what the mic hears — and compensating drift if render/capture sit on different
clocks — is the genuinely hard part. That, plus higher CPU and the
clang/meson/ninja build deps, is why VPIO stays the default and WebRTC is an
*option*.

## Optional in-process Linux WebRTC-APM variant

Linux ships the cancelled-source path (PipeWire/PulseAudio `module-echo-cancel`,
`owns_render() = false`). As a deterministic alternative — independent of the
user's server config — link **`webrtc-audio-processing`** (tonarino crate)
in-process with `owns_render() = true`, a TTS render tap, and a `set_stream_delay`
estimate, behind a Cargo feature (`--features webrtc-aec`; `bundled` needs
clang/meson/ninja). Same `DuplexAudio` contract, fail-quiet → half-duplex.
