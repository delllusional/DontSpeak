# DontSpeak

A local voice layer for Claude Code, Codex, and Claude Desktop: your agent speaks its replies aloud, and you dictate back with one key.

## What it does

- **Speaks the agent's replies** aloud through a local neural voice, or the OS system voice.
- **Caps Lock to talk** — tap to record, tap again to stop and submit; tap mid-reply to barge in, long-press to cancel and discard.
- **Hands-free mode** — an optional always-listening mode that dictates continuously without the key (see [docs/ALWAYS-LISTENING.md](docs/ALWAYS-LISTENING.md)).
- **Driven over MCP** — voices, language, engine, rate, and toggles are all tools your agent can call.
- **Speaker diarization & speaker-lock** — label enrolled voices and restrict dictation to yours.

## Caps Lock gestures

The Caps-Lock LED is the state light: **lit = recording, dark = idle.**

| Gesture | Dark (idle) | Lit (recording) |
|---|---|---|
| **Single tap** | Start recording (or pause the voice if dictation is off) | Stop and submit |
| **Long press** | Silence the voice | Discard and silence |
| **Double tap** | Skip the current spoken message | Skip the current spoken message |

Double tap only counts while the voice is speaking. Hands-free [always-listening mode](docs/ALWAYS-LISTENING.md) ignores the Caps key. Long-press threshold: `long_press_ms`.

## Models & runtimes

- **TTS** — Kokoro-82M, or the OS system voice.
- **STT** — a built-in streaming recognizer (NeMo FastConformer 80ms across platforms; Parakeet TDT 0.6b v2 via Core ML on macOS), the macOS system recognizer, or Claude Code's dictation.
- **Diarization / speaker-lock** — pyannote segmentation + WeSpeaker embeddings, with SepFormer separation.

Each model runs on the fastest backend available, picked by the `provider` ladder (`["ane", "cuda", "cpu"]`):

| Platform | Backend |
|---|---|
| macOS (Apple Silicon) | Apple Neural Engine via FluidAudio Core ML → ONNX Runtime CPU |
| Windows | ONNX Runtime CUDA (NVIDIA GPU) → CPU |
| Linux | ONNX Runtime CPU |

## MCP tools

`speak` · `listen` · `stop_speech` · `get_status` · `list_voices` · `diarize` · `manage_speakers` · `set_config` · `setup_integration`

## License

[MIT](LICENSE). Third-party model and dependency attributions are in [NOTICE.md](NOTICE.md).
