# MCP tools

DontSpeak exposes 7 tools over MCP by default (`speak`, `listen`, `stop_speech`, `mute`,
`get_status`, `list_voices`, `set_config`), in the order below — the
same order the SwiftUI Tools window lists them in. Names, descriptions, and parameters
are generated from one source, `ds-tools` (`rust/crates/ds-tools/src/lib.rs` +
`descriptions.rs`). A parity test pins tool names and descriptions; if you
change the catalog, update this file too.

Client wiring is no longer an MCP tool: the engine keeps each AI client wired
automatically (at boot and on config change), converging to `config.toml`'s `exclude_clients`
(absent ⇒ all supported clients). Wire by hand with `dontspeak wire <client>` /
`dontspeak wire --reconcile`.

`diarize` and `manage_speakers` (documented below for completeness), plus `set_config`'s
4 diarization params (`diarizer_provider`, `clustering_threshold`, `speaker_threshold`,
`stt_speaker_lock`), are implemented but hidden from user-facing surfaces pending the
validation tracked in issue #77 — see `ds_tools::DIARIZATION_ENABLED`.

Every tool explicitly declares all four MCP behavioral annotations. All tools operate only on
the local DontSpeak installation (`openWorldHint=false`). `get_status`, `list_voices`, `listen`,
and `diarize` are read-only; `stop_speech`, `mute`, and `set_config` are idempotent. Tools that
discard queued work, replace stored state, or remove data are marked destructive.

`get_status` and `list_voices` advertise output schemas and return their data in
`structuredContent`; their required text content contains the same JSON. Action tools return a
short text result.

The stdio server accepts one JSON-RPC message per line and caps each line at 1 MiB. It
validates every `tools/call` against the same schema shown below before dispatch. At most
8 tool calls run concurrently; additional calls receive a protocol error, providing
explicit backpressure. MCP cancellation notifications stop an active `listen` and suppress
the cancelled request's response.

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

List, enroll, or remove speaker voiceprints used by `diarize`. Re-enrolling a name replaces it.
Available only on macOS.

| Param | Type | Required | Description |
|---|---|---|---|
| `action` | enum: `list`, `enroll`, `forget` | yes | Operation to perform. |
| `name` | string | no | Speaker name. Required for `enroll` and `forget`. |
| `seconds` | integer 1–60 | no | Enrollment recording time in seconds. Default 15. |

## set_config

Update one or more persistent settings atomically and reload them.

**TTS output**

| Param | Type | Description |
|---|---|---|
| `tts_engine` | enum: `built_in`, `system`, `off` | Speech engine: `built_in`, `system`, or `off`. Omit to keep the automatic preference. Unsupported engines are rejected. |
| `tts_built_in_voices` | array of strings | Ordered built-in voice IDs. The first is the default; remaining voices form the per-terminal pool. |
| `tts_system_voice` | string | Voice name for the system engine; empty = OS default. System engine only. |
| `tts_rate` | number 0.5–2.0 | Speech rate. 1.0 = normal. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | Reply types to narrate. Default both: `digests` speaks long-reply summaries; `shorts` speaks short replies in full. `[]` disables narration. |
| `greet_on_open` | boolean | Greet each new terminal aloud in its pool voice. Default on. |
| `input_clears` | array of `current`, `other` | Speech queues cleared when input is submitted: `current` for this terminal and `other` for all others, including global audio. Default `["current"]`; `[]` clears none. |
| `pause_in_background` | boolean | Pause speech while no terminal is frontmost; resume on focus. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply_sound` | string | Reply-complete sound name or path within an OS sound folder. Default: OS chime; empty = off. |
| `earcon_needs_input_sound` | string | Needs-input cue: system-sound name or absolute path. Default off. |

**STT / dictation**

| Param | Type | Description |
|---|---|---|
| `caps_enabled` | boolean | Enable Caps Lock tap-to-talk and speech cancellation. Default on. Caps still silences speech when dictation is off. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation engine: `built_in`, `system`, `claude_code`, or `off`. Omit to keep the automatic preference. Unsupported or unauthorized engines are rejected. |
| `capture_gain` | `"auto"` or number 0.5–20.0 | Mic gain before recognition. Default `"auto"`. |
| `double_tap_submits` | boolean | Whether a double tap submits and a single tap only inserts. Default false, which swaps those actions. |
| `paste_submit_delay_ms` | integer 0–5000 | Delay between paste and submit, in milliseconds. Default 100; 0 submits immediately. |
| `full_duplex` | boolean | Keep the mic open while replies play, using platform echo cancellation, instead of closing it during speech. Default false; only takes effect with built-in dictation and built-in speech output. |

**Compute backend**

| Param | Type | Description |
|---|---|---|
| `provider` | array of `ane`, `cuda`, `coreml`, `cpu` | Compute providers in preference order; the first usable provider wins. Default `["ane","cuda","cpu"]`. |

**Diarization**

| Param | Type | Description |
|---|---|---|
| `diarizer_provider` | array of `apple_native` | Diarization runtime + on/off switch: `["apple_native"]` = on, `[]` = off (default). macOS-only. |
| `clustering_threshold` | number 0.5–0.9 | Diarization sensitivity; lower values split more speakers. Default 0.7. |
| `speaker_threshold` | number 0.0–1.0 | Minimum voiceprint match score; higher values are stricter. Default 0.65. |
| `stt_speaker_lock` | boolean | Transcribe only enrolled speaker(s), dropping others — needs diarization on and ≥1 enrolled voice. Built-in dictation only. Default off. |

**UI**

| Param | Type | Description |
|---|---|---|
| `tray_indicator` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Speech states that color or animate the tray icon. Default `["stt","tts_animated"]`; `[]` disables the indicator. |
