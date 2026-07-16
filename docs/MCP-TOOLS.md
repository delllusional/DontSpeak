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

Record the microphone and return a transcript. Stops after speech ends or the time limit is
reached.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Maximum recording time in seconds. Default 30. |

## stop_speech

Stop queued and active speech for this terminal session. If no session identity is available,
stop all speech. Active audio fades out. No parameters.

## mute

Set global audio mute. While muted, speech drains silently and earcons are suppressed. The
setting lasts until changed or the DontSpeak engine restarts.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | Set true to mute or false to unmute. |

## get_status

Get speech configuration and runtime state.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Include model, dictation, and runtime statistics. Default false. |

## list_voices

List available English voices by engine and language.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Engine to inspect. Defaults to the configured speech engine, or the built-in engine when speech is off. |

## diarize

Record the microphone and identify who spoke when. Requires enabled diarization and is available
only on macOS.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Recording time in seconds. Default 10. |

## manage_speakers

List, enroll, or remove speaker voiceprints used by diarize. Re-enrolling a name replaces it.
Available only on macOS.

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
| `tts_engine` | `built_in` \| `system` \| `off` | Omit = keep ladder; unsupported rejected |
| `tts_built_in_voices` | string[] | Ordered pool; first = default |
| `tts_system_voice` | string | Empty = OS default |
| `tts_rate` | 0.5–2.0 | 1.0 = normal |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | `shorts` \| `digests`[] | Default both; `[]` = off |
| `greet_on_open` | bool | Default on |
| `input_clears` | `current` \| `other`[] | Default `["current"]` |
| `pause_in_background` | bool | Default false |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply_sound` | string | OS sound name/path; empty = off |
| `earcon_needs_input_sound` | string | Default off |

**STT**

| Param | Type | Description |
|---|---|---|
| `caps_enabled` | bool | Default on; still silences speech when dictation off |
| `stt_engine` | `built_in` \| `system` \| `claude_code` \| `off` | Unsupported rejected |
| `capture_gain` | `"auto"` or 0.5–20.0 | Default `"auto"` |
| `double_tap_submits` | bool | Default false (swaps double/single) |
| `paste_submit_delay_ms` | 0–5000 | Default 100 |
| `full_duplex` | bool | Default false; built-in STT+TTS only |

**Compute**

| Param | Type | Description |
|---|---|---|
| `provider` | `ane` \| `cuda` \| `coreml` \| `cpu`[] | Default `["ane","cuda","cpu"]` |

**Diarization** (hidden #77)

| Param | Type | Description |
|---|---|---|
| `diarizer_provider` | `apple_native`[] | `[]` = off (default) |
| `clustering_threshold` | 0.5–0.9 | Default 0.7 |
| `speaker_threshold` | 0.0–1.0 | Default 0.65 |
| `stt_speaker_lock` | bool | Built-in STT only; default off |

**UI**

| Param | Type | Description |
|---|---|---|
| `tray_indicator` | `stt` \| `tts` \| `stt_animated` \| `tts_animated`[] | Default `["stt","tts_animated"]` |
