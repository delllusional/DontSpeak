# Text-to-speech narration pipeline

Canonical path from assistant text to Kokoro audio. Streaming mechanics:
[STREAMING-NARRATION.md](STREAMING-NARRATION.md). Focus/session:
[PER-TERMINAL-QUEUES.md](PER-TERMINAL-QUEUES.md).

## Contract

1. Adapters select narration; engine owns schedule, focus, mic, readiness, cancel, play.
2. ONNX and Core ML share validated phoneme chunks — no backend-local text frontend.
3. Accepted work stays observable (queued/in-flight) until terminal outcome, except
   external focus hold that leaves always-listening usable.
4. Nothing speakable → successful no-op. Synthesis that produces no audio → failure.
5. Degraded handling keeps audible fallback; one bad char must not silence the rest.

## Data flow

| Stage | Owner | Contract |
|---|---|---|
| Assistant text | Client hooks / Codex subscriber / MCP `speak` | Streamed batches or final utterance |
| Narration select | `ds-narrate` | Emit completed top-level blockquotes once; optional short non-quote final |
| Delivery | Hook/MCP IPC or in-process Codex | `SpeakNarration` / `Speak` / direct enqueue |
| Schedule | `dontspeakd::TtsQueue` | Session FIFO + policy |
| Text frontend | `ds-tts` | GFM → English norm → IPA → vocab filter → typed chunks |
| Synthesis | `ds-helper` | Same `KokoroPhonemeChunk` → ONNX or Core ML |
| Result | Helper + queue | Success, cancel, disabled, load fail, timeout, synth fail |

Three delivery routes (by design): hooks (commit HWM on queue accept), MCP (one-shot),
Codex (in-process). Streaming routes retry rejected work; no route yet propagates
terminal playback ACK to the producer.

## Shared English frontend

1. `SpokenText` (pulldown-cmark GFM): speak prose/labels/code; drop markup/HTML/link
   targets/task/footnote/alert markers. Block boundaries = word boundaries.
2. Bare URLs → word `link` (keep surrounding punctuation).
3. Number/version/identifier expand → one contextual G2P pass.
4. `voice-g2p` (empty eSpeak path — never invoked).
5. Dictionary misses → pinned BART ONNX G2P (cache successes; fail → spell ASCII or
   say `unknown`; degraded not permanently cached).
6. Drop OOV phonemes (warn); split to ≤ 509-char `KokoroPhonemeChunk`.

Cap 509: style matrix rows `0..=509` by unpadded token count. Whitespace/emoji-only →
zero chunks = success before opening audio.

## Models and backends

`ds-model` owns URLs/paths/digests. Kokoro + styles + BART + ORT checked as one stack.
BART is frontend, not ONNX-synth-only — Core ML path still needs ORT
(`kokoro_g2p_files_present`, `ensure_ort_dylib_gpu`).

**ONNX:** chunk → token IDs → style row → pad → shared ORT → PCM commit per batch.

**Apple Core ML:** `SmKokoro` / `smk_synthesize_phonemes` — caller IPA only; 24 kHz;
sync non-null PCM callback; null callback rejected.

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
