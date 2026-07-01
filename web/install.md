# Install DontSpeak

> Instructions for an AI coding agent (e.g. Claude Code) to install **DontSpeak** —
> a local voice layer that speaks an agent's replies aloud and dictates your speech back.
> A human can also just run the one-liner for their OS.

DontSpeak ships as a native host app (which runs the speech engine in-process, captures
Caps-Lock, and downloads its voice models on first launch) **plus** an MCP server that
plugs into Claude Code, Claude Desktop, and Codex. Installing means: get the prebuilt app
for this OS/arch, wire the MCP server + hooks into every client, and launch it once.

**Prebuilt binaries only — do not build from source unless the user asks.** The bootstrap
scripts below download the latest signed release, verify its checksum, install, wire, and
launch. Reuse them; don't reimplement the steps by hand.

## One command

Detect the OS and run the matching line:

- **macOS / Linux:**
  ```sh
  curl -fsSL https://dontspeak.org/install.sh | sh
  ```
- **Windows (PowerShell):**
  ```powershell
  irm https://dontspeak.org/install.ps1 | iex
  ```

That is the whole install. It is idempotent — safe to re-run.

## What it does

1. Resolves the right asset on the latest GitHub Release of `delllusional/DontSpeak`:
   - macOS → `DontSpeak-<arm64|x86_64>.dmg`
   - Windows → `dontspeak-setup-<x64|arm64>.exe`
   - Linux → `dontspeak-<version>-<x86_64|aarch64>.tar.gz`
2. Verifies its SHA-256 against the release's `checksums.txt`.
3. Installs the app (macOS: copy to `/Applications`; Windows: silent Inno install to
   `%ProgramFiles%\DontSpeak`; Linux: extract + run the bundled `install.sh`).
4. Runs `dontspeak wire --all` — registers the MCP server (`~/.claude.json`,
   `claude_desktop_config.json`) and merges the voice hooks (`~/.claude/settings.json`,
   `~/.codex/config.toml`). Additive, backed-up, idempotent.
5. Launches the app so the voice models download automatically on first boot.

## After installing

- **Start a NEW Claude Code session** so it picks up the DontSpeak MCP server.
- **macOS:** on first launch grant Accessibility + Microphone (System Settings ›
  Privacy & Security) — one grant set, all on `DontSpeak.app`.
- **Linux:** run the one-time `sudo` udev step the installer prints (grants `/dev/uinput`
  for synthetic keys / Caps-Lock).
- Voice models download in the background; progress is shown in the app.

## Uninstall / unwire

- Remove just the client integration: `dontspeak wire --all --remove`
- Remove the app: macOS — delete `/Applications/DontSpeak.app`; Windows — Add/Remove
  Programs → DontSpeak; Linux — `~/.local/bin/dontspeak wire --all --remove` then delete
  the installed binaries under `~/.local/bin`.

## Build from source (developers only)

Only if the user explicitly wants a source build:
```sh
git clone https://github.com/delllusional/DontSpeak && cd DontSpeak && ./scripts/install.sh
```
This needs a Rust toolchain (and the per-OS GUI toolchain) and builds the binaries locally.
