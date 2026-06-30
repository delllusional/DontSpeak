# Architecture

DontSpeak gives Claude Code a hands-free voice loop: tap Caps Lock to dictate,
hear replies spoken back. The design is **one native app per OS hosting the same
Rust engine in-process** over the `ds-core` C ABI — macOS (SwiftUI), Windows
(WinUI), and Linux (GTK4), all built and tested in CI — with the Claude Code hooks
and an MCP server as thin clients. macOS is the most polished host.

## One app hosts the engine

The engine — Caps-Lock dictation, the TTS queue, local STT, "test recognition",
model presence, and the RPC server — is a Rust core exposed over a small C ABI
(`ds_engine_start/stop/reload`). The macOS app (`DontSpeak.app`) links it and
runs it on a background thread, so the OS-permission-bearing work runs INSIDE the
one signed app bundle — **all TCC grants (Accessibility / Mic) land on that single
app, granted once** (Accessibility subsumes Input Monitoring for the Caps-key read,
so no separate Input Monitoring grant is needed). There is no separate daemon and no
launchd agent: the app is the login item, `engine_start()` on launch,
`engine_stop()` on quit. The design is portable and **all three platforms ship
implemented apps + platform backends, built and tested in CI**: a headless host
binary (`dontspeakd`) runs the same `engine_run` for Linux/CLI, and the Windows
(`apps/windows/winui/`) and Linux (`apps/linux/gtk/`) apps link the cdylib and call
the same FFI. The `ds-platform` backends are complete real-API impls on each OS
(`linux.rs` evdev/uinput/x11rb; `windows.rs` SendInput/GetKeyState/GetForegroundWindow;
macOS IOKit/CGEvent), and the CI release matrix builds + tests on `ubuntu-latest`,
`windows-2025`, and `macos-26`. macOS remains the most polished host; the Windows/Linux
apps are newer (see the platform notes below). Every other piece (the hooks, the MCP
server) is a thin client that holds no model and talks to the engine over a
Unix-domain socket (`ds-ipc`, NDJSON).

Our config lives in **`config.toml`** under DontSpeak's data dir (macOS: `~/Library/Application
Support/DontSpeak/`, alongside the downloaded `models/`) — a neutral home, separate
from any client. (We never store config in `~/.claude/settings.json`; the engine in fact
DROPS any stale `dontspeak` block a prior version seeded there, leaving that file purely
Claude Code's — its hooks and its own `voice` block, which we set separately.) After a
Caps press the STT **path** is resolved from `stt_engine` — an **ordered preference ladder**
(`ds-config/src/voice.rs`, `pub stt_engine: Vec<SttEngine>`), walked first-usable-rung by
`resolved_stt` (`built_in` → our local Parakeet STT, **the default**; `system`; `claude_code`
→ delegate to Claude Code's own voice dictation by tapping its `voice:pushToTalk` key). A legacy
scalar string still parses. `tts_engine` is the same kind of ladder (`Vec<TtsEngine>`, walked by
`resolved_tts`). These selectors gate subsystems (flip via the MCP `set_config`); the engine
applies changes **surgically** (no full reloads):

| setting | subsystem | rebuild trigger |
| --- | --- | --- |
| `caps_enabled` | Caps Lock key handler (dictation + voice silence/cancel) | toggle only |
| `stt_engine` | STT path (off / claude_code / built_in Parakeet / system) | `stt_engine` change |
| `tts_engine` | TTS (off / Kokoro via warm helper / system `say`) | `tts_engine` change |

Per-call params (voice / rate / narrate) change nothing warm — the next
synth/transcribe reads them fresh. The engine hot-reloads our `config.toml` via
an mtime-watch (the headless binary also honors SIGHUP). Quit the app to stop the
engine; it restarts on next launch.

## Workspace layout

The Rust engine lives in `rust/` as a set of small single-purpose crates; the
per-OS apps live in `apps/macos/` (SwiftUI), `apps/windows/winui/`, and
`apps/linux/gtk/`. See [rust/README.md](rust/README.md) for the full crate-by-crate
roles; in brief the crates are:

- **`ds-config`** — paths + `config.toml` + `~/.claude/settings.json` merge.
- **`ds-ipc`** — engine↔client RPC over the Unix socket.
- **`ds-proc`** — pidfile single-speaker + process-group barge-in.
- **`ds-platform`** — per-OS key/window backends.
- **`ds-model`** — asset URLs/digests + downloader.
- **`ds-tts`** — `Tts` trait + the `ds-helper` warm-child bin.
- **`ds-stt`** — `Stt` trait (Parakeet / ClaudeNative / System).
- **`ds-aec`** — echo-cancelled full-duplex audio.
- **`ds-engines`** — config→engine factories.
- **`ds-tools`** — the single MCP tool catalog.
- **`ds-i18n`** — shared UI-string catalog over the FFI.
- **`ds-status`** — the `model_status` engine→UI contract.
- **`dontspeakd`** — the engine (`engine_run`) + headless Linux/CLI host.
- **`ds-core`** — the stable C-ABI lib the apps link.
- **`dontspeak`** — the one multi-call client (MCP server / hook entries / installers).

Each native app is a menu-bar/health/permissions UI that **hosts the engine**
(`ds_engine_start`) and is the login item. Control
(voice/engine/language/rate/toggles/downloads) is via the MCP, not the app.

## Pluggable engines

`ds-engines::make_tts` / `make_stt` map the config enum to a boxed trait object and
**degrade to the default** when a choice is unavailable (System TTS off-host,
Parakeet with no model) — `make_*` always succeeds. System STT is the exception: it
runs through the warm helper, so the helper-less factory returns the INERT `SystemStt`
(never `claude_code`), and enabling it is availability-gated at the MCP layer so it
never silently falls back.

**TTS** (`Tts`):
| variant | how |
| --- | --- |
| `Kokoro` (default) | native in-process synth: `ort` (ONNX) + `voice-g2p` + `rodio`, via the warm `ds-helper` child. On macOS there is also a Core ML / ANE path (`ds-tts/src/synth_coreml.rs` + `ane_voices.rs`, FluidAudio's `KokoroAneManager` over the `kokoro-82m-coreml` repo in `ds-model/src/coreml_repo.rs`), selected by the `provider` ladder (`ane` ahead of `ort`). English g2p is pure-Rust (`voice-g2p`); non-English Kokoro voices (es/fr/it/pt/hi/ja/zh) phonemize via an OPTIONAL external `espeak-ng` (GPL — invoked as a separate process, never linked), bridged to Kokoro phonemes. Without espeak-ng, non-English is gated off in favor of System voices. |
| `System` | macOS `say` |

**STT** (`Stt`):
| variant | how | start | stop |
| --- | --- | --- | --- |
| `BuiltIn` (token `built_in`, **default**) | DontSpeak's built-in local STT (bundled Parakeet model), via the warm helper | tell the helper to `listen` (it opens the mic + buffers) | end the listen → join the final transcript → paste it (focus-gated) |
| `ClaudeNative` (token `claude_code`) | drives Claude Code's own voice input | one tap of CC's bound `voice:pushToTalk` key (default `Space`, tap toggle), focus-gated | one tap of that key |
| `System` (token `system`) | macOS on-device `SFSpeechRecognizer` (en-US), via the warm helper like the built-in engine | tell the helper to `listen` (mic + buffer) | end the listen → batch-recognize the buffer on-device → paste it (focus-gated) |

`stt_engine` is the ordered path ladder (`resolved_stt` picks the first usable rung):
`claude_code` ⇒ delegate to Claude Code's own
voice dictation (we synthesize its bound key); `built_in` ⇒ DontSpeak's built-in (Parakeet)
engine if its model is present, else degrade to `claude_code`; `system` ⇒ macOS on-device
`SFSpeechRecognizer` — availability-gated at enable time (authorized + on-device-capable)
and INERT (no degrade to `claude_code`) when unavailable, so it never silently falls back.

## Caps-Lock dictation

Caps Lock is a **tap toggle**: one tap starts recording, the next submits. The
engine polls the **latched Caps-Lock LED** every 30 ms — read via
`CGEventSourceFlagsState(HIDSystemState)` masked with the AlphaShift bit — and
recording strictly **mirrors** that LED: an OFF→ON edge starts, ON→OFF stops. The
OS latches the LED, so even a tap too fast to register as a momentary key-down
still flips the bit and is caught on the next poll, and the LED can never drift out
of sync with recording. `IOHIDManager` watches the **physical** key separately
(covered by the Accessibility grant — which subsumes Input Monitoring) solely to
detect a **long press** (≥
`long_press_ms`), which force-resets to idle and drives the LED off. For the `claude_code`
STT path, `start()`/`stop()` each emit one focus-gated tap of Claude Code's bound
`voice:pushToTalk` key (default `Space`), toggling CC's own recording. No DriverKit.

## TTS pipeline

The engine owns a FIFO TTS queue served by the warm `ds-helper --serve` child
(the Kokoro model stays loaded). It is ONE plain queue — no narration/reply kinds and no
cap; what gets spoken is decided upstream by the `narrate` setting. On a record barge-in
the queue pauses and resumes the interrupted item (whatever it is) on cancel — while a
*submit* drops that window's still-pending speech per `drop_speech_on`. Playback is gated
on `mic_active()` so speech never feeds back into a live recording.

**Earcons** (the `Earcon` RPC → the helper's `cue` op) play a short audible cue *outside*
the queue, mixed over any in-flight speech on the same rodio output: a "ding" when Claude
finishes a turn (the **Stop** hook) and a distinct cue when Claude is waiting on you — a
permission/idle notification (the **Notification** hook). Each cue's **sound** is its on/off
(`earcon_reply_sound` / `earcon_needs_input_sound`): a cue plays only when its sound is set AND
resolves — a bundled system-sound NAME (resolved against the OS's bundled sounds, no hardcoded
paths) or an absolute path; empty or unresolvable is silently off. The
reply ding defaults to the OS chime (`ding` on Windows, `Tink` on macOS, `message` on Linux) so
it rings out of the box; needs-input ships off. Honors global mute. The helper decodes
`.aiff`/`.wav`/`.oga` via rodio's symphonia decoders (macOS VPIO full-duplex, which has no rodio
mixer, falls back to `afplay`).

## Parakeet STT (local)

`Parakeet` dictation runs **through the same warm helper child** as TTS, not
in-process: on Caps-ON the engine tells the helper to `listen` (the helper opens the
mic with `cpal`, resamples to 16 kHz with `rubato`, and feeds a **streaming cache-aware
FastConformer transducer** run by `ds-stt::streaming` over the shared `ort` runtime —
`transcribe-rs` is kept ONLY for its energy VAD now, not inference; on macOS the
`ane`/Core ML rung uses FluidAudio's `StreamingAsrManager` instead, `ds-stt/src/coreml.rs`).
Dictation is **streaming**: the helper emits live-preview partial transcripts as audio
arrives, and on Caps-OFF the engine takes the final transcript and pastes it via
`KeyInjector::type_text` (clipboard paste), focus-gated. The same helper `listen` path
backs the engine's "test recognition" panel and the MCP `listen` tool.

## Models & ONNX runtime

`ds-model` is the single source of truth for asset URLs/paths/digests. Assets live
under `~/Library/Application Support/DontSpeak/models/` and download on
demand (pinned SHA-256, atomic rename). ONNX inference uses `ort` with the
**`load-dynamic`** strategy: `libonnxruntime` is resolved at runtime from
`ORT_DYLIB_PATH`, so the host build needs no onnxruntime and the binary stays lean.
Kokoro and Parakeet **share** that one dylib (Microsoft ONNX Runtime 1.27.0, the
version `ort` 2.0.0-rc.12 gates on). `default-features=false` drops `ort`'s bundled
`download-binaries` (the route-B fallback).

## Runtime display is ACTUAL, not naive

The `model_status` `tts_provider` / `stt_provider` tokens (and the app's "Runtime" rows) report
what the model **actually loaded on**, NOT the configured preference — do not assume they echo
`provider`/the engine ladders:

- **TTS (Kokoro)** builds its own `ort` session, so `KokoroSynth::provider()` records the
  **realized** EP (CPU fallback included); `status::tts_provider_token` maps that live value.
- **STT (built-in)** — the streaming FastConformer runner (`ds-stt::streaming` over `ort`, or the
  FluidAudio Core ML rung) doesn't report its realized EP back to the engine, so
  `status::stt_provider_token` reconstructs it from the loader's exact gates: `ane`→`ort_cpu` without
  the FluidAudio shim, `ort_cuda`→`ort_cpu` without the fetched GPU runtime (`cuda_runtime_present`).
  A missing runtime shows `ort_cpu`, never a phantom `ort_cuda`.
- **CUDA is on-demand (Windows x86_64 only):** the GPU onnxruntime + cuDNN wheels download into
  `models/cuda/`; `cuda_runtime_present()` gates whether `ort_cuda` is real for BOTH engines.
- **Known asymmetry:** TTS catches a *session-build* fallback (runtime present but the CUDA EP fails
  to init) because the warm Kokoro child reports its live provider; STT cannot — the streaming runner
  doesn't surface its realized EP, so its token stays gate-derived. Closing it needs the loader to report back.

Guarded by `status::tests::{stt_provider_is_actual_not_naive, tts_provider_reflects_the_childs_realized_runtime}`.

## FFI

`ds-core` is built as a C-ABI staticlib (macOS app) + cdylib (the Windows/Linux
apps). The surface is small and handle-free (~29 functions): the engine lifecycle
(`ds_engine_start` / `_stop` / `_reload`); read-only probes
(`ds_engine_running_global`, `ds_kokoro_present_global`,
`ds_parakeet_onnx_present_global`, `ds_model_status_json` / `ds_model_status_wait`,
`ds_tools_json` — the same `ds-tools::catalog()` the MCP exposes, plus
`ds_libraries_json` / `ds_logs_json`); engine commands the app panels need
(`ds_set_provider` for the TTS execution provider, `ds_set_muted`,
`ds_open_voice_settings` — models download fully automatically, so there's no download
command); and the i18n family (`ds_set_locale` / `ds_locale` / `ds_t` / `ds_t_args`).
There are no voice/engine config setters — that control lives in the MCP. The committed `dontspeak.h` is
**generated by cbindgen** from `src/ffi.rs` (run
`cargo build -p ds-core --features cbindgen`); panics are caught at the boundary.

## Platform & ownership notes

`Stt` is intentionally **not `Send`**: the engine drives it from its single poll
thread, and `ClaudeNative` borrows the engine-owned `Platform` (whose macOS
CGEventSource is `!Send`) through an `Rc`. The factory takes that shared `Rc`, which
is why it's generic over the concrete platform rather than boxing a `Send` trait.
