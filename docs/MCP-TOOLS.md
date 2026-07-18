# MCP tools

Eight tools by default: `speak`, `listen`, `stop_speech`, `mute`, `get_status`,
`get_usage`, `list_voices`, `set_config` — Tools window order. Source: `ds-tools`
(`lib.rs` + `descriptions.rs`); parity test pins names/descriptions.

Client wiring is not an MCP tool — engine converges to `exclude_clients` at boot.
Manual: `dontspeak wire <client>` / `wire --reconcile`.

`diarize`, `manage_speakers`, and four diarization `set_config` params are implemented
but hidden (issue #77, `DIARIZATION_ENABLED`).

Annotations: `get_usage` queries provider APIs (`openWorldHint=true`); the rest are local
only. Read-only: `get_status`, `get_usage`, `list_voices`, `listen`, `diarize`. Idempotent:
`stop_speech`, `mute`, `set_config`. Destructive when discarding queue/state. `get_status`,
`get_usage`, and `list_voices`: `structuredContent` + same JSON in text. Stdio: 1 JSON-RPC
line ≤1 MiB; max 8 concurrent; cancel stops `listen`.

## speak

Queue text for spoken playback.

| Param | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | Text to speak. |
| `voice` | string | no | Voice ID. Defaults to the configured voice. |
| `rate` | number 0.5–2.0 | no | Playback speed. Defaults to the configured rate. |

## listen

Record the mic → transcript. Stops on end-of-speech or time limit.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Max recording seconds. Default 30. |

## stop_speech

Stop this session's speech, or all speech if no session. Fades out. No parameters.

## mute

Global mute until changed or engine restart. Muted speech drains silently; earcons off.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | True to mute, false to unmute. |

## get_status

Speech config and runtime state.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Include model, dictation, and runtime stats. Default false. |

## get_usage

Coding-agent subscription usage shown in the Usage tab.

| Param | Type | Required | Description |
|---|---|---|---|
| `force_refresh` | boolean | no | Bypass the 60-second cache and query providers. Default false. |

## list_voices

List English voices by engine and language.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Engine to inspect. Defaults to configured speech engine, or built-in when speech is off. |

## diarize

Record mic and label who spoke when. Needs diarization; macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Recording seconds. Default 10. |

## manage_speakers

List, enroll, or remove diarize voiceprints. Re-enroll replaces. macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `action` | enum: `list`, `enroll`, `forget` | yes | list \| enroll \| forget. |
| `name` | string | no | Speaker name (required for enroll/forget). |
| `seconds` | integer 1–60 | no | Enrollment seconds. Default 15. |

## set_config

Atomically update and reload persistent settings.

**TTS**

| Param | Type | Description |
|---|---|---|
| `tts_engine` | enum: `built_in`, `system`, `off` | Speech: "built_in", "system", or "off". Omit to keep the automatic preference. Unsupported engines are rejected. |
| `tts_built_in_voices` | array of strings | Ordered built-in voice IDs. First = default; rest = per-agent pool. |
| `tts_system_voice` | string | System voice name; empty = OS default. System engine only. |
| `tts_rate` | number 0.5–2.0 | Speech rate. 1.0 = normal. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | What to narrate. Default both: "digests" = long-reply summaries; "shorts" = short replies whole. [] off. |
| `greet_on_open` | boolean | Greet each new terminal in its agent's pool voice. Default on. |
| `input_clears` | array of `current`, `other` | Queues to clear on submit: "current" this terminal, "other" the rest (incl. global). Default ["current"]; [] none. |
| `pause_in_background` | boolean | Pause speech when no terminal is frontmost. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply_sound` | string | Reply-done sound name/path. Default: OS chime; empty = off. |
| `earcon_needs_input_sound` | string | Needs-input sound name/path. Default off. |

**STT**

| Param | Type | Description |
|---|---|---|
| `caps_enabled` | boolean | Caps Lock PTT and speech cancel. Default on. Still silences speech when dictation is off. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation: "built_in", "system", "claude_code", or "off". Omit to keep the automatic preference. Unsupported/unauthorized rejected. |
| `capture_gain` | `"auto"` or number 0.5–20.0 | Mic gain: "auto" (default) or 0.5–20.0 fixed. |
| `double_tap_submits` | boolean | Double-tap submits; single-tap inserts only. Default false (swaps those). |
| `paste_submit_delay_ms` | integer 0–5000 | Paste→submit delay (ms). Default 100; 0 = immediate. |
| `full_duplex` | boolean | Mic open during replies (platform AEC). Default false; built-in STT+TTS only. |

**Compute**

| Param | Type | Description |
|---|---|---|
| `provider` | array of `ane`, `cuda`, `coreml`, `cpu` | Compute provider preference order. Default ["ane","cuda","cpu"]. |

**Diarization** (hidden #77)

| Param | Type | Description |
|---|---|---|
| `diarizer_provider` | array of `apple_native` | Diarization: ["apple_native"] on, [] = off (default). macOS only. |
| `clustering_threshold` | number 0.5–0.9 | Diarization sensitivity; lower → more speakers. Default 0.7. |
| `speaker_threshold` | number 0.0–1.0 | Min voiceprint match; higher → stricter. Default 0.65. |
| `stt_speaker_lock` | boolean | Transcribe enrolled speakers only. Needs diarization + ≥1 voice. Built-in STT only. Default off. |

**UI**

| Param | Type | Description |
|---|---|---|
| `tray_indicator` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Tray icon speech states. Default ["stt","tts_animated"]; [] off. |
