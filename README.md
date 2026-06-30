# DontSpeak

A local voice layer for Claude Code, Codex, and Claude Desktop: your agent speaks its replies aloud, and you dictate back with one key.

## What it does

- **Speaks the agent's replies** aloud through a local neural voice, or the OS system voice.
- **Caps Lock to talk** — tap to record, tap again to stop and submit; tap mid-reply to barge in, long-press to cancel and discard.
- **Hands-free mode** — an optional always-listening mode that dictates continuously without the key (see [docs/ALWAYS-LISTENING.md](docs/ALWAYS-LISTENING.md)).
- **Driven over MCP** — voices, language, engine, rate, and toggles are all tools your agent can call.
- **Speaker diarization & speaker-lock** — label enrolled voices and restrict dictation to yours.

## Models & runtimes

- **TTS** — Kokoro-82M, or the OS system voice.
- **STT** — a built-in streaming recognizer (NeMo FastConformer 80ms across platforms; Parakeet TDT 0.6b v2 via Core ML on macOS), the macOS system recognizer, or Claude Code's dictation.
- **Diarization / speaker-lock** — pyannote segmentation + WeSpeaker embeddings, with SepFormer separation.

Each model runs on the fastest backend available, picked by the `provider` ladder (`["ane", "ort_cuda", "ort_cpu"]`):

| Platform | Backend |
|---|---|
| macOS (Apple Silicon) | Apple Neural Engine via FluidAudio Core ML → ONNX Runtime CPU |
| Windows | ONNX Runtime CUDA (NVIDIA GPU) → CPU |
| Linux | ONNX Runtime CPU |

## MCP tools

`speak` · `stop_speak` · `listen` · `status` · `list_voices` · `set_config` · `wire` · `diarize` · `speakers`

## License

[MIT](LICENSE). Third-party model and dependency attributions are in [NOTICE.md](NOTICE.md).
