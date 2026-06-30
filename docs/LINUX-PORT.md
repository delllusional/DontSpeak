# Linux port

**Status (2026-06): shipped.** GTK4/libadwaita GUI host (`apps/linux/gtk/`, crate
`ds-linux-gtk`) + headless `dontspeakd` daemon, Linux backends for `ds-platform`
(evdev/uinput/x11rb) and `ds-aec` (PipeWire `module-echo-cancel`), `.deb`/`.rpm` packaging
(`apps/linux/package.sh`) — built and tested in CI on `ubuntu-26.04`.

Design rule (same as macOS/Windows): the `ds-core` Rust engine behind the C ABI is the
single source of truth, and each platform adds only a `ds-platform` input/window backend, a
`ds-aec` duplex-audio backend, and a thin native host — no logic is duplicated into the host.

## Still pending (hardware-gated)

- **Caps-Lock LED *trigger* + uinput injection** — compile-verified and event-driven via
  evdev, but the dictation trigger and key/clipboard injection need a bare-metal run (no
  `/dev/input` evdev keyboard node under WSL; see caveat below).
- **AEC echo *quality*** — the `libpipewire-module-echo-cancel` capture path is wired and
  fails quiet to a half-duplex gate, but echo suppression needs tuning on a real mic+speaker.
- **Optional in-process WebRTC-APM** — `--features webrtc-aec` builds an in-process
  `webrtc-audio-processing` AEC (taps TTS render + delay estimate, `owns_render()=true`,
  matching the macOS VPIO shape) as a documented alternative to the server-side module.

## WSL development caveat (this machine)

The dev box is WSL2 Ubuntu 26.04 under WSLg (Wayland `wayland-0` + Xwayland `:0`,
PulseAudio). What runs here: the full build/compile, the GTK4 GUI host (WSLg Wayland),
audio (cpal/rodio + PipeWire), uinput injection (`/dev/uinput` present), the daemon, MCP,
TTS/STT. What does **not** run here: Caps-Lock LED read — WSL2 exposes **no `/dev/input`
evdev keyboard node**, so the dictation *trigger* must be verified on bare-metal Linux. The
code is written for evdev and compile-verified in WSL; trigger runtime test is deferred to
real hardware.
