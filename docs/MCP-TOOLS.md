# MCP tools

Nine primary tools: `speak`, `listen`, `stop`, `mute`, `status`,
`usage`, `voices`, `models`, `set_config` — Tools window order. Source: `ds-tools`;
parity test pins names/descriptions. `usage` is gated by config `agents`
(off by default): while the gate is off it is hidden from the catalog and
calls are rejected as unknown.

Client wiring is not an MCP tool — engine converges to `exclude_clients` at boot.
Manual: `dontspeak wire <client>` / `wire --reconcile`.

`diarize`, `manage_speakers`, and four diarization `set_config` params are implemented
but hidden (issue #77, `DIARIZATION_ENABLED`).

Annotations. Read-only: `status`, `usage`, `voices`. Idempotent: `stop`, `mute`,
`status`, `usage`, `voices`, `models`, `set_config`. Open-world: `usage`. Only that one
reaches provider APIs; the rest are local only. `status`, `usage`, `voices`, and
`models`: `structuredContent` + same JSON in text. Unlike `status` and `usage`,
`models` reaches the engine and starts the host app if it is not running. Stdio: 1
JSON-RPC line ≤1 MiB; max 8 concurrent; cancel stops `listen`.

## speak

Queue text for spoken playback.

| Param | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | Text to speak. |
| `tts_args` | object | no | Per-target voice/language/params for this utterance. See voices and models. |

Only the target active at playback is applied. Flat `voice`/`language`/`rate` args are not accepted.

```json
{
  "text": "Guten Morgen.",
  "tts_args": {
    "system": { "voice": "Anna", "language": "de", "rate": 1.1 },
    "qwen": { "voice": "ryan", "language": "de", "repetition_penalty": 1.2 }
  }
}
```

## listen

Record mic to transcript.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Max recording seconds. Default 30. |

## stop

Stop this MCP connection's speech. No parameters. The MCP server assigns every
connection a non-empty queue identity, so `stop` never degrades to a global cancel.

## mute

Global mute until changed or engine restart.

| Param | Type | Required | Description |
|---|---|---|---|
| `on` | boolean | yes | True to mute, false to unmute. |

## status

Speech config and runtime state.

| Param | Type | Required | Description |
|---|---|---|---|
| `detail` | boolean | no | Include model, dictation, and runtime stats. |
| `since` | integer ≥0 | no | Long-poll until status sequence changes from this value. |
| `timeout_ms` | integer 1–60000 | no | Long-poll max wait ms when since is set. Default 30000. |

With `detail=true`, nested model lifecycle/stats land under `status` (not `models`).
Pass `seq` back as `since` to long-poll. `timeout_ms` is only valid with `since`.
Download ETA: `(done_bytes - start_bytes) / elapsed_seconds` once elapsed is nonzero.

## usage

Coding-agent subscription usage.

| Param | Type | Required | Description |
|---|---|---|---|
| `refresh` | boolean | no | Bypass 60s cache. Default false. |

Gated by config `agents` (off by default). With the app running, usage goes through
the in-process engine (macOS keychain identity). With the app stopped, local
cache/provider fallback.

## voices

List languages and voices.

| Param | Type | Required | Description |
|---|---|---|---|
| `tts_engine` | enum: `built_in`, `system` | no | Engine to inspect. |
| `tts_model` | enum: `kokoro`, `chatterbox`, `qwen`, `omnivoice` | no | Built-in model to inspect. |
| `language` | string | no | Language to inspect. |

`language` filters this query only; synthesis language is detected per utterance.
Per-model capabilities (languages, providers, params) live in `models`.

## models

Built-in models: capabilities, disk usage, and removal.

| Param | Type | Required | Description |
|---|---|---|---|
| `remove` | enum: `kokoro`, `chatterbox`, `qwen`, `omnivoice`, `parakeet` | no | Model to delete from the cache. The active TTS/STT model is refused. |

Without `remove` this only reads. Output: `model_dir`, `total_bytes`, and one `assets` row
per model id — `kind` (`tts`/`stt`/`frontend`/`runtime`), `installed`, `bytes`, `active`,
`removable`, `reason` (`active` \| `shared` \| null), `variants[]` (one per
`DownloadTarget` this host supports, e.g. `kokoro_model` + `kokoro_mlx`), and
`capabilities` (languages/providers/params for TTS models, null otherwise). A successful
`remove` also returns `removed: { id, bytes }`.

`installed` = the files are present; the engine additionally verifies checksums when it
loads. Sizes are logical bytes (like `du --apparent-size`), symlinks are not followed, and
`total_bytes` covers the whole cache — subtracting the rows shows unattributed leftovers.

Removal takes every on-disk variant of one model (ONNX and MLX). It is refused when:

- the model is the active TTS model —
  ``models: `kokoro` is the active TTS model — switch with set_config tts_model first``
- the model is the active STT model —
  ``models: `parakeet` is the active STT model — switch with set_config stt_engine first``
- one of its downloads is in flight —
  ``models: `chatterbox` is downloading right now — try again when it finishes``
- the id is a shared asset (`onnxruntime`, `kokoro_frontend`, `cuda`) —
  ``models: `onnxruntime` is shared by every model and cannot be removed``

Shared assets are listed with their sizes but never removed; `removable` is also false
while a download is in flight, where `reason` stays null (live download state belongs to
`status`). Removing a model that was never downloaded succeeds and reclaims 0 bytes; it
may create empty cache directories and lock sidecars. Removal never changes the selection
and never triggers a re-download.

Partial failure is surfaced, not repaired: the response is an MCP error
(``models: could not remove `<id>`: <io error>``) with no `removed` block, the row then
reads `installed:false` with non-zero `bytes`, and re-running `models remove <id>` is the
recovery. On Windows a model whose files the running engine still has open can fail to
remove — stop speaking/listening or restart the app, then retry.

## diarize

Record and label speakers. macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `seconds` | integer 1–60 | no | Recording seconds. Default 10. |

## manage_speakers

List, enroll, or forget diarize voiceprints. macOS only.

| Param | Type | Required | Description |
|---|---|---|---|
| `action` | enum: `list`, `enroll`, `forget` | yes | list \| enroll \| forget. |
| `name` | string | no | Speaker name for enroll/forget. |
| `seconds` | integer 1–60 | no | Enrollment seconds. Default 15. |

## set_config

Update and reload settings.

**TTS**

| Param | Type | Description |
|---|---|---|
| `tts_engine` | enum: `built_in`, `system`, `off` | Speech engine. Omit to keep the automatic preference. |
| `tts_model` | enum: `kokoro`, `chatterbox`, `qwen`, `omnivoice` | Built-in model. |
| `tts_voices` | object | Voice pools by target. `system: []` uses the OS default. |
| `tts_params` | object | Param objects by target. rate default 1.0 (system/kokoro only). `{}` resets. |

**Narration**

| Param | Type | Description |
|---|---|---|
| `narrate` | array of `shorts`, `digests` | Narration modes. Default both. |
| `greet` | boolean | Greet new terminals. Default on. |
| `clear_on_input` | array of `current`, `other` | Queues to clear on submit. `current` = the submitting terminal, `other` = everything else (incl. untagged). Default ["current"]. |
| `pause_bg` | boolean | Pause speech when no terminal is frontmost. Default false. |

**Earcons**

| Param | Type | Description |
|---|---|---|
| `earcon_reply` | string | Reply-done sound. Default: OS chime; empty = off. |
| `earcon_input` | string | Needs-input sound. Default off. |

**STT**

| Param | Type | Description |
|---|---|---|
| `caps` | boolean | Caps Lock PTT and speech cancel. Default on. |
| `stt_engine` | enum: `built_in`, `system`, `claude_code`, `off` | Dictation engine. Omit to keep the automatic preference. |
| `capture_gain` | `"auto"` or number 0.5–20 | Mic gain: "auto" (default) or 0.5–20.0. |
| `double_tap_submit` | boolean | Double-tap submits. Default false. |
| `paste_delay_ms` | integer 0–5000 | Paste→submit delay ms. Default 100. |
| `full_duplex` | boolean | Mic open during replies. Default false. |
| `provider` | array of `mlx`, `cuda`, `coreml`, `cpu` | Compute provider order. Default ["mlx","cuda","cpu"]. |

**Diarization** (hidden while `DIARIZATION_ENABLED` is false)

| Param | Type | Description |
|---|---|---|
| `diarizer` | array of `mlx` | Diarization providers. [] = off (default). |
| `activity_threshold` | number 0.1–0.9 | Speaker-activity cutoff. Default 0.5. |
| `match_threshold` | number 0.0–1.0 | Voiceprint match threshold. Default 0.65. |
| `speaker_lock` | boolean | Only enrolled speakers. Default off. |

**Tray**

| Param | Type | Description |
|---|---|---|
| `tray` | array of `stt`, `tts`, `stt_animated`, `tts_animated` | Tray speech states. Default ["stt","tts_animated"]. |

**Agents**

| Param | Type | Description |
|---|---|---|
| `agents` | boolean | Agents tab and usage tool. Off by default. |
