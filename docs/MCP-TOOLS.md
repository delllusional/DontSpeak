# MCP tools

DontSpeak exposes 8 tools over MCP by default (`speak`, `listen`, `stop_speech`, `mute`,
`get_status`, `list_voices`, `set_config`, `setup_integration`), in the order below — the
same order the SwiftUI Tools window lists them in. Names, descriptions, and parameters
are generated from one source, `ds-tools` (`rust/crates/ds-tools/src/lib.rs` +
`descriptions.rs`), so this table can't drift from what Claude actually sees; if you
change the catalog, update this file too.

`diarize` and `manage_speakers` (documented below for completeness), plus `set_config`'s
4 diarization params (`diarizer_provider`, `clustering_threshold`, `speaker_threshold`,
`stt_speaker_lock`), are implemented but currently hidden from every user-facing surface
pending more testing — see `ds_tools::DIARIZATION_ENABLED`.

## speak

Speak text aloud.

| Param | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | The text to speak. |
| `voice` | string | no | Voice id (default: the configured voice). |
| `rate` | number 0.5–2.0 | no | Speed multiplier (default: from config). |

## listen

Open the mic and return the transcribed text. Auto-stops when the speaker stops
talking — no key press needed.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Hard upper bound in seconds (default 30); the mic normally stops on end-of-speech first. |

## stop_speech

Stop any in-progress speech immediately. No parameters.

## mute

Silence or restore ALL spoken output — the app's global mute. Muted replies and
narration still queue but play silently. Persists, unlike the one-shot `stop_speech`;
`get_status` shows the muted state.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | `true` = mute; `false` = unmute. |

## get_status

Report current state: engine, active voice, default rate, whether speech is playing,
queue length, paused, muted. Pass `detail=true` for per-engine model status,
dictation state, and stats.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Per-engine model status, dictation state, and stats. Default false. |

## list_voices

List available voices, grouped by language (English only in this build). Optional
engine filter; default: the configured engine.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Which engine's voices to list. Default: the configured engine. |

## diarize

Record the mic and return who spoke when: per-speaker time spans (seconds), labelled
with an enrolled name when matched. Needs diarization on (`set_config
diarizer_provider`). macOS-only.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Seconds to record (default 10). |

## manage_speakers

Manage the enrolled voiceprints `diarize` uses to name speakers. `list`: show enrolled
names. `enroll`: record the mic and learn the name (re-enrolling replaces it). `forget`:
remove the name. macOS-only.

| Param | Type | Required | Description |
|---|---|---|---|
| `action` | enum: `list`, `enroll`, `forget` | yes | What to do. |
| `name` | string | no | Speaker name — required for `enroll` and `forget`. |
| `seconds` | integer 1–60 | no | Seconds to record for `enroll` (default 15; longer/varied = stronger). Ignored otherwise. |

## set_config

Update persistent settings. All fields optional; provide at least one. Validated,
applied together, then hot-reloaded. To change the voice, set `tts_built_in_voices`
or `tts_system_voice`.

**TTS output**

| Param | Type | Description |
|---|---|---|
| `tts_engine` | enum: `built_in`, `system`, `off` | Spoken-reply engine: `built_in` (on-device) or `system` (OS voice) to force exactly that engine, or `off` to turn spoken replies off. Omit to keep the automatic preference (config-file only). Rejected if the engine isn't usable on this platform/build. |
| `tts_built_in_voices` | array of strings | Ordered voice ids for the built-in engine — first is the default, the rest a per-terminal pool. English ids only in this build. Built-in only. |
| `tts_system_voice` | string | Voice name for the system engine; empty = OS default. System engine only. |
| `tts_rate` | number 0.5–2.0 | Speech rate (1.0 = normal). Both engines. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | What to narrate aloud (default both). `digests`: speak the spoken digest of long replies. `shorts`: also speak short replies in full. `[]` = nothing. |
| `greet_on_open` | boolean | Greet each new terminal aloud in its pool voice. Default on. |
| `input_clears` | array of `current`, `other` | Which sessions a submit (typed + Enter, or a voice/dictation submit) clears pending speech for: `current` (the submitting window) and/or `other` (every other window, including untagged/global audio). Default `["current"]`; `[]` = never. |
| `pause_in_background` | boolean | Pause speech while no terminal is frontmost; resume on focus. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply_sound` | string | Reply-done chime: system-sound name or absolute path. Default: OS chime; empty = off. |
| `earcon_needs_input_sound` | string | Needs-input cue: system-sound name or absolute path. Default off. |

**STT / dictation**

| Param | Type | Description |
|---|---|---|
| `caps_enabled` | boolean | Enable the Caps Lock handler — tap-to-talk dictation plus silence/cancel. Default on. With dictation off (`stt_engine="off"`), Caps still silences the voice. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation engine: `built_in` (on-device), `system` (OS recognizer, macOS only), or `claude_code` (Claude Code's voice key) to force exactly that engine, or `off` to turn dictation off. Omit to keep the automatic preference (config-file only). Rejected if the engine isn't usable on this platform/build; `system` is also checked for on-device availability/authorization when set. |
| `capture_gain` | `"auto"` or number 0.5–20.0 | Mic gain before recognition. Default `"auto"`. |
| `double_tap_submits` | boolean | Default false: a single tap submits (paste + Return), a fast double tap only inserts. `true` swaps them. |
| `full_duplex` | boolean | Keep the mic open while replies play, using platform echo cancellation, instead of closing it during speech. Default false; only takes effect with built-in dictation and built-in speech output. |

**Compute backend**

| Param | Type | Description |
|---|---|---|
| `provider` | array of `ane`, `cuda`, `coreml`, `cpu` | Compute-backend ladder for speech output and recognition (first usable wins). Default `["ane","cuda","cpu"]`. |

**Diarization**

| Param | Type | Description |
|---|---|---|
| `diarizer_provider` | array of `apple_native` | Diarization runtime + on/off switch: `["apple_native"]` = on, `[]` = off (default). macOS-only. |
| `clustering_threshold` | number 0.5–0.9 | Diarization sensitivity (default 0.7); lower splits more speakers apart. |
| `speaker_threshold` | number 0.0–1.0 | Match cutoff (default 0.65) for labelling a span with an enrolled name; higher = stricter. |
| `stt_speaker_lock` | boolean | Transcribe only enrolled speaker(s), dropping others — needs diarization on and ≥1 enrolled voice. Built-in dictation only. Default off. |

**UI**

| Param | Type | Description |
|---|---|---|
| `tray_indicator` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Tray icon: which states color it and whether it pulses. Default `["stt","tts_animated"]`. `[]` = never color. |

## setup_integration

Write a config file, or register/remove a client integration — the same setup the
installer does. Targets: `"narration_spec"`, `"claude_code"`, `"claude_desktop"`,
`"codex"`, `"qwen_code"`. Additive and backed up; `enabled=false` removes only our entry.

| Param | Type | Required | Description |
|---|---|---|---|
| `target` | enum: `narration_spec`, `claude_code`, `claude_desktop`, `codex`, `qwen_code` | yes | What to wire: the narration spec, or a client integration. |
| `enabled` | boolean | yes | `true` = register; `false` = remove. |
