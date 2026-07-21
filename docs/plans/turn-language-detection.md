# Plan: Turn-level TTS language detection

**Branch / worktree:** `feat/turn-language-detection` @ `C:/Users/usr/Develop/git/dontspeak-turn-lang`  
**Baseline:** current `main` @ `1c56ef2`  
**Effort:** high (multi-crate, wire + queue + narrate)  
**Status:** revised after plan-reviewer (Revise → address blockers below)

---

## Problem statement

Language detection for built-in TTS runs on the **spoken queue item only**.

Verified path:

1. Streaming (`ds-narrate`) admits each completed top-level blockquote separately.
2. Adapters enqueue via `SpeakNarration` / `TtsQueue::enqueue_narration` with **only that digest's text**.
3. `dontspeakd::ttsq::TtsQueue::play_speech` calls `ds_tts::detect_language(text, model)` on that digest alone.

Short digests (~27–36 chars) are misclassified by whatlang (FR/PT false positives), which selects Kokoro's eSpeak frontend + a non-English voice path and logs voice-mismatch WARNs.

Docs currently say schedule should "detect and pin one ISO language per utterance".

Product intent: language should reflect the **whole assistant turn/message**. Speaking stays **per utterance** (mid-turn streaming must keep working). Frontend chunking after detection stays per utterance under the **pinned** language.

Non-goals:

- Do not refuse utterances for language (keep clamp / `en` fallback).
- Do not buffer speech until `is_final`.
- No FFI / UI strings.
- No new detection crates (`whatlang` already in `ds-tts`).
- Short-only turns with normalized prose **&lt; 64 chars** remain best-effort (may still false-positive); document, do not treat as regression.

---

## Chosen approach

**Snapshot full reconstructed message-so-far at admission (capped); attempt solid detect+pin in the engine at enqueue; speak the digest under the pinned language at play.**

### Behavior

| Route | Spoken text | Detection corpus | Pin scope |
| --- | --- | --- | --- |
| Streaming `SpeakNarration` | one completed blockquote / short | Accum **cumulative so-far** at selection time (capped) | `(session, message_key)` once solid |
| Non-streaming Stop | each blockquote from `stop_utterances` | full `last_assistant_message` (capped) | unique per-reply `message_key` shared by all lines of that Stop |
| MCP `Speak` / `enqueue` | caller string | that string **is** the turn text | no pin map (one-shot) |
| `ds-helper` oneshot / synth-check | phrase | phrase (dev / standalone) | unchanged |

### Pin policy (API-true)

`ds_tts::detect_language` always returns `String` (no-evidence → `"en"`). There is no `Option` and no `is_final` on the wire. Rules:

1. **Corpus** for an item = non-empty `detection_text` if present, else spoken `text`. Always truncated to **`MAX_SPEAK_BYTES` (10 KiB) at enqueue/wire**, same as spoken text. Truncation is prefix-based on bytes (match existing speak limit semantics).
2. **Pin map key** = `(session_string, message_key)` where `session_string` is the queue session tag as stored on the item (including sticky `grok-stop:…` when that is the admit session).
3. **Solid pin only when** normalized prose char count (`normalize_spoken_text(corpus).chars().count()`) is **≥ 64**. Below that threshold: **do not pin**; each play detects on that item's corpus (legacy short-turn behavior).
4. **First solid pin wins** for that map key. Later admissions with the same key do not overwrite.
5. **Attempt solid detect+pin at `enqueue_narration`** (admission), under the same locks used for accepted-id insert:
   - Lock order: `accepted_narrations` **before** `items` (existing); pin map lock is a **new** mutex taken only after `accepted_narrations` and **never while holding `items`** (or: take pins after items is dropped). Prefer: pins lock independent; never hold pins + items together. Config lock for model: take briefly for `tts_model` Copy, drop before detect.
6. **`play_speech` resolution**:
   1. If pin map has `(session, message_key)` → use it.
   2. Else detect on item corpus (or spoken text).
   3. Always `ds_tts::supported_language(&code, live_model)` before `speak_one` (model hot-switch clamp).
7. **Do not retain full `detection_text` after solid pin** if memory is a concern: once pinned at admit, subsequent queue items for the same key may set `detection_text: None` and rely on pin at play. First item(s) under threshold still need corpus until solid. Implementer may keep capped `detection_text` on the item for simplicity if tests stay green; hard cap at enqueue is mandatory either way.
8. **`detection_text` does not count toward `pending_bytes`.**
9. **Pending utterance id hash** stays `session|key|text|after` — **`detection_text` must not enter the id hash** (retry stability).

### Stop `message_key` (unique per reply)

- Streaming: `message_key = StreamBatch.key` / `pending.key` (already the message/item id).
- Stop path: `message_key = "stop:" + hex(sha256(full_assistant_body)[..16])` (or first 16 hex chars of the full hash). **Stable across multi-line Stop admits of the same body; unique across different replies.**
- Test: two Stops in one session with different languages must not share a pin.

### Pin map lifecycle

```rust
// Cap matches accepted narrations
const LANGUAGE_PINS_MAX: usize = 8192;
// HashMap<(session, message_key), iso> + FIFO order for eviction
language_pins: Mutex<LanguagePins>, // insert/evict like AcceptedNarrations
```

- **`forget_narration_session(session)`**: drop all pins whose session key is `session` **or** `grok-stop:{session}` (sticky sibling), matching how queue clearing treats sticky tags.
- Call sites that already call `forget_narration_session` (SessionEnd) get sticky pin cleanup free.
- Optional: also clear pins for a session on `clear_session` if that path should reset language mid-session — only if existing narration accepted-ids are also cleared there (today they are SessionEnd-scoped); **do not invent new clear timing**. Match accepted-narration forget only.

### Why this shape

- Full so-far text is available **at admit time** inside `Accum::feed` (`cumulative`).
- Engine remains sole owner of model-scoped whatlang.
- Wire change is **additive optional fields** only.
- Mid-turn streaming unchanged: one queue item per digest.
- Pin-at-admit uses fuller corpus sooner so the first played digest benefits when a later admission already solid-pinned (if order allows); when only short corpus exists at first play, short-turn fallback applies until solid.

### Rejected alternatives

| Alternative | Why not |
| --- | --- |
| Engine re-reads `narrate-display-*.json` at play time | Race with offset/parts clear after final; Stop/MCP have no witness; fragile |
| Detect in client, pass ISO `language` | Needs model allowlist + live model on hooks; model-switch race; more wire semantics |
| Buffer digests until `is_final` then detect | Breaks mid-turn streaming contract |
| Re-detect every utterance on full so-far, no pin | Fixes most false positives but allows mid-turn language flip-flops as corpus grows |
| Pin only, still detect on digest text | Does not fix short-digest FR/PT false positives |

**Reuse:** keep `ds_tts::detect_language` / `whatlang`; no new crates. Custom work is plumbing + pin map only.

---

## API / wire changes

### `ds-ipc::Request::SpeakNarration`

```rust
SpeakNarration {
    text: String,
    /// Full reconstructed turn text so far for language detection (capped by engine).
    /// Absent/empty → engine detects on `text` (legacy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detection_text: Option<String>,
    /// Message/item id for per-turn language pin. Absent → no pin map entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_key: Option<String>,
    session: Option<String>,
    narration_id: Option<String>,
    source: ClientSource,
}
```

**Do not** change `Speak`. Engine truncates `detection_text` to `MAX_SPEAK_BYTES` at enqueue (reject only if spoken `text` exceeds limit — existing rule).

Round-trip tests: with fields, without fields, legacy JSON decode.

### `ds-narrate`

```rust
pub struct SelectedUtterance {
    pub text: String,
    pub detection_text: String, // cumulative so-far at selection (may be long; callers/adapters may pre-cap)
}

pub struct NarrationUtterance {
    pub id: String,
    pub text: String,
    pub detection_text: String,
    pub message_key: String, // = batch.key / pending.key
}
```

- `Accum::feed` returns `Vec<SelectedUtterance>` (both fields).
- `DisplayStep.speak` becomes `Vec<SelectedUtterance>` (update mic-gate early returns + all accum/stream tests).
- `PendingUtterance`: **reuse existing `key` as message_key**; add only `detection_text: String` with `#[serde(default)]` for old state files.
- Id hash: **do not** include `detection_text`.
- `stop_utterances` stays `Vec<String>`; Stop **callers** attach full body + unique `message_key`.

### `dontspeakd::ttsq`

```rust
enum QueueAction {
    Speech {
        text: String,
        detection_text: Option<String>, // capped; optional after pin
        message_key: Option<String>,
        voice: Option<String>,
        rate: Option<f32>,
    },
    Earcon(...),
}
```

- **`enqueue(text, voice, rate, source, session)`** — detection fields always `None` (MCP Speak).
- **`enqueue_narration(text, source, session, narration_id, detection_text, message_key)`** — builds `QueueAction::Speech` with detection fields itself (does not go through a detection-blind helper that strips them). Today it calls `enqueue`; change to shared private `enqueue_action` with full Speech fields, or pass options through.
- **Every** `QueueAction::Speech { .. }` construction and test match site is signature churn — update all (grep `QueueAction::Speech` and `enqueue_narration(`).
- `play_speech` as in pin policy §6.
- `forget_narration_session` also drops sticky sibling pins.

### Call-site wiring

| Site | Change |
| --- | --- |
| `dontspeakd/src/ipc.rs` `SpeakNarration` | Pass new fields into `enqueue_narration` |
| `dontspeak/src/hook_narrate.rs` streaming admit | `detection_text` + `message_key` from utterance |
| `hook_narrate` Stop loop | full assistant body as `detection_text`; `message_key = stop:{sha256_prefix}` |
| `dontspeakd/src/codex_stream/mod.rs` | Plumb utterance fields |
| `dontspeakd/src/grok_stream/mod.rs` | Plumb utterance fields |
| `ds-helper` oneshot / synth-check | No change |
| FFI / hosts | No change |

### Deploy note

- Wire + hooks + `ds-narrate` → **CLI** rebuild.
- `ttsq` / ipc handler / stream supervisors → **engine/host** rebuild.
- Older CLI without fields still works (legacy detect-on-text).

---

## File-level change list

| File | Change |
| --- | --- |
| `rust/crates/ds-ipc/src/protocol.rs` | Optional fields + round-trip cases |
| `rust/crates/ds-narrate/src/accum.rs` | `SelectedUtterance` return |
| `rust/crates/ds-narrate/src/stream.rs` | utterance/pending/DisplayStep; tests |
| `rust/crates/ds-narrate/src/lib.rs` | Re-exports if needed |
| `rust/crates/dontspeakd/src/ttsq.rs` | Speech fields; pin map; admit pin; play resolve; forget sticky; tests |
| `rust/crates/dontspeakd/src/ipc.rs` | Plumb fields |
| `rust/crates/dontspeakd/src/codex_stream/mod.rs` | Plumb |
| `rust/crates/dontspeakd/src/grok_stream/mod.rs` | Plumb |
| `rust/crates/dontspeak/src/hook_narrate.rs` | Streaming + Stop context |
| `rust/crates/ds-tts/src/language.rs` | Module docs + English preamble vs short digest regression test |
| `docs/TTS-PIPELINE.md` | Per-turn detect/pin contract; short-turn caveat |
| `docs/STREAMING-NARRATION.md` | Detection context on admission |
| `docs/plans/turn-language-detection.md` | This plan |

---

## Test plan

All offline / tempdir.

### `ds-narrate`

1. First quote's `detection_text` includes preamble + quote cumulative.
2. Second quote's `detection_text` is fuller so-far (monotonic).
3. Pending retry preserves `detection_text` + `key` as message_key.
4. Old pending JSON without `detection_text` deserializes.
5. Pending id stable when only `detection_text` would change (hash excludes it).
6. Existing `stop_utterances` tests still pass.

### `ds-tts` language

1. English preamble + short false-friend digest: full corpus → `en` for Kokoro (regression).

### `ds-ipc`

1. Round-trip with/without new fields; legacy decode.

### `dontspeakd::ttsq`

1. English `detection_text` ≥ 64 + short spoken digests → pin English; play uses pin.
2. Second narration same `(session, message_key)` reuses pin.
3. Different `message_key` / session independent.
4. Prose &lt; 64 does not pin; two short items detect independently.
5. `forget_narration_session` clears real + `grok-stop:` sibling pins.
6. Pin map capped at 8192 (FIFO eviction like accepted ids) — smoke if cheap.
7. MCP `enqueue` path unchanged (`detection_text=None`).
8. `detection_text` over 10 KiB truncated at enqueue; does not fail unless spoken text over limit.
9. Two Stop-style keys with different bodies: no cross-pin.

### Adapters

- Codex/Grok suites compile; admit enqueues once.
- `hook_narrate` Stop: full body + stable per-reply key when request construction is asserted.

Commands (from `rust/`):

```sh
cargo test -p ds-narrate --locked
cargo test -p ds-tts --locked language
cargo test -p ds-ipc --locked
cargo test -p dontspeakd --locked ttsq
cargo test -p dontspeak --locked hook_narrate
cargo clippy --workspace --all-targets --locked -- -D warnings
```

---

## Migration / backward compatibility

| Client | Behavior |
| --- | --- |
| New CLI + new engine | Full so-far detection + pin |
| Old CLI + new engine | Fields absent → detect on digest |
| New CLI + old engine | Unknown fields ignored (no `deny_unknown_fields`) |
| On-disk pending without `detection_text` | Default `""` → use spoken text |
| MCP `Speak` | Unchanged |
| Short-only turns | Best-effort; documented |

No config migration.

---

## Implementation order

1. **`ds-narrate`**: `SelectedUtterance`; pending `detection_text`; tests.
2. **`ds-ipc`**: optional fields + round-trip.
3. **`ttsq`**: Speech fields; pin map (cap, sticky forget); pin-at-admit; play resolve + clamp; tests.
4. **Adapters**: ipc + codex/grok + hook_narrate; Stop unique key.
5. **Docs**: TTS-PIPELINE + STREAMING + language.rs docs.
6. **Workspace clippy/tests**.

Do not land on `main` unless asked. Open a PR against main after verification.

---

## Invariants checked

- Config untouched; no FFI; no new deps; no UI strings; offline tests; additive wire; Risk yes.

---

## Risk: yes

**Areas:** `ds-ipc` (additive `SpeakNarration` fields), engine language pin map (session lifecycle, sticky forget, cap). Not FFI, model pinning, OS permissions, licensing, or release/signing.

Reason: wire + cross-process queue contract; pin eviction and legacy decode must stay fail-closed.
