# DontSpeak

Local voice layer for Claude Code, Codex, Qwen Code, and Grok: the agent speaks
replies aloud; you dictate back with Caps Lock.

## Install

Downloads the prebuilt app, wires MCP + hooks into supported clients, launches
(models download on first run):

```sh
# macOS / Linux
curl -fsSL https://github.com/delllusional/DontSpeak/releases/latest/download/install.sh | sh
```
```powershell
# Windows
irm https://github.com/delllusional/DontSpeak/releases/latest/download/install.ps1 | iex
```

Or: **"Install DontSpeak.org app."** — agent reads
[dontspeak.org/llms.txt](https://dontspeak.org/llms.txt). Start a new session so MCP loads.

**After install:** macOS prompts Accessibility + Microphone; Linux may need a one-time
`sudo` udev step for Caps Lock.

```sh
dontspeak claude
dontspeak codex
dontspeak qwen
dontspeak grok
```

Preserves client args and exit status. Codex interactive launches also prep app-server
streaming. Details: [docs/CLIENT-INTEGRATIONS.md](docs/CLIENT-INTEGRATIONS.md).

**Update:** re-run install (stop/replace/re-wire/relaunch). In-app version pill only
signals availability.

**From source:** `git clone … && ./scripts/install/local/install.sh` (Rust toolchain).

**Uninstall:** macOS/Linux `~/.local/bin/dontspeak-uninstall` (or
`scripts/install/bundle/uninstall.sh`); Windows Settings › Apps › DontSpeak. Keep app
but drop a client: `exclude_clients` in `config.toml`.

## What it does

- Speaks replies (neural or OS voice)
- Turn digests — agent summarizes in short `>` lines, spoken verbatim; short non-quote
  replies can speak whole
- Caps Lock talk — tap start/stop; double-tap skip/paste-without-Enter; long-press cancel
- Optional always-listening: [docs/ALWAYS-LISTENING.md](docs/ALWAYS-LISTENING.md)
- MCP tools for voice/engine/rate/toggles
- Diarization implemented but hidden (issue #77)

## Caps Lock gestures

LED: **lit = recording, dark = idle.**

| Gesture | Dark (idle) | Lit (recording) |
|---|---|---|
| **Single tap** | Start recording (or pause voice if dictation off) | Stop + submit (paste + Enter) |
| **Long press** | Silence | Discard + silence |
| **Double tap** | Skip current spoken message | Stop + paste **without** Enter |

Idle double-tap only while speech plays; else first tap starts recording.
`double_tap_submits` (default false) swaps stop gestures; `long_press_ms` sets threshold.
Always-listening does not use Caps Lock.

## Models & runtimes

- **TTS** — Kokoro-82M or OS voice
- **STT** — streaming FastConformer (80 ms); Parakeet TDT via Core ML on macOS; macOS
  system recognizer; Claude Code dictation. See [docs/STT-PIPELINE.md](docs/STT-PIPELINE.md)
- Diarization (pyannote / WeSpeaker / SepFormer) — issue #77

`provider` ladder default `["ane", "cuda", "cpu"]`:

| Platform | Backend |
|---|---|
| macOS Apple Silicon | ANE (FluidAudio Core ML) → ORT CPU |
| Windows / Linux x86_64 | ORT CUDA → CPU |

## MCP tools

`speak` · `listen` · `stop_speech` · `mute` · `get_status` · `list_voices` · `set_config` —
[docs/MCP-TOOLS.md](docs/MCP-TOOLS.md). Wiring automatic (`exclude_clients`); inspect with
`dontspeak wire --list` / `wire <client>`.

## License

[MIT](LICENSE). Third-party: [NOTICE.md](NOTICE.md).
