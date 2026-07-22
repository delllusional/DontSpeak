# Open-model voices: Chatterbox & OmniVoice

Plan for [#152](https://github.com/delllusional/DontSpeak/issues/152).  
**Base:** `main` @ `4e41098`  
**Risk:** yes (TTS inference contracts, MCP surface, on-disk user voice packages)

**Invariant: all voice-selection interaction is via MCP.**  
No host-only picker, no hand-editing `config.toml` as the supported way to choose a voice, no CLI-only sticky select. Agents and any future UI use the same MCP tools (`voices`, `manage_voices`, `set_config` for pool membership, `speak.tts_args.<target>.voice`).

---

## 1. Problem (verified on main)

| Model | Registry `voices` | Runtime behavior today |
| --- | --- | --- |
| **Chatterbox** | `["default"]` only | ORT `ensure_voice` hard-rejects anything else; loads pinned `default_voice.wav`. MLX forces `voice: nil`, `refAudio: nil`. |
| **OmniVoice** | 10 preset ids (`default`, `young_woman`, …) | Both paths resolve the id to a style instruct through the ONE `ds-tts` `OMNIVOICE_PRESETS` table: ORT builds the `<\|instruct_start\|>` prompt block; MLX resolves before the FFI call, `refAudio: nil`. `default` = automatic voice design. |
| **Qwen / Kokoro** | real catalogs | Allowlist matches the model. |

`set_config` validates non-Kokoro pools with `descriptor.voices.contains(...)`, so free-form Omni prompts and alternate Chatterbox refs are rejected before they reach the synth.

Agent sticky map on main is `(ClientSource, language) → voice id` in `ttsq` — **agent-scoped**, not terminal-session-scoped. SessionEnd barges FIFO but keeps the assignment.

---

## 2. Upstream truth (research)

### OmniVoice

Sources: [voice-design.md](https://github.com/k2-fsa/OmniVoice/blob/master/docs/voice-design.md), [issue #44](https://github.com/k2-fsa/OmniVoice/issues/44).

| Mode | Input | Sticky across turns? |
| --- | --- | --- |
| **Voice design** | `instruct` = **comma-separated attributes** (gender, age, pitch, style, EN accent, ZH dialect) — **not free-form prose** | **No.** Same instruct → different timbres across `generate()` calls. Within one long call, later chunks re-use the first chunk as ref. |
| **Voice cloning** | `ref_audio` / `VoiceClonePrompt` | **Yes** if the clone prompt artifact is reused. |
| **Auto** | text only | Random-ish each time. |

Official sticky recipe for design: **design once → save audio → create clone prompt → reuse clone**. Design is a *factory*, not a stable identity. Attribute set is closed; design quality is strongest on **en/zh**.

### Chatterbox Multilingual

Sources: [resemble-ai/chatterbox](https://github.com/resemble-ai/chatterbox).

- OSS voice = **reference audio** (prepared conditionals), not text design.
- ~6–10 s of audio used from the ref; recommend ≥10 s clean mono WAV.
- Multi-voice = **multiple refs / saved conditionals**, swap per request.
- Commercial “text design” is **not** in the open weights.

### Product language

Do **not** promise “type any description and keep that voice for the session”:

- OmniVoice design is **structured attributes**, and **not** session-sticky alone.
- True sticky description requires **materialize → clone** (Omni) or a **reference clip** (Chatterbox).
- “Pin” in DontSpeak means **pin a voice package id** (via MCP + agent assignment), not re-send raw instruct each turn.

---

## 3. What our ports can do today

| Capability | Chatterbox ORT | Chatterbox MLX | OmniVoice ORT | OmniVoice MLX |
| --- | --- | --- | --- | --- |
| Multiple named refs | Cache ready; only `"default"` admitted | Voice forced nil | N/A | N/A |
| User ref wav | Hard-coded `default_voice.wav` | `refAudio: nil` | Not wired | `refAudio: nil` |
| Design / instruct | N/A | N/A | Preset ids → instruct prompt block (`OMNIVOICE_PRESETS`) | Preset ids → instruct into `generate(... voice:)` (on-device check: #169) |
| Persist conditioning | In-process only | Default | None | None |

Expanding the config allowlist alone does nothing for OmniVoice ORT and little for MLX Chatterbox. Backend wiring is the real cost.

---

## 4. Product model

Stop treating every model as `Vec<catalog_id>`. Introduce **voice kinds** under one stable string id:

```text
VoiceSpec {
  id: String,                 // pool key, agent assignment, per-target speak voice
  kind: Catalog | Design | Clone,
  // Catalog: kokoro/qwen fixed id
  // Design:  structured instruct (Omni) + optional materialize-to-clone
  // Clone:   ref audio / on-disk conditioning artifact
}
```

| Model | Exposure | Package means |
| --- | --- | --- |
| **Kokoro / Qwen** | Unchanged catalog | Catalog members |
| **Chatterbox** | Named **clone packages** (`default` + user refs) | `id` → ref wav (+ optional cached encoder outputs) |
| **OmniVoice** | Phase A: **design presets** (attributes). Phase B: **clone packages** for sticky identity | Design → instruct; clone → artifact |

---

## 5. All voice selection via MCP (invariant)

**Every** interaction that chooses, creates, lists, pins, or materializes a voice goes through MCP tools. Config files and on-disk packages are storage only — not the interaction surface. A host UI, if added later, must call the same engine operations that back these tools (no parallel private API for picking voices).

### MCP tools

| Tool | Role |
| --- | --- |
| **`voices`** | Discover packages: `id`, `kind`, label, attributes/ref summary, `active` (in pool), optional “assigned to this agent”. Read-only. |
| **`manage_voices`** (new) | Lifecycle + pin: `list`, `create`, `update`, `delete`, **`select`**, `materialize`. |
| **`set_config`** | Global pool membership only: `tts_voices.chatterbox` / `omnivoice` = **registered package ids**. Does **not** create packages or accept raw instruct prose as pool entries. Invoked via MCP like any other config change. |
| **`speak`** | Optional `tts_args.<target>.voice` = package id for this utterance; omit → agent sticky from last `select` / assignment. |

Not supported as product paths for selection:

- Editing `config.toml` by hand as the way to “pick my voice”
- Host-only combo boxes that write config without going through the tool/engine path
- One-off CLI flags that bypass MCP for sticky assignment

### `manage_voices` actions

| Action | Purpose |
| --- | --- |
| `list` | Packages for a model (same shape as a `voices` subset). |
| `create` | Register a package: Chatterbox `ref` path/bytes; Omni `instruct` attributes **or** ref for clone. Returns stable `id`. |
| `update` | Change label / instruct / replace ref. |
| `delete` | Remove package; refuse if it is the sole active pool entry for the selected model. |
| **`select`** | Set the **calling agent’s** sticky voice to this package id (must be in pool or auto-admit to pool). This is the “pin for me” operation. |
| `materialize` | Omni design → bake short audio → clone package (sticky identity). Optional auto on first `select` of a design package. |

Wire sketch (illustrative):

```json
// create Omni design preset
{ "action": "create", "model": "omnivoice", "id": "british_male",
  "kind": "design", "instruct": "male, middle-aged, low pitch, british accent" }

// create Chatterbox clone from a local path the engine can read
{ "action": "create", "model": "chatterbox", "id": "narrator",
  "kind": "clone", "ref_path": "..." }

// pin for this agent (sticky until select again / pool change)
{ "action": "select", "model": "omnivoice", "id": "british_male" }

// make design sticky across turns
{ "action": "materialize", "model": "omnivoice", "id": "british_male" }
```

**Why not only `set_config`:** package definitions (wav bytes, attribute strings, kind) are not a good fit for nested pool arrays; selection/pin is agent-scoped, not global config. Pool membership stays on `set_config`; package lifecycle + agent pin live on `manage_voices`.

**Shipped presets:** a small set of Omni attribute packages can be built-in (listed by `voices` / `manage_voices list`) so agents can `select` without `create` first.

### Sticky / “pin for session” (research answer)

| Layer | Today | After this plan |
| --- | --- | --- |
| **Agent sticky** | `(ClientSource, language) → voice id` | Same map; ids are **package ids**. `manage_voices select` writes it deliberately. |
| **Session sticky** | No | Optional later; not required for #152. |
| **Description sticky** | No | Design alone drifts; `materialize` → clone for true lock. |
| **Per-utterance** | `speak.tts_args.<target>.voice` | Package id override; does not change sticky unless product chooses to. |

**UX promise agents can rely on:**

1. `voices` / `manage_voices list` → choose an `id`.  
2. `manage_voices select` (or pool + first speak) pins that id for the agent.  
3. Optional `materialize` for Omni when stable timbre is required.  
4. Never promise free-form prose → stable speaker without bake/ref.

---

## 6. On-disk package store

Under the OS **data** dir (user-created voices are not pure cache):

```text
voices/
  chatterbox/
    default → built-in asset (virtual)
    narrator/
      package.toml    # kind=clone, label=...
      ref.wav
  omnivoice/
    british_male/
      package.toml    # kind=design, instruct=...
    british_male_clone/   # after materialize
      package.toml    # kind=clone
      ref.wav
```

Config pools remain string ids only:

```toml
[tts_voices]
chatterbox = ["default", "narrator"]
omnivoice  = ["british_male"]
```

Validation is **per kind**: catalog membership (Kokoro/Qwen); package exists on disk (Chatterbox/Omni); Omni design instruct parses against the official attribute table.

---

## 7. Implementation phases

### Phase 0 — Honesty

Document contracts in `docs/TTS-PIPELINE.md` and MCP descriptions. Do not present Omni’s single registry string as a real selectable catalog voice while ORT ignores it.

### Phase 1 — Voice string means something + MCP lifecycle

**Chatterbox (ORT first):**

1. `ensure_voice(id)` resolves `default` or `voices/chatterbox/<id>/ref.wav`.  
2. Keep in-memory conditioning cache.  
3. MLX: pass `refAudio` for packages.  
4. Registry/list = discovered packages + `default`.  
5. `manage_voices` create/delete/select + `set_config` pool admit.

**OmniVoice:**

1. Wire instruct on the provider path that supports it (MLX first if ORT export is auto-only).  
2. Ship curated design presets; optional custom attributes that **parse** the official table.  
3. `manage_voices` create/select for design packages.

**Exit:** ≥2 package ids per open model; agent A vs B can stick different ids; selection works entirely over MCP.

### Phase 2 — Sticky design + imports

1. Omni `materialize` (design → ref/clone package).  
2. Import ref from file for both models (duration/rate checks, hermetic tests).  
3. Optional disk cache of Chatterbox encoder outputs.

**Exit:** same package id across many turns has stable timbre for Omni (clone) and Chatterbox (same ref).

### Phase 3 — Optional

Session-keyed pin; status surfaces resolved package id (#151). Any host control is a thin client of the same MCP/engine path only.

---

## 8. Code touch points

| Area | Files |
| --- | --- |
| Registry / pools | `ds-config` `tts_model.rs`, `voice.rs` |
| MCP catalog | `ds-tools` (new `manage_voices` + descriptions), `dontspeak` tool dispatch |
| Listing | `ds-voices`, `dontspeak/src/voices.rs` |
| Assignment | `dontspeakd/src/ttsq.rs` (reuse sticky map; `select` writes package id) |
| Chatterbox | `ds-tts/src/chatterbox/synth.rs` |
| OmniVoice | `ds-tts/src/omnivoice.rs`, `synth_mlx.rs`, MLX `shim.swift` |
| Package I/O | new small module under `ds-config` or `ds-voices` (data-dir paths) |
| Docs | this file, `docs/MCP-TOOLS.md`, `docs/TTS-PIPELINE.md` |

Tests: tempdir packages; no live network; unknown id fails closed; attribute parse table; agent assignment sticky across two package ids; `set_config` rejects raw instruct in pool arrays.

---

## 9. Non-goals

- Free-form “warm British narrator with a smile” as a stable id without attribute parse or bake.  
- Raw instruct strings in `tts_voices.*` pool arrays.  
- Multi-ref fusion / multi-speaker single utterance.  
- Assuming Python `VoiceClonePrompt` exists in ORT until ported.  
- Coupling to terminal-window death (#35 / #140).

---

## 10. Recommended ship order

1. Phase 0 docs.  
2. Phase 1 Chatterbox packages + `manage_voices`.  
3. Phase 1 Omni presets + instruct on supporting provider.  
4. Phase 2 materialize.  
5. Session pin only if product needs per-window voices beyond agent sticky (still via MCP).

---

## 11. Direct answers

**Do the two models still expose one voice?** Yes on current main — registry and gates enforce one each; Omni ORT does not consume its string.

**Can a voice be pinned for a description for a session?**

- **Pin a package for an agent across turns:** yes — existing sticky assignment + MCP `manage_voices select`.  
- **Pin free-form / design description alone with stable timbre:** no on Omni without materialize; no on Chatterbox OSS (needs audio ref).  
- **True sticky “description”:** design package → `materialize` → clone package → `select` that id.

---

## 12. Risk

**Risk: yes** — TTS inputs, MCP contract (`manage_voices`), on-disk user assets, MLX shim. Implement in slices; Chatterbox package path and Omni presets can land as separate PRs under this plan.
