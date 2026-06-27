# Architecture

DontSpeak gives Claude Code a hands-free voice loop on macOS: tap Caps Lock to
dictate, hear replies spoken back. It is a **single native app that hosts a Rust
engine in-process**, with the Claude Code hooks and an MCP server as thin clients.

## One app hosts the engine

The engine — Caps-Lock dictation, the TTS queue, local STT, "test recognition",
model presence, and the RPC server — is a Rust core exposed over a small C ABI
(`ds_engine_start/stop/reload`). The macOS app (`DontSpeak.app`) links it and
runs it on a background thread, so the OS-permission-bearing work runs INSIDE the
one signed app bundle — **all TCC grants (Accessibility / Mic) land on that single
app, granted once** (Accessibility subsumes Input Monitoring for the Caps-key read,
so no separate Input Monitoring grant is needed). There is no separate daemon and no
launchd agent: the app is the login item, `engine_start()` on launch,
`engine_stop()` on quit. The design is portable — a headless host binary
(`dontspeakd`) is meant to run the same `engine_run` for Linux/CLI, and future
Windows/Linux apps are intended to link the cdylib and call the same FFI — but
**only macOS is functional today**: off macOS-arm64 the `ds-platform` backends are
largely stubs and the headless Linux host does not yet start cleanly, so treat
Windows/Linux as PLANNED / EXPERIMENTAL (see the platform notes below). Every other
piece (the hooks, the MCP server) is a thin client that holds no model and talks to
the engine over a Unix-domain socket (`ds-ipc`, NDJSON).

Our config lives in **`config.toml`** under DontSpeak's data dir (macOS: `~/Library/Application
Support/DontSpeak/`, alongside the downloaded `models/`) — a neutral home, separate
from any client. (We never store config in `~/.claude/settings.json`; the engine in fact
DROPS any stale `dontspeak` block a prior version seeded there, leaving that file purely
Claude Code's — its hooks and its own `voice` block, which we set separately.) After a
Caps press the STT **path** is chosen by `stt_engine` alone (`built_in` → our local Parakeet
STT, **the default**; `claude_code` → delegate to Claude Code's own voice dictation by tapping
its `voice:pushToTalk` key). Two further selectors gate subsystems (flip via the MCP
`set_config`); the engine applies changes **surgically** (no full reloads):

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

Rust workspace under `rust/` (small single-purpose crates) + the SwiftUI app in `apps/macos/`:

- **`ds-config`** — paths + our `config.toml` read/write (the config truth) + `~/.claude/settings.json` read/write for Claude Code's hooks and `voice` block (atomic merge) + the config enums.
- **`ds-ipc`** — the engine↔client RPC protocol over the Unix socket.
- **`ds-proc`** — pidfile single-speaker + process-group kill (barge-in) + mic-active probe.
- **`ds-platform`** — per-OS traits (CapsLockReader / KeyInjector / FrontmostWindow / CapsKeyMonitor). macOS impl is shipped; Linux/Windows are cfg-gated stubs.
- **`ds-model`** — model URLs/paths/pinned digests + a blocking `attohttpc` downloader (atomic temp+rename, retry, sha-verify, ORT `.tgz` extract).
- **`ds-tts`** — `Tts` trait: `KokoroTts` (native in-process synth, default) + `SystemTts` (`say`).
- **`ds-stt`** — `Stt` trait: `BuiltIn` (local, bundled Parakeet model — **the default**), `ClaudeNative` (taps Claude Code's `voice:pushToTalk` key — default `Space` — to drive CC's own dictation), `SystemStt` (macOS on-device `SFSpeechRecognizer`).
- **`ds-aec`** — the echo-cancelled duplex-audio primitive (`DuplexAudio`) for full-duplex coexist (mic open while TTS plays). macOS VPIO + Windows WASAPI; stub elsewhere. See `docs/AEC.md`.
- **`ds-engines`** — the factory: config enum → `Box<dyn Tts/Stt>`, degrade-to-default-never-silent.
- **`ds-tools`** — the single MCP tool catalog (`catalog()`), shared by the MCP server and the app's Tools view so the list never drifts.
- **`ds-i18n`** — the shared UI-string catalog (`locales/en.yml`) every platform UI renders over the FFI. See `docs/localization.md`.
- **`dontspeakd`** — the **engine**: a library (`engine_run` — Caps loop, TTS queue, RPC server, test-recognition, model status, hot-reload) **plus** a thin headless host binary intended for Linux/CLI (experimental — does not yet start cleanly off macOS). The macOS app hosts the same `engine_run` in-process via `ds-core`.
- **`ds-core`** — the stable C-ABI staticlib the app links: `ds_engine_start/stop/reload` (host the engine in-process) + read-only probes (engine liveness, model presence, status JSON). Header generated by cbindgen; cdylib for future Win/Linux hosts.
- **`dontspeak`** — the one multi-call client binary (a client of the engine socket), dispatched by subcommand. ALL transport is stdio — there is no HTTP/remote bridge.
  - no args → the stdio **MCP server** Claude Code connects to: `speak` / `stop_speak` / `status` / `listen`, `list_voices` / `set_voice`, `set_config` (one atomic setter for all persistent voice settings), `wire_client`, and the diarization tools (`diarize` / `enroll` / `forget_speaker` / `list_speakers`). `set_voice` sets — or, with no `voice`, clears — a transient session override (auto-routed Kokoro/System); `set_config` persists to our `config.toml`.
  - `dontspeak notify` (command sink — greet / mark-active / narrate / barge / earcon, replies nothing) and `dontspeak provide` (query — returns the event's `hookSpecificOutput`, e.g. the injected narration spec) → the two Claude Code hook entries, split by contract; each reads the hook JSON on stdin and routes on its event name.
  - `dontspeak wire-hooks` / `wire-desktop` → installer wiring (Claude Code hooks / Claude Desktop MCP registration).
- **`ds-helper`** (a bin in `ds-tts`) — the **one warm helper child** the engine supervises (`--serve`): it hosts **both** Kokoro TTS (`speak`) and Parakeet STT (`listen`), loading each model once. Bundled in the app on macOS. Its one-shot mode (`ds-helper <text> <voice> <rate>`) is the cold synthesis fallback (the hooks themselves never synthesize — the engine owns playback).

The macOS GUI is the **SwiftUI app** in `apps/macos/` — a menu-bar + health/permissions
panel that **hosts the engine** (`ds_engine_start`) and is the login item.
Control (voice/engine/language/rate/toggles/downloads) is via the MCP, not the app.

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
| `Kokoro` (default) | native in-process synth: `ort` (ONNX) + `voice-g2p` + `rodio`, via the warm `ds-helper` child. English g2p is pure-Rust (`voice-g2p`); non-English Kokoro voices (es/fr/it/pt/hi/ja/zh) phonemize via an OPTIONAL external `espeak-ng` (GPL — invoked as a separate process, never linked), bridged to Kokoro phonemes. Without espeak-ng, non-English is gated off in favor of System voices. |
| `System` | macOS `say` |

**STT** (`Stt`):
| variant | how | start | stop |
| --- | --- | --- | --- |
| `BuiltIn` (token `built_in`, **default**) | DontSpeak's built-in local STT (bundled Parakeet model), via the warm helper | tell the helper to `listen` (it opens the mic + buffers) | end the listen → join the final transcript → paste it (focus-gated) |
| `ClaudeNative` (token `claude_code`) | drives Claude Code's own voice input | one tap of CC's bound `voice:pushToTalk` key (default `Space`, tap toggle), focus-gated | one tap of that key |
| `System` (token `system`) | macOS on-device `SFSpeechRecognizer` (en-US), via the warm helper like the built-in engine | tell the helper to `listen` (mic + buffer) | end the listen → batch-recognize the buffer on-device → paste it (focus-gated) |

`stt_engine` is the single path selector: `claude_code` ⇒ delegate to Claude Code's own
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
mic with `cpal`, resamples to 16 kHz with `rubato`, and transcribes via
`transcribe-rs`'s `ParakeetModel` (TDT 0.6b v2 int8) over its `ort` runtime); on Caps-OFF the engine
ends the listen, joins the final transcript, and pastes it via
`KeyInjector::type_text` (clipboard paste), focus-gated. One-shot per utterance (no
streaming partials for dictation). The same helper `listen` path backs the engine's
"test recognition" panel and the MCP `listen` tool.

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
- **STT (Parakeet)** runs via `transcribe-rs` (which owns its session), so `status::stt_provider_token`
  mirrors the loader's exact gates: `ane`→`ort_cpu` without the FluidAudio shim, `ort_cuda`→`ort_cpu`
  without the fetched GPU runtime (`cuda_runtime_present`). A missing runtime shows `ort_cpu`, never a
  phantom `ort_cuda`.
- **CUDA is on-demand (Windows x86_64 only):** the GPU onnxruntime + cuDNN wheels download into
  `models/cuda/`; `cuda_runtime_present()` gates whether `ort_cuda` is real for BOTH engines.
- **Known asymmetry:** TTS catches a *session-build* fallback (runtime present but the CUDA EP fails
  to init); STT cannot — `transcribe-rs` doesn't surface its realized EP. Closing it needs an upstream hook.

Guarded by `status::tests::{stt_provider_is_actual_not_naive, tts_provider_reflects_the_childs_realized_runtime}`.

## FFI

`ds-core` is built as a C-ABI staticlib (macOS app) + cdylib (future
Win/Linux). The surface is small and handle-free: the engine lifecycle
(`ds_engine_start` / `_stop` / `_reload`); read-only probes
(`ds_engine_running_global`, `ds_kokoro_present_global`,
`ds_parakeet_onnx_present_global`, `ds_model_status_json`,
`ds_tools_json` — the same `ds-tools::catalog()` the MCP exposes); and the one
engine command the app's panel needs (`ds_set_provider` for the TTS execution
provider — models download fully automatically, so there's no download command). There are no voice/engine
config setters — that control lives in the MCP. The committed `dontspeak.h` is
**generated by cbindgen** from `src/ffi.rs` (run
`cargo build -p ds-core --features cbindgen`); panics are caught at the boundary.

## Platform & ownership notes

`Stt` is intentionally **not `Send`**: the engine drives it from its single poll
thread, and `ClaudeNative` borrows the engine-owned `Platform` (whose macOS
CGEventSource is `!Send`) through an `Rc`. The factory takes that shared `Rc`, which
is why it's generic over the concrete platform rather than boxing a `Send` trait.
