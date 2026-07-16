# Linux port

GTK4/libadwaita host (`apps/linux/gtk/`, crate `ds-linux-gtk`) — same in-process
engine model as macOS/Windows via `ds-core`. Adds `ds-platform` input/window,
`ds-aec` duplex audio, thin native UI. Ship as tarball (`apps/linux/package.sh`).

## Platform backends

- `ds-platform` — evdev Caps + LED, uinput inject, x11rb focus/window.
- `ds-aec` — PipeWire `module-echo-cancel`; else half-duplex. In-process WebRTC AEC
  not implemented — [AEC.md](AEC.md).

## CI

Per-commit clippy/tests: `ubuntu-latest`. Release: `ubuntu-26.04` / arm; GTK tests
required; tarball upload best-effort.

## WSL2

Dev target: WSL2 Ubuntu 26.04 (WSLg Wayland + Xwayland, Pulse). Build, GUI, audio,
uinput, engine, MCP, TTS/STT all work. Caps-Lock needs a real evdev keyboard node —
verify dictation on bare-metal Linux.
