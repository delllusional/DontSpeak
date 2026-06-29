# Full-duplex coexist — Windows & Linux port plan

> **Status (Windows):** Steps 1, 3, and 4 are implemented.
> - **§3 / §10.1** — `DuplexAudio` generalized with `owns_render()`; the helper's
>   render path branches on `render_via_duplex` (capture-only backends keep rodio).
>   No behavior change on macOS (VPIO still `owns_render() == true`).
> - **§5A** — Windows `ds-aec` backend (`src/windows.rs`): WASAPI capture in the
>   Communications category (capture-side AEC). On-device probe confirms it opens,
>   negotiates the mix rate, and the event-driven loop delivers frames. The AEC
>   echo-suppression *quality* check (§9) still needs a real mic + speaker run.
> - **§5C** — WinUI: the tray pill (recording-over-speaking, `tray_indicator`-gated)
>   was already present; added the dictation overlay (`DictationPanel.cs`, a Win32
>   layered no-activate/click-through/topmost window) + the `dictation` JSON parse.
>   It is wired into `App.xaml.cs` (instantiated and driven on status updates) — still
>   needs an on-device WinUI build to verify rendering.
>
> Remaining: WebRTC-APM-on-macOS (§4/§4a, step 2). Linux §6 native backend is implemented.


Plan for bringing **full-duplex coexist** (dictate / interrupt by Caps tap *while*
the TTS voice is still speaking, with the recogniser hearing only you) to Windows
and Linux. The macOS implementation shipped and is the reference — see `AEC.md`
(the AEC primitive + milestones) and the `coexist:` / `coexist UX:` commits.

The headline for whoever implements this: **most of the feature is already
cross-platform and done.** The engine demux, the gesture state machine, the helper
protocol, the config plumbing, and the MCP surface are all platform-agnostic Rust
that ships on every OS today. The per-OS work is narrow: one new `ds-aec` backend
(the echo-cancelled capture primitive) plus app-UI parity. Read §2 before estimating
— it is much less than it looks.

---

## 1. What "coexist" is (recap of the shipped behavior)

- A warm `ds-helper --serve` child holds Kokoro (TTS) + Parakeet (STT) warm.
- In full-duplex the helper renders a reply AND runs a **concurrent listen thread**
  draining an echo-cancelled mic at the same time (`concurrent_listen_loop` →
  `transcribe_loop`), emitting `PARTIAL`/`FINAL`/`LDONE` while the playback thread
  emits `DONE` for the speak.
- The engine owns a **persistent stdout reader** that demuxes those lines into a
  speak slot (`DONE`/`STATS`/`ERR`) vs a listen slot
  (`LDONE`/`PARTIAL`/`FINAL`/`STTSTATS`/`STTERR`), so a speak and a listen run at
  once. Ending a listen uses the `lstop` op (never `stop`), so it can't cancel a
  concurrent reply.
- Caps gesture (full-duplex): idle tap → dictate (voice keeps playing); idle
  long-press → cancel the voice + dictate; dictating short-press → submit (single
  press); dictating long-press → discard (voice keeps playing). There is **no
  implicit voice barge** — stopping the voice is an explicit gesture.
- The menu-bar pill shows recording (orange) over speaking (purple) while you
  dictate; a stripped panel shows the live transcript.
- `full_duplex` is an MCP-only config flag (`set_config`), scoped to the
  Parakeet + Kokoro path; anything else degrades to the half-duplex gate.

---

## 2. What is ALREADY cross-platform (do NOT reimplement)

All of this is portable Rust / shared contract and ships on Windows + Linux today:

- **Engine demux** — `dontspeakd/src/tts.rs` (`reader_loop`, speak/listen slots,
  `play`/`listen`/`stop_listen`, the lifecycle mutex). No OS-specific code.
- **Gesture state machine** — `dontspeakd/src/lib.rs` (`tick`, `start_recording`,
  `stop_recording`, `apply_long_press`/`long_press_action`, `fd_discard`,
  `fd_cancel_voice`, single-press submit via the steady `!down` check, the
  `dictation.local_stt` flag). It reads `TtsManager::full_duplex_active()` — already
  set from `full_duplex_wanted(cfg)` on every platform.
- **Helper protocol + concurrent listen** — `ds-tts/src/bin/ds_helper/`
  (`serve`, `concurrent_listen_loop`, `transcribe_loop`, the `listen`/`lstop`/`stop`
  ops). The protocol (`LDONE` vs `DONE`, etc.) is the same everywhere.
- **Config + MCP** — `ds-config` (`full_duplex`, `capture_gain`), `ds-tools` +
  the `dontspeak` binary's `mcp` module (`set_config`), the `model_status` JSON (incl.
  `dictation.local_stt`,
  `running.stt_active`).
- **The C ABI** (`ds_core`) that the WinUI app already loads
  (`apps/windows/winui/Native.cs`) — same `ds_model_status_json` etc. as macOS.

So the engine *already* asks for full-duplex on Windows/Linux; it just can't get an
echo-cancelled mic yet. That is the gap.

---

## 3. The one architectural difference vs macOS

macOS uses **VoiceProcessing I/O (VPIO)**: ONE unit owns BOTH the speaker render and
the mic capture on one clock, so we route TTS render *through* it (the render is the
AEC reference) and no delay/drift alignment is needed. That is why, on macOS, the
helper skips rodio and pushes PCM into the VPIO render ring.

**Windows and Linux native AEC is capture-side**: the OS audio engine
(Communications APO) or the sound server (PipeWire/PulseAudio `module-echo-cancel`)
taps the *system render endpoint* as the reference **itself**. So:

> On Windows/Linux, **TTS render stays on the existing rodio path, untouched.** Only
> the CAPTURE side changes — open the echo-cancelled stream/source and run the
> concurrent listen on it. This is *simpler* than macOS, not harder.

The only path that is harder is linking the **WebRTC APM** directly in-process
(§4 fallback): then you must tap the render reference and maintain a delay estimate
yourself. Prefer the native graph-level AEC where the OS/server owns the reference.

### Abstraction change this implies

`ds-aec`'s `DuplexAudio` currently assumes "owns render + capture" (macOS). Generalize
it with one capability bit so the helper knows whether to keep rodio:

```rust
impl DuplexAudio {
    /// macOS VPIO: true (we feed render via `render_push`, skip rodio).
    /// Windows/Linux capture-side AEC: false (rodio still renders; this only
    /// provides the echo-cancelled capture).
    pub fn owns_render(&self) -> bool;
    pub fn capture_handle(&self) -> CaptureHandle; // must be Send + Sync
    pub fn capture_rate(&self) -> u32;
    pub fn render_push(&self, pcm_24k: &[f32]);    // no-op when !owns_render
    pub fn render_pending(&self) -> bool;          // false when !owns_render
    pub fn render_clear(&self);                     // no-op when !owns_render
    pub fn barge_flag(&self) -> Arc<AtomicBool>;    // for explicit stop/cancel
}
```

Then in `ds_helper/serve.rs::serve`, gate on `owns_render()` instead of
`duplex.is_some()`:
- the `device` (rodio) open (`let device = if duplex.is_some()…`) becomes "open rodio
  **unless** the duplex owns render";
- the several `match (&duplex, &player)` render arms (player creation, the per-batch
  push, the wait-for-end, the barge clear) push to VPIO only when `owns_render`, else
  append to / drive the rodio `player` exactly as the half-duplex path does. A clean
  way: compute `let render_via_duplex = duplex.as_ref().is_some_and(|d| d.owns_render())`
  once and branch on that, so a capture-only duplex transparently uses the rodio
  player it already opened.

Everything else in the serve loop (the concurrent listen thread off
`capture_handle()`, `transcribe_loop`, the protocol) is unchanged.

---

## 4. Native AEC engines per platform (research hints — verify when implementing)

> These are starting points; the implementing party should confirm current API
> shape, quality, and availability. Quality varies by device/driver/distro.

### Windows
1. **WASAPI "Communications" category** *(recommended primary).* Open the mic with
   the `windows`/`wasapi` crate and call `IAudioClient2::SetClientProperties` with
   `AudioClientProperties { eCategory: AudioCategory_Communications }` **before
   `Initialize`**. The OS engages the capture-side AEC APO (+ Win11 *Voice Clarity*),
   managing the render-endpoint loopback reference itself. Do **NOT** set
   `AUDCLNT_STREAMOPTIONS_RAW` — RAW opts *out* of processing. `cpal` cannot set
   `SetClientProperties`, so this is a direct WASAPI capture replacing cpal input on
   Windows only.
2. **Voice Capture DSP (DMO)** — the `CWMAudioAEC` Microsoft Media Object (AEC + AGC
   + noise suppression) that references the render stream. Older, explicit, well
   documented; good if the Communications APO route is flaky on a target device.
3. **AudioGraph echo-cancellation** (WinRT `Windows.Media.Audio`) — higher-level
   graph with the communications category + built-in echo-cancellation effect.
4. **Fallback:** WebRTC APM (below). Quality depends on the endpoint's installed
   APO, so keep the fallback wired.

### Linux
1. **PipeWire `libpipewire-module-echo-cancel`** *(primary on modern distros)* — runs
   the WebRTC canceller and exposes a **cancelled virtual source**; the app just opens
   that named source. Zero in-process DSP.
2. **PulseAudio `module-echo-cancel aec_method=webrtc`** — same idea on Pulse systems.
   Ship a config drop-in + docs for both; the engine records the named cancelled
   source.
3. **In-process WebRTC APM** (`webrtc-audio-processing` crate, tonarino; `bundled`
   feature needs clang/meson/ninja) — deterministic regardless of the user's server
   config. Feed `process_render_frame` (a TTS tap) + `process_capture_frame` (mic) and
   maintain a `set_stream_delay` estimate.
4. **SpeexDSP** echo canceller — lighter, lower quality; a last resort.

### Universal fallback (all three OSes) — and the recommended FIRST backend
**WebRTC Audio Processing Module** works on Windows, Linux, *and* macOS. It is the
safety net when a native path is missing or low quality. Cost: you own the
render-reference tap + delay/drift alignment (it is NOT a graph module, it is a frame
processor — feed `process_render_frame` from a TTS tap, `process_capture_frame` from
the mic, and maintain a `set_stream_delay` estimate). Wire it once behind the same
`DuplexAudio` contract (`owns_render() = true`, since you control the reference tap),
reusable everywhere.

**Build it first, on macOS.** It is the single highest-leverage piece because it
de-risks BOTH ports on the machine you already have: running the WebRTC backend on a
Mac exercises the *exact* capture-side shape the Windows/Linux native backends will
use (rodio keeps rendering, the AEC is a frame processor with a reference tap +
delay estimate, the concurrent listen drains its cancelled capture). Shake out the
delay/drift alignment and the demux behavior on macOS, and the native Win/Linux
backends reduce to "open the OS's cancelled source and plug into a proven path."

See §4a for why this is worth offering on macOS as a *selectable* backend even though
VPIO stays the default.

### 4a. VPIO vs WebRTC on macOS — offer both, selectable
Keep **VPIO** the macOS default (best quality, hardware-tuned, and it owns render +
capture on ONE clock so far-end/near-end are aligned for free — no delay work). But
expose **WebRTC APM** as an alternative AEC backend, chosen like the execution-provider
setting. Benefits of having it on macOS:
- **De-risks the port** (above) — the main reason.
- **Tunable** where VPIO is a black box: explicit AEC aggressiveness, noise
  suppression, clean AGC-off, high-pass — an escape hatch if VPIO's voice-comm
  coloring ever caps Parakeet accuracy (we already had to disable VPIO's AGC).
- **Render decoupling**: TTS stays on the smooth rodio path and is merely *tapped* as
  the reference, instead of routed through VPIO's realtime render thread — which
  sidesteps the whole RT-render-starvation chop class of bug (the one CoreML papered
  over) and stops seizing the output device.
- **A real full-duplex fallback** when `VPIO::open()` fails (split devices, odd audio
  config) instead of dropping to half-duplex.

The cost VPIO avoids and WebRTC must pay: **delay + clock-drift alignment.** On macOS
the reference is the PCM you synthesized (you have it), but time-aligning it to what
the mic hears — and compensating drift if render/capture sit on different clocks — is
the genuinely hard part. That, plus higher CPU and the clang/meson/ninja build deps,
is why VPIO stays the default and WebRTC is an *option*. The two are mutually
exclusive (never run both cancellers at once); select one, like `set_provider`.

---

## 5. Windows implementation plan

**A. `ds-aec` Windows backend** (`rust/crates/ds-aec/src/windows.rs`, gate
`#[cfg(windows)]` in `lib.rs` alongside the macOS/stub arms).
- `DuplexAudio::open()` → open a WASAPI capture client in the Communications category
  (§4.1). `owns_render() = false`. `capture_rate()` = the negotiated mix rate.
- `CaptureHandle` (Send + Sync) drains the cancelled capture ring — mirror the macOS
  `CaptureHandle` (lock-free SPSC ring from the WASAPI event-driven capture thread to
  the helper's concurrent-listen thread).
- `render_push`/`render_pending`/`render_clear` are no-ops; `barge_flag` returns an
  `AtomicBool` the explicit `stop` path can set.
- Fail-quiet: any COM/format error → `Err`, so the helper degrades to half-duplex.

**B. Helper** — apply the `owns_render()` generalization (§3). With it false, rodio
keeps rendering; the concurrent listen drains the WASAPI cancelled capture. No other
helper change.

**C. WinUI app parity** (`apps/windows/winui/`):
- **Tray pill** (`TrayIcon.cs`) — recording (orange) overrides speaking (purple) while
  `running.stt_active` is true; gate by `tray_indicator` (`none|stt|tts|both`). The
  state already arrives in `model_status_json`.
- **Dictation panel** — a borderless, **non-activating, click-through, topmost**
  window showing only the transcript (`dictation.text`), shown when
  `awaiting_confirm || (recording && local_stt)`. WinUI/Win32 equivalent of the macOS
  `OverlayPanel` (use `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST`).
- **Caps gesture** — the engine's `tick` already drives recording from the latched
  Caps-Lock LED + physical edges via `ds-platform/windows`. Verify the Windows
  `ds-platform` exposes `caps_lock_on()` / `caps_physically_down()` / `set_caps_lock()`
  / focus-gated paste. If the half-duplex dictation already works on Windows, this is
  done — coexist adds no new gesture code.

**D. Config** — none. `full_duplex` is already in `set_config`; the engine already
calls `full_duplex_wanted` on Windows.

---

## 6. Linux implementation plan

Linux can run **headless** (the standalone `dontspeakd` host — `apps/linux/enable-daemon.sh`
wires it as a systemd user service — + system keybindings + uinput) **or** with the native
GTK4/libadwaita GUI host (`apps/linux/gtk/`, crate `ds-linux-gtk`, with its own tray +
dictation overlay; see `docs/LINUX-PORT.md`). Either way Linux coexist is mostly the capture
primitive + the (already cross-platform) engine.

**A. `ds-aec` Linux backend** (`rust/crates/ds-aec/src/linux.rs`) — IMPLEMENTED.
- Preferred: open the **cancelled virtual source** exposed by PipeWire/PulseAudio
  `module-echo-cancel` (§4) by name. `owns_render() = false`; rodio renders normally;
  the WebRTC canceller in the server references the render endpoint. `CaptureHandle`
  drains that source.
- Deterministic alt: link **`webrtc-audio-processing`** in-process (§4.3),
  `owns_render() = true` with a TTS render tap + delay estimate. Heavier build deps;
  make it a Cargo feature.
- Ship the PulseAudio/PipeWire config drop-ins + docs (mirror `apps/linux/udev-rule.txt`
  style) so the cancelled source exists.
- Fail-quiet → half-duplex.

**B. Helper** — same `owns_render()` generalization (§3); nothing Linux-specific.

**C. UX** — two paths now exist:
- GUI host (`apps/linux/gtk/`): the GTK4 app provides a state-driven tray + a
  `gtk4-layer-shell` dictation overlay fed by the same status push (the macOS panel
  analogue). This is the shipped GUI parity.
- Headless: rely on the existing dictation flow; surface the live transcript via the
  Claude Code UI / a `libnotify` toast on final. The `model_status.dictation` fields
  are already emitted; a tiny notifier can read them.

**D. Gesture** — driven by the daemon's Caps/keybinding path (`ds-platform/linux`,
uinput). Confirm `caps_lock_on()` works under the user's session (X11 vs Wayland — LED
read may need `/sys/class/leds` or an X call). If half-duplex dictation already works
on Linux, coexist needs no new gesture code.

---

## 7. The `ds-aec` contract both backends must satisfy

Whatever the native engine, each backend implements the §3 `DuplexAudio` surface so
the helper and engine stay untouched:
- `open() -> Result<Self, String>` (fail-quiet → half-duplex),
- `capture_rate() -> u32`, `capture_handle() -> CaptureHandle` (Send + Sync, drains an
  echo-cancelled mono f32 ring),
- `owns_render() -> bool` + `render_push`/`render_pending`/`render_clear` (real on an
  owns-render backend, no-ops otherwise),
- `barge_flag() -> Arc<AtomicBool>` for explicit stop/cancel.

The stub (`stub.rs`) stays for any platform without a backend — `open()` fails, caller
degrades to half-duplex. Keep `DuplexAudio` `!Send` only if the backend requires it
(macOS does); the **`CaptureHandle` must be Send + Sync** regardless, since the
concurrent-listen thread holds it.

---

## 8. Rollout / safety (unchanged from macOS)

- `full_duplex` (default **off**) gates everything; off ⇒ exactly today's half-duplex
  behavior, untouched.
- `DuplexAudio::open()` failure (no AEC APO, split devices, missing server module,
  unsupported OS) → fall back to half-duplex cpal + rodio. AEC is never a hard dep.
- Scope stays Parakeet + Kokoro (`full_duplex_wanted`); other engine combos use the
  gate.

---

## 9. Testing per platform

- `ds-aec` pure pieces (ring wiring, resamplers) unit-tested; the device backend is
  not unit-tested (real I/O) — exercise via a probe bin (mirror `--coexist-probe`) and
  on-device.
- **Echo-suppression check:** play a known tone / a long TTS reply while capturing;
  confirm captured RMS during playback is near the no-playback floor (the macOS path
  logs this as the `coexist-listen: … rms=…` line — reuse it; both `transcribe_loop`
  callers already emit it).
- **The real test:** TTS a long reply, dictate over it, confirm the transcript is your
  words only (no TTS bleed) and the reply keeps playing — exactly the macOS acceptance
  test.
- Reuse the engine-level `coexist_smoke` ignore-test (`tts.rs`) — it is OS-agnostic
  (drives `TtsManager` start→speak+listen→stop) and will exercise the new backend end
  to end once `DuplexAudio::open()` succeeds on the target.

---

## 10. Suggested sequencing

1. Generalize `DuplexAudio` with `owns_render()` + the helper's render branch points
   (§3). Pure refactor, no behavior change on macOS — verify `coexist_smoke` still
   green. **Do this first; it unblocks everything below.**
2. **WebRTC APM backend, built and validated ON macOS** (§4/§4a) — the
   capture-side/frame-processor shape every native backend will reuse. Get the
   reference tap + `set_stream_delay` alignment right here, where you can A/B it
   against the working VPIO path on the same machine. Expose it as a selectable AEC
   backend (VPIO default; like `set_provider`). This de-risks both ports before
   touching Windows or Linux.
3. Windows `ds-aec` native backend (§5A, WASAPI Communications) → engine-level
   `coexist_smoke` on Windows; WebRTC APM (step 2) is the ready fallback.
4. WinUI parity (§5C) → full on-device coexist on Windows.
5. Linux `ds-aec` native backend (§6A, the cancelled source first) → `coexist_smoke`
   on Linux; WebRTC APM as the deterministic alternative.
6. Linux UX (§6C, notifier).

Windows before Linux (it was sequenced first — at the time Linux had no GUI to validate
against; a native GTK host has since landed, see §6C). The WebRTC backend lands before
either so every platform has a proven, deterministic path when the native one is missing
or low-quality.
