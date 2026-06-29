# Linux port plan

> **Status (2026-06):** Phases 1–4 IMPLEMENTED and compile-verified on Linux (Ubuntu 26.04 /
> WSLg). Phase 1 (platform backend) is runtime-validated — `dontspeakd` boots and runs the
> RPC/TTS/STT service; the GTK host launches cleanly. Hardware-gated items still pending a
> bare-metal run: the Caps-Lock LED *trigger* + uinput injection (no `/dev/input` keyboard or
> writable `/dev/uinput` under WSL) and AEC echo quality (needs a real mic+speaker + the
> echo-cancel module loaded). The in-process WebRTC-APM AEC alternative (§6A) is left as a
> documented future option.

Bring DontSpeak to Linux at parity with the shipped macOS app and the now-functional
Windows port, reusing the existing Rust core verbatim and adding only the per-OS
backends + a native GUI host. The macOS SwiftUI app and the Windows WinUI app are the
two reference implementations; the Linux host is the third native host over the **same
`ds-core` C ABI**.

Design rule (unchanged from the other ports): the **Rust engine is the single source of
truth**; each platform adds (1) a `ds-platform` input/window backend, (2) a
`ds-aec` duplex-audio backend, and (3) a thin native GUI host that hosts the engine
in-process and renders status pushed over the FFI. No logic is duplicated into the host —
anything shared is extracted into a crate first (it already is).

## UI framework decision

Native, modern GNOME stack (mirrors macOS=SwiftUI, Windows=WinUI): **GTK4 + libadwaita**
driven from Rust (`gtk4-rs` / `libadwaita-rs`). Tray via **StatusNotifierItem** (`ksni`,
pure-Rust DBus — GTK4 dropped legacy StatusIcon). Focus-safe dictation overlay via
**`gtk4-layer-shell`** (wlr-layer-shell, `keyboard_interactivity=none`) on Wayland, with an
X11 override-redirect fallback. The host is one Rust binary linking the engine cdylib, so
unlike mac (Swift) / Windows (C#) there is no language boundary — the GUI calls
`ds_*` directly.

Target distro baseline: **Ubuntu 26.04 LTS** (GTK 4.22, libadwaita 1.9, gtk4-layer-shell
1.3), Fedora 41+. Both X11 and Wayland sessions supported.

## Reference-port crosscheck (what each platform ships)

| Concern | macOS | Windows | Linux (shipped) |
| --- | --- | --- | --- |
| Platform input (`ds-platform`) | IOHID/IOKit/CG | LL hook + SendInput + UIA | **evdev/uinput + x11rb** (`linux.rs`; `new()` never fails — capability gaps degrade to a preflight warning) |
| Duplex audio (`ds-aec`) | VPIO | WASAPI | **PipeWire/PulseAudio `module-echo-cancel` source** (`linux.rs`, wired in `lib.rs`; optional in-proc WebRTC APM behind a feature) |
| System TTS | `say` | PowerShell | **`spd-say` wired** |
| System STT | SFSpeech | (deferred) | **none → inert/degrade** (correct) |
| Kokoro/Parakeet helper (cpal/rodio) | ✓ | ✓ | **cross-platform, runs on Linux** (ALSA/PipeWire) |
| Model + XDG data dirs | ✓ | ✓ | **XDG-correct** in `paths.rs` |
| Headless host (`dontspeakd`) | n/a | n/a | **boots and runs** the RPC/TTS/STT service (platform wired; signals done) |
| GUI host | SwiftUI | WinUI | **GTK4/libadwaita app** (`apps/linux/gtk/`, crate `ds-linux-gtk`) |
| Packaging | dmg/notarize | Inno installer | **`apps/linux/package.sh`** → `.deb`/`.rpm` (`cargo-deb`/`generate-rpm` metadata in the GTK `Cargo.toml`), `install-gui.sh`, systemd user unit, udev rule, AEC drop-ins |

## FFI surface the host consumes (already stable, generated `dontspeak.h`)

Lifecycle `ds_engine_start/_stop/_reload`; probes `_engine_running_global`,
`_kokoro_present_global`, `_parakeet_onnx_present_global`; status
`_model_status_json` + **blocking** `_model_status_wait(since, timeout_ms)` (the push
channel — call on a dedicated thread); control `_set_muted`, `_set_provider`; formatters
`_engine_state_word[_files]`, `_duration_live`; metadata `_tools_json`, `_homepage_url`,
`_brand_colors_json`, `_version`; i18n `_set_locale`, `_locale`, `_t`, `_t_args`; plus
`_string_free`. The Linux host links the **cdylib** (already emitted by `ds-core`).

## Phases (all four IMPLEMENTED — build-it-first, validated in order)

The four phases below are done; the bullets describe what shipped. Only the
hardware-gated runtime checks called out in the header remain (bare-metal Caps-LED
trigger, AEC echo quality, the in-process WebRTC-APM §6A option).

### Phase 1 — Platform input layer + headless daemon boots (the functional core) — DONE
`ds-platform/src/linux.rs` (deps already declared: `evdev` 0.13, `x11rb` 0.13):
- `LinuxPlatform::new()` — discover the keyboard evdev node (first device exposing
  `EV_LED`+`LED_CAPSL`), open it, build an `evdev::uinput::VirtualDevice` sink.
- `CapsLockReader::read` / `caps_lock_on` — evdev LED state (`get_led_state().contains(LED_CAPSL)`).
- `CapsKeyMonitor` — **event-driven** on Linux (evdev delivers real EV_KEY edges): implement
  `caps_event_driven()=true` + a background reader thread feeding a lossless `drain_caps_events()`
  queue (superior to polling, mirrors the Windows hook design) + `caps_physically_down` +
  `set_caps_lock` (EV_LED via uinput).
- `KeyInjector::tap_key` / `press_enter` — uinput key synthesis (chord → Linux keycodes).
- `KeyInjector::type_text` — clipboard paste (set clipboard via `arboard`/wl-clipboard, then
  uinput Ctrl+V), focus-gated, same shape as Windows.
- `FrontmostWindow::terminal_frontmost` — X11 `_NET_ACTIVE_WINDOW`+`WM_CLASS` via x11rb against
  the terminal allowlist; Wayland fail-open (documented, compositor-isolated).
- `preflight()` — verify input-group access to the evdev node; clear message + `udevadm` hint.
- `mic_watch`/`mic_active` — PulseAudio/PipeWire source-in-use probe (optional; stub `false` ok).
Exit criteria: `dontspeakd` boots on Linux, MCP server answers, Caps dictation pastes into a
terminal, TTS plays. (Caps LED read needs a real keyboard evdev node — see WSL caveat.)

### Phase 2 — Full-duplex AEC — DONE (echo *quality* pending a bare-metal mic+speaker run)
`ds-aec/src/linux.rs`, two-tier (per `docs/FULL-DUPLEX-PORT.md`):
- Default: open the server-side **`libpipewire-module-echo-cancel`** (WebRTC-backed) named
  cancelled source, capture-only (`owns_render()=false`); fail-quiet → half-duplex gate.
- Optional `--features webrtc-aec`: in-process `webrtc-audio-processing` (clang/meson/ninja),
  tap TTS render + delay estimate (`owns_render()=true`), matching the macOS VPIO shape.

### Phase 3 — GTK4 + libadwaita GUI host (`apps/linux/gtk/`, crate `ds-linux-gtk`) — DONE
- `AdwApplication`: `engine_start` on startup / `engine_stop` on shutdown (track `did_start`).
- Status push: dedicated thread in `model_status_wait` → `glib` channel → reactive UI state.
- Tray: `ksni` StatusNotifierItem, state-driven icon (idle/recording=orange/speaking=purple),
  menu = Mute / Status / Tools / Quit.
- Status/health window: `AdwWindow` + `AdwPreferencesGroup`/`AdwExpanderRow` sections
  (DontSpeak lifetime, TTS, STT, Diarization, Caps Lock), title-bar tint from live state.
- Tools window: rows from `ds_tools_json`.
- Dictation overlay: `gtk4-layer-shell` surface, glass card + speak-now glow + per-word text,
  fed by the same status push; position persisted under `~/.config/dontspeak/`.
- i18n via `ds_t`; theme via `AdwStyleManager`; autostart via
  `~/.config/autostart/dontspeak.desktop`.

### Phase 4 — Packaging + docs + CI — DONE
- `apps/linux/install-gui.sh` / `enable-daemon.sh` (local build → `~/.local/bin`, install
  udev rule, enable the systemd **user** service, wire Claude Code hooks).
- `ds-daemon.service` (systemd user unit) shipped in the package.
- `.deb`/`.rpm` via `cargo-deb`/`cargo-generate-rpm` (`[package.metadata.deb]` /
  `[package.metadata.generate-rpm]` in the GTK `Cargo.toml`, driven by `apps/linux/package.sh`;
  AppImage optional); ALSA/PipeWire + speech-dispatcher listed as deps.
- Linux build in CI: `release.yml` has a `linux` job (runs-on `ubuntu-26.04`, runs
  `apps/linux/package.sh --skip-appimage`, uploads `linux-packages`); `ci.yml` runs the
  test matrix on `ubuntu-latest` (and on every push). Linux is built and tested in CI.

## WSL development caveat (this machine)

The dev box is WSL2 Ubuntu 26.04 under WSLg (Wayland `wayland-0` + Xwayland `:0`,
PulseAudio). What runs here: the full build/compile, the GTK4 GUI host (WSLg Wayland),
audio (cpal/rodio + PipeWire), uinput injection (`/dev/uinput` present), the daemon, MCP,
TTS/STT. What does **not** run here: Caps-Lock LED read — WSL2 exposes **no `/dev/input`
evdev keyboard node**, so the dictation *trigger* must be verified on bare-metal Linux. The
code is written for evdev and compile-verified in WSL; trigger runtime test is deferred to
real hardware.
