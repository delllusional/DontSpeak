# MCP tools

Eight tools by default: `speak`, `listen`, `stop`, `mute`, `status`,
`usage`, `voices`, `set_config` — Tools window order. Source: `ds-tools`;
parity test pins names/descriptions.

Client wiring is not an MCP tool — engine converges to `exclude_clients` at boot.
Manual: `dontspeak wire <client>` / `wire --reconcile`.

`diarize`, `manage_speakers`, and four diarization `set_config` params are implemented
but hidden (issue #77, `DIARIZATION_ENABLED`).

Annotations: `usage` queries provider APIs (`openWorldHint=true`); the rest are local
only. Read-only: `status`, `usage`, `voices`, `listen`, `diarize`. Idempotent:
`stop`, `mute`, `set_config`. `status`, `usage`, and `voices`: `structuredContent` +
same JSON in text. Stdio: 1 JSON-RPC line ≤1 MiB; max 8 concurrent; cancel stops
`listen`.

## speak

Queue text for spoken playback.

| Param | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | Text to speak. |
| `voice` | string | no | Voice ID. Omit to use the calling agent's assigned voice. |
| `rate` | number 0.5–2.0 | no | Playback speed. Defaults to the configured rate. |

## listen

Record the mic → transcript. Stops on end-of-speech or time limit.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Max recording seconds. Default 30. |

## stop

Stop this session's speech, or all speech if no session. Fades out. No parameters.

## mute

Global mute until changed or engine restart. Muted speech drains silently; earcons off.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | True to mute, false to unmute. |

## status

Speech config and runtime state.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Include model, dictation, and runtime stats. Default false. |

With `detail=true`, nested model lifecycle/stats land under the `status` key
(not `models`). Engine tokens are config tokens (`built_in` / `system` / `off`).
Concise `model` is the configured built-in model and `language` is `auto` (both non-null by
schema even when the engine resolves to `system`/`off`); the `detail` `status` section uses resolved
`ModelStatus` semantics and nulls them when no built-in model is active.

## usage

Coding-agent subscription usage shown in the Agents tab.

| Param | Type | Required | Description |
|---|---|---|---|
| `refresh` | boolean | no | Bypass the 60-second cache and query providers. Default false. |

## voices

List selectable models, languages, and voices.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Engine to inspect. Defaults to configured speech engine, or built-in when speech is off. |
| `tts_model` | enum: `kokoro`, `chatterbox`, `qwen`, `omnivoice` | no | Built-in model to inspect. Defaults to the configured model. |
| `language` | string | no | Language to inspect. Defaults to the model's catalog default. |

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
| `tts_model` | enum: `kokoro`, `chatterbox`, `qwen`, `omnivoice` | Built-in model: "kokoro", "chatterbox", "qwen", or "omnivoice". |
| `tts_voices` | object | Voice arrays keyed by `system`, `kokoro`, `chatterbox`, `qwen`, or `omnivoice`. `system: []` uses the OS default; model pools must be non-empty. A pool may mix languages: each utterance is spoken by a pooled voice for its detected language, or by one of the model's own voices for that language when the pool has none. |
| `rate` | number 0.5–2.0 | Speech rate. 1.0 = normal. Model support is validated. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | What to narrate. Default both: "digests" = long-reply summaries; "shorts" = short replies whole. [] off. |
| `greet` | boolean | Greet each new terminal in its agent's pool voice. Default on. |
| `clear_on_input` | array of `current`, `other` | Queues to clear on submit: "current" this terminal, "other" the rest (incl. global). Default ["current"]; [] none. |
| `pause_bg` | boolean | Pause speech when no terminal is frontmost. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply` | string | Reply-done sound name/path. Default: OS chime; empty = off. |
| `earcon_input` | string | Needs-input sound name/path. Default off. |

**STT**

| Param | Type | Description |
|---|---|---|
| `caps` | boolean | Caps Lock PTT and speech cancel. Default on. Still silences speech when dictation is off. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation: "built_in", "system", "claude_code", or "off". Omit to keep the automatic preference. Unsupported/unauthorized rejected. |
| `capture_gain` | `"auto"` or number 0.5–20 | Mic gain: "auto" (default) or 0.5–20.0 fixed. |
| `double_tap_submit` | boolean | Double-tap submits; single-tap inserts only. Default false (swaps those). |
| `paste_delay_ms` | integer 0–5000 | Paste→submit delay (ms). Default 100; 0 = immediate. |
| `full_duplex` | boolean | Mic open during replies (platform AEC). Default false; built-in STT+TTS only. |
| `provider` | array of `mlx`, `cuda`, `coreml`, `cpu` | Compute provider preference order. Core ML is macOS TTS-only; default ["mlx","cuda","cpu"]. |

**Diarization** (hidden while `DIARIZATION_ENABLED` is false)

| Param | Type | Description |
|---|---|---|
| `diarizer` | array of `mlx` | Diarization: ["mlx"] on, [] = off (default). macOS only. |
| `activity_threshold` | number 0.1–0.9 | Sortformer speaker-activity cutoff; lower detects quieter speech. Default 0.5. |
| `match_threshold` | number 0.0–1.0 | Min voiceprint match; higher → stricter. Default 0.65. |
| `speaker_lock` | boolean | Transcribe enrolled speakers only. Needs diarization + ≥1 voice. Built-in STT only. Default off. |

**Tray**

| Param | Type | Description |
|---|---|---|
| `tray` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Tray icon speech states. Default ["stt","tts_animated"]; [] off. |
