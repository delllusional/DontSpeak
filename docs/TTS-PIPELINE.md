# Text-to-speech narration pipeline

Canonical path from assistant text to built-in or system speech. Streaming mechanics:
[STREAMING-NARRATION.md](STREAMING-NARRATION.md). Focus/session:
[PER-TERMINAL-QUEUES.md](PER-TERMINAL-QUEUES.md).

## Contract

1. Adapters select narration; engine owns schedule, focus, mic, readiness, cancel, play.
2. Kokoro's ORT and MLX paths share validated phoneme chunks. Chatterbox, Qwen, and
   OmniVoice share deterministic plain-text chunks and keep model-specific tokenization
   inside their inference pipelines.
3. Accepted work stays observable (queued/in-flight) until terminal outcome, except
   external focus hold that leaves always-listening usable.
4. Nothing speakable → successful no-op. Synthesis that produces no audio → failure.
5. Degraded handling keeps audible fallback; one bad char must not silence the rest.

## Data flow

| Stage | Owner | Contract |
|---|---|---|
| Assistant text | Client hooks / Codex subscriber / MCP `speak` | Streamed batches or final utterance |
| Narration select | `ds-narrate` | Emit completed top-level blockquotes once; optional short non-quote final; preserve selected text for the shared frontend |
| Delivery | Hook/MCP IPC or in-process Codex | `SpeakNarration` / `Speak` / direct enqueue |
| Schedule | `dontspeakd::TtsQueue` | Session FIFO + policy; detect one ISO language per **utterance** at admit and carry it with optional target-keyed MCP arguments |
| Text frontend | `ds-tts` | One GFM → prose cleanup for every route; model-capability frontend → bounded typed chunks; System stops at prose |
| Synthesis | `ds-helper` (built-in) / OS voice (System) | Selected built-in model → ORT CPU everywhere or a supported accelerator; System → `say` / SAPI / spd-say |
| Result | Helper + queue | Success, cancel, disabled, load fail, timeout, synth fail |

Narration selection decides what to speak but does not rewrite it. Every delivery route
converges on `ds_tts::normalize_spoken_text` (markup, URL, hash, whitespace once).
Backend normalization starts from that prose. For built-in speech, the `whatlang` detector
sees that prose scoped to the selected model's languages, so detection only yields a
speakable language (or English for ambiguous/unspeakable input). System speech detects over
the full mapped language range because it has no built-in-model allowlist. The model is never
swapped for language; the **voice** is (see [Voice selection](#voice-selection)).

**Per-utterance language.** Every queued speech item carries the ISO code decided when it
was admitted, so a reply that switches language is voiced per utterance instead of under
one turn-wide verdict. `ds_tts::chunk_language` decides it: the utterance's own prose when
whatlang calls that reliable or its confidence clears 0.3, else the turn corpus when that
is reliable, else English. Streaming and Stop narration supply the corpus as
`detection_text` (reconstructed message-so-far, capped at 10 KiB); MCP `Speak` has none
and stands on its spoken text unless the active `tts_args.<target>.language` overrides the
result. The confidence floor is what keeps a one-line English
digest out of a French voice — such misfires sit under 0.25, while short prose that really
is Romance clears 0.3 (`digest_confidence_separates_english_from_the_rest`). A digest with
neither its own evidence nor a corpus is spoken English rather than a coin flip.
`play_speech` clamps the carried code with `supported_language`, which covers a model
switch between admit and play.

Three delivery routes (by design): hooks (commit HWM on queue accept), MCP (one-shot),
Codex (in-process). Streaming routes retry rejected work. Every admitted utterance gets a
handle; `speak` returns it and status carries the terminal record (resolved voice/language
plus `spoken`/`failed`/`cancelled`/`dropped`). Hooks and Codex do not surface the handle to
their producer, so only MCP has the ACK end to end. MCP `Speak` chooses the active
`tts_args` target at playback, so a queued config switch cannot leak one model's voice,
language, or parameters into another. Named parameter values merge over that target's
configured values for the utterance.

## Kokoro language frontends

All Kokoro backends consume the same validated phoneme chunks. System TTS stops after
shared prose cleanup and leaves pronunciation to the OS.

1. `SpokenText` (pulldown-cmark GFM): speak prose/labels/code; drop markup/HTML/link
   targets/task/footnote/alert markers and commit-like hashes. Block boundaries = word
   boundaries. This is the only text-cleanup implementation.
2. Bare URLs → word `link` (keep surrounding punctuation).
3. English: number/version/identifier expansion → `voice-g2p`; dictionary misses use
   pinned BART ONNX G2P (cache successes; fail → spell ASCII or say `unknown`).
4. Spanish, French, Hindi, Italian, and Portuguese: the MLX Audio/Misaki eSpeak route,
   using a checksum-pinned `espeakng-loader` wheel loaded through the eSpeak C ABI.
5. Drop OOV phonemes (warn); split to ≤ 509-character `KokoroPhonemeChunk`.

Cap 509: style matrix rows `0..=509` by unpadded token count. Whitespace/emoji-only →
zero chunks = success before opening audio.

## Models and backends

`ds-model` owns immutable URLs, paths, digests, licenses, and per-model directories.
Kokoro + styles + English BART + multilingual frontend assets + ORT are checked as one stack.
BART is frontend, not ONNX-synth-only — MLX still needs ORT
(`kokoro_g2p_files_present`, `ensure_ort_dylib_gpu`).

**ONNX:** detected language + bounded chunk → model tokenizer/conditioning → shared ORT sessions → PCM
commit per batch. Provider acceleration is enabled only where the registry declares it.

**Apple MLX:** the shared `DontSpeakMLX` ABI loads only DontSpeak-populated model
directories. Kokoro receives IPA; Chatterbox uses its pinned default conditioning; Qwen
receives plain multilingual text and a speaker ID; OmniVoice receives a style instruct
resolved from its preset id in Rust (`OMNIVOICE_PRESETS`) — `default` stays automatic
voice design. All return 24 kHz PCM through the synchronous borrowed-buffer callback.

**FluidAudio (`fluid`, Apple Silicon, Kokoro only, opt-in):** the same signed shim dylib
exposes `ds_fluid_tts_*` over FluidAudio's ANE Kokoro. It receives the same Rust-owned IPA
phonemes as every other Kokoro backend — DontSpeak skips FluidAudio's own G2P so the backends
cannot drift. Two extra assets it needs: FluidAudio's `initialize()` eagerly loads a
G2P/lexicon set from its hardcoded `~/.cache/fluidaudio/Models/kokoro`, which DontSpeak
pre-fills (the `KOKORO_G2P_COREML` repo) for load only; and because the Core ML repo ships one
voice pack (`ANE/af_heart.bin`) and `ensureVoicePack` throws with no fallback, any other voice's
`[510, 256]` pack is materialized on demand from the ONNX `voices-v1.0.bin` before synthesis.

### Built-in model registry

| Model | Language mode | Voices | Providers | Rate / full duplex |
|---|---|---|---|---|
| Kokoro | English, Spanish, French, Hindi, Italian, Portuguese | Kokoro voice catalog | ORT CPU/CUDA/Core ML, MLX, FluidAudio | yes / yes |
| Chatterbox Multilingual | 23 explicit languages | pinned reference voice | ORT CPU/CUDA, MLX | no / no |
| Qwen3-TTS CustomVoice | 10 explicit languages | 9 built-in speakers | ORT CPU/CUDA, MLX | no / no |
| OmniVoice | auto (any detected language) | 10 design presets | ORT CPU/CUDA, MLX | no / no |

Chatterbox caches transient reference-voice conditioning and uses named/model-derived KV
caches. Qwen uses the exported cached talker and fixed-frame decoder. OmniVoice runs
16-step classifier-free-guided per-cell unmasking against the bidirectional fp32
backbone before Higgs decoding. All long loops poll cancel.

OmniVoice stays **non-default on CPU**: the CPU EP misses the first-chunk latency bar
(measured RTF 9.3 at 32 steps, 4.6 at the shipped 16 — bar ≤ 2.0), while CUDA meets its
bar (RTF 0.6). Under a CUDA backbone the Higgs decoder runs on the **CPU EP** — the
pinned FP16 decoder graph emits NaN under CUDA — until
[#165](https://github.com/delllusional/DontSpeak/issues/165) closes; status and
synth-check report the backbone's provider.

### Voice selection

The detected language picks the voice as well as the frontend. `ds_voices::VoiceCatalog`
is the one place that answers "can this voice speak this language", for every engine:

| Catalog | Owns a language? | From |
|---|---|---|
| Kokoro | yes | id family char (`if_sara` → `it`) |
| System | yes | `say -v ?` locale tag, primary subtag |
| Chatterbox, Qwen, OmniVoice | no | conditioned by the language argument at synthesis |

`resolve_engine_voice` narrows the configured pool to the voices that own the detected
language. A catalog whose voices own none returns the pool unnarrowed, so this is a no-op
for those models rather than a per-engine branch. When the pool owns nothing for the
language, the catalog's own voices for it (the same list the picker and the `voices` tool
show) stand in, so a language the user configured nothing for is still spoken by a voice
that owns it. Either way the candidates go through `pick_agent_voice`, keeping the roll
random, the assignment sticky, and agents on distinct voices while spares remain.

Assignments are keyed by `(agent, language)`: one voice per language per agent, so a reply
that switches language does not re-roll the other. Nothing owning the language anywhere —
a fresh install whose Kokoro ids are still the static English fallback — keeps the agent's
usual voice; synthesis still receives the detected language, so pronunciation is right
even when the voice is not, and `g2p` logs that mismatch.

Pool membership is validated against routed languages, not English: Kokoro publishes
German, Japanese, and Mandarin voices whose frontends this build does not ship, and those
stay rejected.

## Queue and focus

Bounded session FIFO: 10 KiB/item; 128 / 1 MiB global; 32 / 256 KiB per session.
Overflow rejects without advancing producer offset; ID-dedup closes accept/commit window.
Half-duplex listener uses `is_busy()`; dequeue + `in_flight` is one locked transition
(no false idle gap).

Holds:

- **Mic-only** — reports busy (closes always-listening capture → frees mic).
- **Focus** — does not report busy unless already playing (mic can't restore focus).
  Focus wins if both. Idle to listener under focus-hold to avoid open/close thrash.

Worker keeps accepted work through warm-up; every terminal path clears in-flight and
records outcome.

## Helper protocol

Shared one-shot + warm-server:

- Each speak request carries its detected ISO language; helper startup has no language setting
- Zero chunks → success, no backend load
- Load fail → `TTSLOADERR` (+ terminal `ERR` warm)
- Empty PCM / synth fail → `ERR`; never commit partial on later batch fail
- `DONE` only after valid audio

Prep: synth+validate batch, commit, overlap next batch while playing (first-batch
TTFA). Slow synth may gap between batches; re-prepend leading silence after drain.
Memory: one batch. Warm: persistent stream; macOS one-shot: accumulate for `afplay`.

Record-barge: rodio `PROGRESS` HWM (batches fully played); requeue with `skip`. Skew →
replay-from-top. Full-duplex: no mark (no pause/resume).

macOS full-duplex: feeder thread + ~2 s VPIO lookahead; mute zeros output at render
while draining ring at wall rate (AEC far-end = speakers). Fail: abort feeder before
clearing ring.

Terminal outcomes are recorded per utterance handle (`tts.recent_utterances`, 16 deep) but not
correlated back to narration ids: a hook producer holds a narration id, not the handle the
queue minted for it.

## Gaps / planned

No exactly-once through terminal playback yet. Gaps:

- Witness means "streaming armed", not "text accepted" (can suppress Stop early)
- `DisplayState` is one message/session, not bounded multi-ID map
- Missing `message_id` → first-48-char key (unreliable for incremental)
- Mic decision at first batch vs queue-owned policy — queue should own delivery policy
- Paths/UUIDs/email/emoji/Unicode policy incomplete
- Parity fixtures need adversarial 509/510 + technical text
- Non-Latin unknowns → `unknown`; no deterministic transliteration
- No stale-age / priority between MCP speech and streaming narration

Kokoro readiness: `READY` handshake ≤ 120 s (`READY_HANDSHAKE_TIMEOUT`); wedge kills
helper; crash heal on single-flight background thread (issue #59).

## Verification boundaries

| Boundary | Fixtures |
|---|---|
| Adapter → reducer | delta/cumulative, dup, reorder, interleave, missing ID, malformed final, reconnect |
| Reducer → normalizer | multi-quote, short fallback, MD links/code, URLs/paths, long digits, Unicode |
| Queue | warm-up, load fail, IPC fail, pre-READY wedge, mic/focus, barge, overflow, dup ID |
| Helper | success, cancel, empty PCM, first/partial piece fail, lazy reload |
| Backend parity | normalized equality, bounds, phoneme/token coverage |
| E2E | one ID → one terminal outcome per client route |

No live network in correctness fixtures. Audio smoke = platform/release routes.
