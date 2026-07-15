# Text-to-speech narration pipeline

This is the canonical end-to-end design for assistant text becoming Kokoro audio. It covers
narration selection, delivery to the engine, queue policy, the shared English text frontend,
the ONNX and Apple Core ML backends, and helper outcomes. Client-specific streaming mechanics
remain in [STREAMING-NARRATION.md](STREAMING-NARRATION.md); focus and session routing remain in
[PER-TERMINAL-QUEUES.md](PER-TERMINAL-QUEUES.md).

## Current contract

The pipeline has five load-bearing rules:

1. Client adapters select narration. Once an item reaches the engine, the engine owns scheduling,
   focus, microphone, readiness, cancellation, and playback policy; the legacy upstream mic gate
   that predates acceptance is called out under planned evolution.
2. ONNX and Apple Core ML consume the same validated phoneme chunks. Neither synthesis backend
   owns a text frontend.
3. Accepted work remains observable as queued or in flight until a terminal outcome, except
   while an external focus hold deliberately leaves always-listening usable.
4. Text with nothing speakable is a successful no-op. A request that reaches synthesis and
   produces no audio is a failure.
5. Degraded text handling preserves an audible fallback where possible; one unsupported
   character must not silence an otherwise useful utterance.

## Data flow

| Stage | Owner | Current contract |
|---|---|---|
| Assistant text | Claude hooks, Codex app-server subscriber, Qwen/Grok final hooks, MCP `speak` | Produce streamed batches or a final utterance |
| Narration selection | `ds-narrate` | Accumulate one message, emit completed top-level blockquotes once, and optionally emit a short blockquote-less final reply |
| Engine delivery | Hook IPC, MCP IPC, or in-process Codex enqueue | Hook adapters send `SpeakNarration`; MCP sends `Speak`; the Codex subscriber bypasses IPC and enqueues directly |
| Scheduling | `dontspeakd::TtsQueue` | Session-aware FIFO with record, focus, readiness, pause/resume, barge, and cancellation policy |
| Shared text frontend | `ds-tts` | Render GitHub-flavored Markdown, normalize English text, produce contextual IPA, filter it to Kokoro's vocabulary, and split it into typed chunks |
| Synthesis | `ds-helper` | Feed identical `KokoroPhonemeChunk` values to the ONNX or Apple Core ML backend |
| Playback result | Helper protocol and queue outcome | Distinguish success, cancellation, disabled/unavailable engines, load failure, timeout, and synthesis failure internally |

There are deliberately three delivery routes rather than one:

- Claude, Qwen, and Grok hook adapters send identified narration over NDJSON IPC and commit
  their delivered high-water mark only when the engine reports queue acceptance.
- The MCP `speak` tool sends IPC and reports immediate acceptance or rejection.
- The Codex app-server subscriber lives in the engine process and enqueues directly.

The streaming routes share stable admission identities and retry rejected work; the explicit MCP
route remains a one-shot request. No route yet propagates a terminal playback acknowledgement
back to the originating producer. That remaining limitation drives the planned evolution below.

## Shared English text frontend

`ds-tts` transforms source text once, before selecting a synthesis backend:

1. `SpokenText` parses GitHub-flavored Markdown with `pulldown-cmark`. Visible prose, labels,
   and code remain; formatting syntax, raw HTML, link targets, task markers, footnote markers,
   and GitHub alert markers do not become speech. Block boundaries are word boundaries at both
   start and end, which prevents nested list items from being joined into one word.
2. Bare URLs become the word `link`. ASCII, typographic, and CJK punctuation surrounding the
   URL is preserved so sentence boundaries survive.
3. Shared English normalization expands numbers, dotted versions, and supported technical
   identifiers before one whole-utterance contextual G2P pass.
4. Released `voice-g2p` supplies the Misaki tokenizer, tagger, lexicon, morphology, and contextual
   pronunciations. DontSpeak passes an empty eSpeak executable name, which cannot resolve through
   `PATH`; no production path invokes eSpeak.
5. Dictionary misses go to DontSpeak's checksum-pinned BART ONNX G2P model. Successful model
   loads and pronunciations are cached. If loading or prediction fails, the word is spelled when
   an ASCII spelling remains or rendered as the audible word `unknown`; degraded entries remain
   retryable rather than becoming permanent cache hits.
6. Any output phonemes outside `KOKORO_VOCAB` are dropped with a warning. The remaining IPA is
   split into `KokoroPhonemeChunk` values capped at 509 phoneme characters.

The 509 cap has no spare row: Kokoro's voice-style matrix contains rows `0..=509` and is indexed
by the unpadded token count. A 510-character phoneme chunk could select a missing style row, while
the vocabulary filter means a 509-character chunk produces at most 509 model tokens.

Whitespace-, image-, emoji-, symbol-, and punctuation-only input returns zero chunks. Both the
one-shot and warm-server helper paths treat that as success before opening audio or loading a
synthesis backend.

## Models, runtime, and backends

`ds-model` is the source of truth for asset URLs, paths, and SHA-256 digests. Kokoro synthesis,
voice styles, BART G2P, and ONNX Runtime are downloaded and presence-checked as one usable TTS
stack.

The BART model is part of the shared frontend, not the ONNX synthesizer. Consequently the Apple
Core ML / ANE path also needs ONNX Runtime:

- the ONNX route checks the shared G2P assets through its Kokoro presence gate;
- the Apple route uses `config_gate::kokoro_g2p_files_present()` in its native spawn/status gate;
- `bart::load` resolves the runtime through `ensure_ort_dylib_gpu` when inference is needed.

Presence policy requires those assets even though a transient runtime load failure can degrade to
letter spelling. This keeps advertised unknown-word quality consistent across backends.

### ONNX synthesis

The helper converts each typed chunk to Kokoro token IDs, chooses the voice-style row from the
unpadded token count, pads the model input, and runs the shared dynamically loaded ORT instance.
Returned PCM is trimmed and committed at the helper's per-batch playback boundary.

### Apple Core ML synthesis

`SmKokoro` accepts caller-supplied IPA through `smk_synthesize_phonemes` and FluidAudio's
`synthesizeFromPhonemesDetailed`. The shim does not normalize text or run G2P. Its required C
callback borrows one non-null PCM buffer synchronously on success; callers copy the samples
during the callback, and the expected sample rate is 24 kHz. A null callback is rejected.

## Queue, listener, and focus policy

`TtsQueue` is one bounded, session-tagged FIFO. Admission limits every item to 10 KiB of UTF-8
text and stops accepting new work at 128 queued items or 1 MiB globally, and 32 queued items or
256 KiB for one session. A narration overflow rejects that admission attempt without advancing
the producer's delivered offset. The identified utterance remains in the shared narration state:
the Codex subscriber retries it on housekeeping ticks, while hook clients retry before later
streaming batches and at `Stop`. The engine deduplicates a stable narration ID, closing the
success-response/state-commit window without adding a second queue item. Rejections remain logged
by the engine and echoed to the hook's stderr.
The active session and terminal-focus signals select which item may run; barge and explicit
cancellation use the same queue state. Details of session selection are in
[PER-TERMINAL-QUEUES.md](PER-TERMINAL-QUEUES.md).

The half-duplex listener uses `is_busy()` as its play gate. Dequeuing the final item and publishing
it as `in_flight` is one observable transition: the worker writes `in_flight` while holding the
queue lock, and `is_busy()` takes that lock before sampling the flag. A reader therefore observes
either queued work or in-flight work, never a false idle gap between them.

Two independent conditions can hold a dequeued item:

- A **mic-only hold** reports busy. Busy closes the always-listening capture, which frees the mic
  and lets the hold clear.
- A **focus hold** does not report busy unless audio is already playing. Closing the mic cannot
  restore terminal focus, so focus takes precedence if both holds are active. Queued and in-flight
  focus-held blocks read idle to the listener, avoiding both permanent shutdown and open/close
  oscillation. When terminal focus returns, pending work becomes busy, capture closes, and the
  mic-only hold clears before playback.

The worker keeps accepted work through ordinary model warm-up and retryable readiness waits.
Every terminal path clears the in-flight state and records an internal outcome rather than
pretending that a failed synthesis played successfully.

## Helper protocol and failure semantics

The one-shot and warm-server routes share the frontend and chunk contract:

- zero phoneme chunks return success without loading synthesis;
- a backend load error returns an error (`TTSLOADERR` plus terminal `ERR` in warm mode);
- a piece that fails synthesis, produces empty PCM, or leaves the request with no audio returns
  `ERR`;
- a failed batch never commits partial PCM; if a later batch fails, the helper stops any already
  committed prefix before reporting `ERR`;
- successful synthesis returns `DONE` only after the request produced valid audio.

The shared frontend creates model-bounded, sentence-aware phoneme batches with a small first
budget and a ramp toward the model limit. Preparation synthesizes and validates one complete
batch, commits it immediately, then synthesizes the next batch while playback drains the already
committed PCM. This keeps the failing batch transactional while making time-to-first-audio depend
on the first batch rather than the entire utterance. When synthesis runs slower than real time,
playback can gap briefly between batches; the previous design instead delayed all audio until the
whole utterance was staged. After such a drain the helper re-prepends the short leading silence so
the resumed onset is not clipped. It also bounds preparation memory to one
phoneme batch, including for queue items near the 10 KiB admission limit. The default warm helper
commits into a persistent output stream and starts incrementally. The short-lived macOS one-shot
route still accumulates committed batches for reliable `afplay` teardown.

Record-barge resume is batch-granular. On the rodio path the warm helper reports a `PROGRESS`
high-water mark — the absolute count of batches estimated fully played, capped at the barge's
audible-stop instant so committed-but-unheard audio is never counted — and the engine echoes the
mark back as `skip` on the requeued item's next run, so the helper drops the already-played prefix
instead of re-speaking the block from the top. `STATS` then covers the resumed tail only. Version
skew degrades to replay-from-top in both directions: an older engine ignores `PROGRESS`, and an
older helper never emits it. Full-duplex reports no mark — it never pauses/resumes.

On macOS full-duplex, committed batches are handed to a dedicated feeder thread, so committing
returns immediately and the next batch's inference overlaps pacing. The feeder paces small chunks
into VPIO with about two seconds of render lookahead, rechecking cancellation and global mute
between chunks and using a shorter lookahead for muted silence so unmute also responds promptly.
On failure the helper aborts the feeder before clearing the render ring, so a committed prefix
never plays under an error reply.

Queue logs preserve disabled, unavailable, cancelled, timeout, load-error, synthesis-error, and
played outcomes even though those terminal states are not yet propagated back to the original
narration record.

## Known limitations and planned evolution

The pipeline has stable admission IDs, engine-side deduplication, and producer retries, but it
cannot yet promise exactly-once delivery through terminal playback. Remaining work is to retain
bounded state for interleaved messages and correlate helper terminal outcomes with the admitted
narration ID.

The concrete gaps are:

- A seeded streaming witness currently means "streaming armed", not "text accepted". It can
  suppress the final fallback before any narration reaches the queue.
- `DisplayState` stores one current message per session rather than a bounded map of stable message
  IDs, ordering metadata, completed high-water marks, and TTLs.
- When a transport omits `message_id`, the hook adapter derives a key from the first 48 characters
  of a batch. Incremental batches are not a reliable message identity.
- `ds-narrate` makes one mic decision at a message's first batch while the queue separately owns
  hold/pause behavior. The queue should become the sole delivery-policy owner.
- File paths, UUIDs, email addresses, emoji, Unicode normalization/transliteration, and explicit
  unsupported-language behavior still need a documented spoken-text policy.
- Backend-parity fixtures need adversarial 509/510 boundaries, technical text, and platform smoke
  coverage at the shared phoneme boundary.
- Accented words can degrade to a partial ASCII spelling, while wholly non-Latin unknown words
  generally become the audible word `unknown`; deterministic transliteration is still absent.
- Queue overflow is rejected at acceptance, but the FIFO has no stale-work age or priority
  distinction between user-requested MCP speech and replaceable streaming narration.

The Kokoro readiness deadline is a real upper bound (issue #59, fixed): `start_locked()`'s
pre-`READY` handshake is itself bounded (120 seconds, `READY_HANDSHAKE_TIMEOUT`) and a helper
that stays alive without printing `READY`, `ERR`, or EOF is killed at that bound; the queue
worker's readiness wait never blocks on child lifecycle calls — crash healing runs on a
single-flight background thread while the wait keeps polling.

## Verification boundaries

| Boundary | Required fixtures |
|---|---|
| Adapter to reducer | Delta/cumulative input, duplicate, reordered, interleaved, missing-ID, malformed-final, reconnect |
| Reducer to normalizer | Multiple blockquotes, short fallback, Markdown links/code, URLs/paths, long digits, Unicode |
| Queue policy | Warm-up, load failure, IPC failure, pre-READY wedge, mic/focus transitions, session barge, overflow, duplicate ID |
| Helper protocol | Success, cancellation, empty PCM, first-piece failure, partial-piece failure, lazy reload failure |
| Backend parity | Normalized input equality, exact input bounds, representative phoneme/token coverage |
| End to end | One identified utterance reaches one terminal playback outcome on every client route |

Correctness fixtures must not use live network endpoints. Model/audio smoke tests belong in the
platform build and release routes; reducer, protocol, and queue tests use pure or injected inputs.
