# MCP tools

Seven tools by default: `speak`, `listen`, `stop_speech`, `mute`, `get_status`,
`list_voices`, `set_config` — same order as the Tools window. Catalog source:
`ds-tools` (`lib.rs` + `descriptions.rs`); parity test pins names/descriptions —
update this file with the catalog.

Client wiring is **not** an MCP tool: engine converges to `exclude_clients` at boot.
Manual: `dontspeak wire <client>` / `wire --reconcile`.

`diarize`, `manage_speakers`, and four diarization `set_config` params are implemented
but hidden (issue #77, `DIARIZATION_ENABLED`).

Annotations: all local (`openWorldHint=false`). Read-only: `get_status`, `list_voices`,
`listen`, `diarize`. Idempotent: `stop_speech`, `mute`, `set_config`. Destructive when
discarding queue/state/data. `get_status` / `list_voices` use `structuredContent` + same
JSON in text. Stdio: one JSON-RPC line, 1 MiB cap, schema-validated calls, max 8 concurrent;
cancel stops `listen`.

## speak

Queue text for spoken playback.

| Param | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | Text to speak. |
| `voice` | string | no | Voice ID. Defaults to the configured voice. |
| `rate` | number 0.5–2.0 | no | Playback speed. Defaults to the configured rate. |

## listen

Record the mic and return a transcript. Stops on end-of-speech or the time limit.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Max recording time in seconds. Default 30. |

## stop_speech

Stop this session's speech (or all if no session). Active audio fades out. No parameters.

## mute

Set global mute until changed or the engine restarts. While muted, speech drains silently
and earcons are suppressed.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | True to mute, false to unmute. |

## get_status

Get speech configuration and runtime state.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Include model, dictation, and runtime stats. Default false. |

## list_voices

List available English voices by engine and language.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Engine to inspect. Defaults to the configured speech engine, or the built-in engine when speech is off. |

## diarize

Record the mic and identify who spoke when. Diarization on; macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Recording time in seconds. Default 10. |

## manage_speakers

List, enroll, or remove speaker voiceprints for diarize. Re-enroll replaces. macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `action` | enum: `list`, `enroll`, `forget` | yes | Operation to perform. |
| `name` | string | no | Speaker name. Required for enroll and forget. |
| `seconds` | integer 1–60 | no | Enrollment recording time in seconds. Default 15. |

## set_config

Update one or more persistent settings atomically and reload them.

**TTS**

| Param | Type | Description |
|---|---|---|
| `tts_engine` | enum: `built_in`, `system`, `off` | Speech engine: "built_in", "system", or "off". Omit to keep the automatic preference. Unsupported engines are rejected. |
| `tts_built_in_voices` | array of strings | Ordered built-in voice IDs. First is default; rest are the per-terminal pool. |
| `tts_system_voice` | string | System-engine voice name; empty = OS default. System engine only. |
| `tts_rate` | number 0.5–2.0 | Speech rate. 1.0 = normal. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | Reply types to narrate. Default both: "digests" = long-reply summaries; "shorts" = short replies in full. [] disables. |
| `greet_on_open` | boolean | Greet each new terminal in its pool voice. Default on. |
| `input_clears` | array of `current`, `other` | Queues cleared on submit: "current" this terminal, "other" all others (incl. global). Default ["current"]; [] clears none. |
| `pause_in_background` | boolean | Pause speech while no terminal is frontmost; resume on focus. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply_sound` | string | Reply-done sound name or path in an OS sound folder. Default: OS chime; empty = off. |
| `earcon_needs_input_sound` | string | Needs-input cue: system-sound name or path. Default off. |

**STT / dictation**

| Param | Type | Description |
|---|---|---|
| `caps_enabled` | boolean | Caps Lock tap-to-talk and speech cancel. Default on. Caps still silences speech when dictation is off. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation engine: "built_in", "system", "claude_code", or "off". Omit to keep the automatic preference. Unsupported or unauthorized engines are rejected. |
| `capture_gain` | `"auto"` or number 0.5–20.0 | Mic gain before recognition: "auto" (default) or a fixed 0.5–20.0 multiplier. |
| `double_tap_submits` | boolean | Double tap submits and single tap inserts only. Default false, which swaps those actions. |
| `paste_submit_delay_ms` | integer 0–5000 | Delay between paste and submit (ms). Default 100; 0 submits immediately. |
| `full_duplex` | boolean | Keep mic open during replies with platform echo cancellation. Default false; built-in dictation and speech only. |

**Compute backend**

| Param | Type | Description |
|---|---|---|
| `provider` | array of `ane`, `cuda`, `coreml`, `cpu` | Compute providers in preference order; first usable wins. Default ["ane","cuda","cpu"]. |

**Diarization** (hidden #77)

| Param | Type | Description |
|---|---|---|
| `diarizer_provider` | array of `apple_native` | Diarization on/off: ["apple_native"] = on, [] = off (default). macOS only. |
| `clustering_threshold` | number 0.5–0.9 | Diarization sensitivity; lower splits more speakers. Default 0.7. |
| `speaker_threshold` | number 0.0–1.0 | Min voiceprint match score; higher is stricter. Default 0.65. |
| `stt_speaker_lock` | boolean | Transcribe only enrolled speakers. Needs diarization on and ≥1 enrolled voice. Built-in dictation only. Default off. |

**UI**

| Param | Type | Description |
|---|---|---|
| `tray_indicator` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Speech states that color or animate the tray icon. Default ["stt","tts_animated"]; [] disables the indicator. |
