# DontSpeak

A local voice layer for Claude Code, Codex, and Claude Desktop: your agent speaks its replies aloud, and you dictate back with one key.

## Install

One command — it downloads the prebuilt app for your OS, wires the MCP server + voice hooks into every client, and launches it (the voice models download themselves on first run):

```sh
# macOS / Linux
curl -fsSL https://dontspeak.org/install.sh | sh
```
```powershell
# Windows (PowerShell)
irm https://dontspeak.org/install.ps1 | iex
```

Or just tell your agent: **"Install DontSpeak.org app."** — it reads [dontspeak.org/llms.txt](https://dontspeak.org/llms.txt) and does it. Start a new session afterwards so the MCP server loads.

**After installing:** macOS grants Accessibility + Microphone on first launch; Linux prints a one-time `sudo` udev step for Caps-Lock capture.

**Build from source (developers):** `git clone https://github.com/delllusional/DontSpeak && cd DontSpeak && ./scripts/install.sh` (needs a Rust toolchain).

**Uninstall** (app + integrations + all data/models): macOS/Linux — `~/.local/bin/dontspeak-uninstall` (or `scripts/uninstall.sh` from a checkout); Windows — Settings › Apps › DontSpeak. To unwire the clients but keep the app: `dontspeak wire --all --remove`.

## What it does

- **Speaks the agent's replies** aloud through a local neural voice, or the OS system voice.
- **Turn digests** — a per-turn instruction has the agent lead each reply with short `> ` lines; only those are spoken verbatim, so a long reply gets a short spoken summary instead of the whole wall of text read aloud.
- **Caps Lock to talk** — tap to start/stop, double tap skips speech (or pastes without Enter after dictation), long press cancels.
- **Hands-free mode** — an optional always-listening mode that dictates continuously without the key (see [docs/ALWAYS-LISTENING.md](docs/ALWAYS-LISTENING.md)).
- **Driven over MCP** — voices, language, engine, rate, and toggles are all tools your agent can call.
- (Speaker diarization/speaker-lock — labeling enrolled voices and restricting dictation to yours — is implemented but hidden behind an internal flag pending more testing; not yet available.)

## Caps Lock gestures

The Caps-Lock LED is the state light: **lit = recording, dark = idle.**

| Gesture | Dark (idle) | Lit (recording) |
|---|---|---|
| **Single tap** | Start recording (or pause the voice if dictation is off) | Stop and submit (paste + Enter) |
| **Long press** | Silence the voice | Discard dictation and silence the voice |
| **Double tap** | Skip the current spoken message | Stop and paste **without** Enter |

Double tap while idle only counts when the voice is speaking — otherwise a tap starts recording immediately, zero added latency. On the stop tap, a second tap within the double-tap window flips that tap's outcome (submit ↔ insert-only); it only applies to the local transcription engines (Parakeet / system), which deliver the transcript after the stop tap. Which gesture submits is configurable, not fixed: `double_tap_submits` (default `false`) swaps the table above — single-submits/double-inserts becomes single-inserts/double-submits. Long-press threshold: `long_press_ms`. Hands-free [always-listening mode](docs/ALWAYS-LISTENING.md) ignores the Caps key.

## Models & runtimes

- **TTS** — Kokoro-82M, or the OS system voice.
- **STT** — a built-in streaming recognizer (NeMo FastConformer 80ms across platforms; Parakeet TDT 0.6b v2 via Core ML on macOS), the macOS system recognizer, or Claude Code's dictation.
- (Diarization / speaker-lock — pyannote segmentation + WeSpeaker embeddings, with SepFormer separation — implemented, hidden behind an internal flag pending more testing.)

Each model runs on the fastest backend available, picked by the `provider` ladder (`["ane", "cuda", "cpu"]`):

| Platform | Backend |
|---|---|
| macOS (Apple Silicon) | Apple Neural Engine via FluidAudio Core ML → ONNX Runtime CPU |
| Windows (x86_64) | ONNX Runtime CUDA (NVIDIA GPU) → CPU |
| Linux (x86_64) | ONNX Runtime CUDA (NVIDIA GPU) → CPU |

## MCP tools

`speak` · `listen` · `stop_speech` · `mute` · `get_status` · `list_voices` · `set_config` · `setup_integration` — full descriptions and parameters in [docs/MCP-TOOLS.md](docs/MCP-TOOLS.md). (Diarization tools `diarize`/`manage_speakers` exist but are hidden pending more testing.)

## License

[MIT](LICENSE). Third-party model and dependency attributions are in [NOTICE.md](NOTICE.md).
