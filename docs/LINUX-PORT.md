# Linux port

The Linux host is a GTK4/libadwaita app (`apps/linux/gtk/`, crate `ds-linux-gtk`)
hosting the engine in-process, on the same design as macOS and Windows: `ds-core`'s C
ABI is the single source of truth, and this host adds only a `ds-platform`
input/window backend, a `ds-aec` duplex-audio backend, and a thin native UI. It's
distributed as a tarball (`apps/linux/package.sh`).

## Platform backends

- `ds-platform` — evdev for Caps-Lock key state and LED, uinput for key/clipboard
  injection, x11rb for window/focus queries.
- `ds-aec` — echo cancellation via PipeWire's `module-echo-cancel`; if that module
  isn't available, the capture path falls back to a half-duplex gate. An in-process
  WebRTC-APM alternative, matching the macOS VPIO shape, is noted as a future option
  in [docs/AEC.md](AEC.md)'s "Why native OS AEC" section.

## CI

Per-commit clippy+test runs on `ubuntu-latest`. The release-only tarball build runs on
`ubuntu-26.04`/`ubuntu-26.04-arm` with `continue-on-error: true` and no test step, so a
runner-availability hiccup there doesn't block a release.

## Developing under WSL2

This machine develops the Linux app inside WSL2 Ubuntu 26.04 (WSLg: Wayland
`wayland-0` + Xwayland `:0`, PulseAudio). The full build, the GTK4 GUI (via WSLg),
audio (cpal/rodio + PipeWire), uinput injection, the in-process engine, MCP, and
TTS/STT all run and are exercised here. Caps-Lock state comes from an evdev keyboard
node, which WSL2 doesn't expose — so the dictation trigger's runtime behavior is
verified on bare-metal Linux hardware rather than in this dev environment.
