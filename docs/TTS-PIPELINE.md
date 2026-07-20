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
| Schedule | `dontspeakd::TtsQueue` | Session FIFO + policy; detect and pin one ISO language per utterance |
| Text frontend | `ds-tts` | One GFM → prose cleanup for every route; model-capability frontend → bounded typed chunks; System stops at prose |
| Synthesis | `ds-helper` (built-in) / OS voice (System) | Selected built-in model → ORT CPU everywhere or a supported accelerator; System → `say` / SAPI / spd-say |
| Result | Helper + queue | Success, cancel, disabled, load fail, timeout, synth fail |

Narration selection decides what to speak but does not rewrite it. Every delivery route
converges on `ds_tts::normalize_spoken_text`, which owns markup, URL, hash, and whitespace
cleanup once. Backend normalization starts from that prose; it does not duplicate cleanup.
The `whatlang` detector sees that normalized prose, scoped to the selected model's
supported languages, so detection only ever yields a language that model can speak (or
the English fallback for ambiguous or unspeakable input). No utterance is refused for
language — the model is never changed to fit a detected language.

Three delivery routes (by design): hooks (commit HWM on queue accept), MCP (one-shot),
Codex (in-process). Streaming routes retry rejected work; no route yet propagates
terminal playback ACK to the producer.

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
5. Japanese: a native OpenJTalk-compatible tokenizer/dictionary mapped with Misaki's
   Kokoro phoneme rules. Mandarin: Jieba segmentation, pinyin, number expansion, and
   Misaki's pinyin-to-IPA mapping.
6. Drop OOV phonemes (warn); split to ≤ 509-character `KokoroPhonemeChunk`.

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
receives plain multilingual text and a speaker ID; OmniVoice uses automatic voice design.
All return 24 kHz PCM through the synchronous borrowed-buffer callback.

### Built-in model registry

| Model | Language mode | Voices | Providers | Rate / full duplex |
|---|---|---|---|---|
| Kokoro | English, Spanish, French, Hindi, Italian, Japanese, Portuguese, Mandarin | Kokoro voice catalog | ORT CPU/CUDA/Core ML, MLX | yes / yes |
| Chatterbox Multilingual | 23 explicit languages | pinned reference voice | ORT CPU/CUDA, MLX | no / no |
| Qwen3-TTS CustomVoice | 10 explicit languages | 9 built-in speakers | ORT CPU/CUDA, MLX | no / no |
| OmniVoice | auto (any detected language) | default voice | ORT CPU, MLX (no CUDA — pinned int4 export) | no / no |

Chatterbox caches transient reference-voice conditioning and uses named/model-derived KV
caches. Qwen uses the exported cached talker and fixed-frame decoder. OmniVoice performs
32-step confidence-weighted unmasking before Higgs decoding. All long loops poll cancel.

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

Terminal outcomes logged but not yet correlated back to narration records.

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
