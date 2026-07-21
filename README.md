# DontSpeak

Local voice layer for Claude Code, Codex, Qwen Code, Grok, and Kimi Code: the agent speaks
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
dontspeak kimi
```

Preserves client args and exit status. Codex interactive launches also prep app-server
streaming (`codex --remote`). Grok stays a direct `grok` launch; mid-turn digests ride the
host engine tail of session `updates.jsonl` (config `grok_stream`, default on). Details:
[docs/CLIENT-INTEGRATIONS.md](docs/CLIENT-INTEGRATIONS.md) and
[docs/STREAMING-NARRATION.md](docs/STREAMING-NARRATION.md).

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
`double_tap_submit` (default false) swaps stop gestures; `long_press_ms` sets threshold.
Always-listening does not use Caps Lock.

## Models & runtimes

- **TTS** — Kokoro-82M, Chatterbox Multilingual, Qwen3-TTS, or OmniVoice
  (`tts_model`), or OS voice
- **STT** — Parakeet TDT 0.6b v3 (25 European languages, detected by the model) everywhere:
  ONNX on Windows/Linux, MLX Audio on Apple Silicon, plus System Speech on macOS; Claude Code
  dictation. See [docs/STT-PIPELINE.md](docs/STT-PIPELINE.md)
- Diarization — Sortformer, with WeSpeaker speaker identity and SepFormer
  speaker-lock separation — issue #77

`provider` ladder default `["mlx", "cuda", "cpu"]`:

| Platform | Backend |
|---|---|
| macOS Apple Silicon | MLX Audio → ORT CPU |
| macOS Intel | ORT CPU |
| Windows / Linux x86_64 | ORT CUDA → CPU |

`coreml` remains available as an explicit macOS TTS provider through ONNX Runtime.

## MCP tools

`speak` · `listen` · `stop` · `mute` · `status` · `usage` ·
`voices` · `set_config` —
[docs/MCP-TOOLS.md](docs/MCP-TOOLS.md). Wiring automatic (`exclude_clients`); inspect with
`dontspeak wire --list` / `wire <client>`.

## License

[MIT](LICENSE). Third-party: [NOTICE.md](NOTICE.md).
