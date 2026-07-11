# Speech-to-text pipeline audit and implementation plan

Date: 2026-07-11

Scope: built-in ONNX/FastConformer STT, macOS Core ML Parakeet, macOS System STT,
half/full-duplex capture, always-listening, helper transport, partial/final delivery,
and the macOS/Windows/Linux overlay consumers. The recognizer technologies and model
families stay unchanged.

## Acceptance criteria

- Capture and inference never run on an app UI thread or the engine poll thread.
- A listen stop cannot be lost, even when it arrives before capture starts.
- At most one logical recognition session owns the helper's listen stream.
- Resampling and memory use grow linearly with utterance duration.
- The selected STT provider is honored in tap-to-talk and always-listening modes.
- Partials are delivered through bounded newest-value queues; a slow UI cannot build a
  stale backlog.
- Audio callbacks do not allocate or wait on a contended mutex.
- Capture backend failure either reconnects or produces an actionable terminal failure.
- A wedged helper finalization is bounded and recoverable by restarting the helper.

Target budgets after model warm-up:

- First visible partial: p95 <= 800 ms.
- Partial transport overhead after the recognizer emits: p95 <= 80 ms.
- Stop-to-final: p95 <= 500 ms in the normal path.
- Engine poll work: <= 10 ms per tick; no model inference in a tick.
- UI status backlog: at most one pending snapshot.

## Findings

### P0 — shared correctness and latency

1. `StreamSession` resamples the complete captured history on every 50 ms drain. This
   makes the capture-to-model layer quadratic and copies the whole utterance even at
   16 kHz.
2. The common streaming path bypasses configured capture gain and the speaker-lock
   final filter. The latter is hidden, but silently ignoring an enabled safety control
   is still incorrect.
3. Half-duplex and full-duplex helper control use resettable booleans. A stop received
   before the queued start begins can be overwritten, leaving capture active until its
   hard timeout.
4. `TtsManager` has one global listen event queue but no exclusive session lease. Test
   recognition can overlap Caps dictation and clear or consume its events.
5. Always-listening hard-codes the ONNX transcriber even when macOS resolved to ANE or
   System STT, and performs model load/inference synchronously on the engine poll thread.

### P1 — resource and real-time behavior

6. STT preload warms both an offline transcriber and a separate streaming backend,
   duplicating model sessions and memory in the steady-state path.
7. The half-duplex CPAL callback writes through `Mutex<Vec<f32>>`; each drain discards
   capacity, so the real-time callback may allocate again and can wait on the consumer.
8. The helper requires Kokoro files and an output device even when it is needed only for
   STT. TTS and STT role residency are not independently bootable.
9. Core ML streaming calls have no direct cancellation. A native call that never returns
   can strand the listen thread and prevent a final result.

### P1 — platform-specific behavior

10. Linux full-duplex capture exits permanently after one PulseAudio/PipeWire read error.
11. A Windows WASAPI reconnect can negotiate a different rate while downstream keeps
    interpreting samples at the original rate.
12. Legacy macOS System STT recognizes a phrase reset only when the new hypothesis is
    shorter. A short previous phrase followed by a longer phrase can be dropped.
13. Linux forwards unchanged timeout snapshots through an unbounded channel. Windows
    enqueues every partial separately onto the dispatcher. Both can replay stale UI work
    after a temporary stall.

## Implementation plan

1. Replace whole-history resampling with a persistent Rubato streaming resampler, add
   incremental gain conditioning, and force the hidden speaker-lock mode through the
   existing filtered fallback until it has a streaming-safe implementation.
2. Replace helper listen booleans with monotonically increasing session generations and
   enforce one `TtsManager` listen lease. Bound post-stop finalization with helper recovery.
3. Move always-listening capture and transcription to background workers and construct its
   backend through `LocalTranscriber` using the resolved engine/provider.
4. Use a bounded lock-free capture ring for CPAL, preload only the backend used by normal
   streaming, and make TTS/STT helper startup roles independent.
5. Add Linux capture reconnect, resample Windows reconnects back to the stable published
   rate, strengthen legacy macOS phrase-reset detection, and coalesce Linux/Windows status
   updates to the newest value.
6. Add regression tests for incremental resampling, gain, listen ownership/generations,
   queue coalescing, and pure phrase-reset logic where the platform build permits it.
7. Run format, targeted tests, workspace tests, and clippy; then audit the final diff and
   commit it on `stt-pipeline-audit-fixes`.

## Implementation status

Completed on 2026-07-11 on branch `stt-pipeline-audit-fixes`:

- `2b0bafc` — incremental resampling/gain, final capture drain, and a lock-free bounded
  CPAL callback ring.
- `7bd2392` — exclusive generation-tagged listen sessions, bounded finalization recovery,
  single-backend STT residency, and independent STT-only helper startup.
- `b33643d` — provider-aware off-thread always-listening, Linux capture reconnect,
  Windows reconnect-rate continuity, macOS legacy reset strengthening, and newest-only UI
  delivery.
- Final verification checkpoint — generation/lease regressions, in-use backend unload
  completion, strict Clippy, full workspace tests, and final diff review.

Validation completed on the Windows host:

- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`
- `dotnet build apps/windows/winui/DontSpeak.WinUI.csproj` (zero warnings/errors)

The Linux cross-check is limited by PulseAudio's native `pkg-config` dependency not being
configured for cross-compilation on this Windows machine. Swift tooling is not installed, so
the macOS shim must also pass its native CI/build job. The latency percentiles above are runtime
budgets rather than unit-test claims; validate them with warm-model hardware telemetry on each
release platform.

## Independent correctness audit (follow-up)

Date: 2026-07-11. A second pass, independent of the implementation above, reviewed every
finding's fix for correctness (not just "does it compile"), verified the branch's actual CI
state, and fixed what it found. Findings 1–4/6–9/11/13 checked out correct as implemented;
finding 12 substantively improves the reset detection it targets. Three real defects were
found and fixed on this branch (all committed on `stt-pipeline-audit-fixes`); a few smaller
gaps are recorded below as deliberately NOT fixed in this pass, with why.

### Fixed

1. **Branch did not compile on Linux (per-commit CI gate).** `b33643d`'s Linux capture
   reconnect (finding #10) added `log::warn!`/`log::info!` calls to
   `rust/crates/ds-aec/src/linux.rs` without adding `log` to `ds-aec/Cargo.toml`'s
   `[target.'cfg(target_os = "linux")'.dependencies]` — a hard `E0433` on the one platform
   `ci.yml` actually builds (Linux-only per-commit gate), invisible from a Windows dev
   machine because `linux.rs` is never compiled there. Confirmed both failing GitHub Actions
   runs for `b33643d` and `646f769`. Fixed by adding `log = { workspace = true }` next to the
   other Linux-only deps; verified by building + `cargo clippy --workspace --all-targets -D
   warnings` + `cargo test --workspace` on a real Ubuntu 26.04 host (not just `cargo check`).

2. **[HIGH] The exact race finding #3 fixed at the ds-helper/serve.rs layer was reintroduced
   one layer up, in `dontspeakd`.** `HelperStt::start()` (Caps dictation) and
   `TestSession::run()` (MCP test recognition) each spawn/dispatch `TtsManager::listen()` on
   one thread while their OWN `stop()` can arrive on a DIFFERENT thread. `listen()` only
   published `active_listen_generation` once it actually started running on its thread, so a
   stop that raced the thread handoff (a fast Caps tap-then-release, or a test-recognition
   client that calls stop right after listen) saw `active_listen_generation == 0` and
   silently no-opped — the queued listen then proceeded unaware, reproducing "stop received
   before the queued start begins" one layer above where the ds-helper fix closed it. Fixed
   by adding `TtsManager::listen_cancellable(&AtomicBool, ...)`: each caller now owns a fresh
   early-stop flag, set by its `stop()`/`abort()` before `stop_listen()` runs, and checked
   both before the helper is started and at the existing generation-check point. `listen()`
   (unused once both callers moved over) was removed rather than left as dead code.
   Regression test: `tts::status_gate_tests::listen_cancellable_honors_a_stop_that_raced_the_call_itself`.

3. **[MEDIUM] Always-listening didn't rebuild on a live STT provider switch.** Finding #5's
   acceptance criterion is "the selected STT provider is honored... in always-listening
   mode," but `Engine::reload`'s listener-rebuild trigger
   (`self.listener.is_none() || listen_changed || local_avail_flipped`) never checked
   `change.stt_changed` the way the tap-to-talk `build_stt` rebuild does. Switching STT
   engine/provider (e.g. CPU → CUDA) while Always mode stayed on and both providers were
   already locally available left the background listener on the STALE provider
   indefinitely — `local_avail_flipped` only catches a false→true/true→false edge, not a
   same-availability provider swap. Pre-existing (not a regression from this branch's other
   commits), but within scope since it's the same acceptance criterion finding #5 claims to
   satisfy. Fixed by adding `change.stt_changed` to the rebuild condition. Regression test:
   `engine::tests::reload_always_mode_rebuilds_the_listener_on_a_live_stt_provider_switch`
   (a new `#[cfg(test)]`-only `Listener::provider()` accessor makes the rebuild observable
   without needing real, checksum-pinned Parakeet model files).

4. **[LOW] A duplicate `stop_listen()` call restarted the wedge-recovery clock.** Finding
   #9's finalize-timeout bound (10 s Parakeet / 35 s System) is meant to be a hard deadline
   from the FIRST stop; `stop_listen()` unconditionally overwrote `listen_stop_started`'s
   `Instant` on every call, so a second stop for the same generation (no live call site does
   this today, but nothing prevented it) would have pushed the deadline back out instead of
   being a no-op. Fixed: only the first stop for a given generation sets the clock.
   Regression test: `tts::status_gate_tests::stop_listen_does_not_restart_the_finalize_clock_on_a_duplicate_call`.

5. **[LOW] Stale comments contradicted the code next to them.** `ds-helper/src/serve.rs`'s
   NOTE above the STT preload claimed `STTLOADED`'s truthiness still reflected `transcriber`
   specifically — no longer true once `7bd2392` unified it behind `loaded`. `tts/mod.rs`'s
   comment above the Kokoro-presence gate claimed Kokoro was gated unconditionally and no
   `DONTSPEAK_TTS_PRELOAD`-style role gate existed in `serve.rs` — contradicted by
   `prefs.tts_preload` two lines below and by `serve.rs`'s own `tts_wanted` gate, both added
   in the SAME commit the stale comment predates. Both rewritten to describe current
   behavior.

All fixes verified with `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo test --workspace --locked`, and `cargo fmt --all --check` on Windows, AND — unlike the
implementation pass above — the same three commands on a real Ubuntu 26.04 host (VirtualBox,
not cross-compiled), so the Linux per-commit CI gate this branch actually runs is now genuinely
exercised, not just reasoned about.

### Reviewed and confirmed correct (no change)

Findings 1 (incremental resampler), 2 (gain + speaker-lock fallback), 6 (single-backend STT
residency), 7 (lock-free CPAL ring), 8 (independent STT/TTS helper roles), 11 (Windows WASAPI
reconnect-rate continuity), and 13 (Linux `bounded(1)`+`force_send` / Windows
latest-value-only dispatch) were each traced end-to-end against their acceptance criteria and
found correctly implemented. Finding 9's bounded finalize-timeout genuinely recovers a wedged
native call by killing and restarting the HELPER PROCESS (`mark_dead_locked` reaps the child),
not just abandoning an in-process thread. Finding 12's macOS phrase-reset fix (a `phraseGap`
timing heuristic alongside the existing shorter-hypothesis check) correctly covers the
reset-to-longer-no-shared-prefix case the finding described.

### Known gaps left open (not fixed in this pass)

- **Finding 12's `0.65s` phrase-gap constant is asserted, not measured** (unlike the paired
  `<0.5` ratio threshold, which has an empirical justification in `shim.swift`). A genuine
  reset arriving faster than 0.65 s still reproduces the original bug. Tuning this needs real
  recognizer timing data and a Swift toolchain, neither available in this pass — left as a
  documented constant, not silently "fixed" with a guessed number.
- **Linux capture reconnect (finding #10) never reaches a terminal failure state** — it
  retries every 500 ms indefinitely against a dead PulseAudio/PipeWire server rather than
  eventually surfacing an actionable error the way Windows' `last_error()` does. Bounded and
  non-busy, so not a regression, but the acceptance criterion ("reconnects OR produces an
  actionable terminal failure") is only half-met on Linux. Left open: adding a UI-facing
  terminal-failure surface is a feature addition, not a bug fix, and needs its own design
  pass (does it reuse `last_error()`'s shape? gate on a retry count or a wall-clock budget?).
- **Test coverage gaps remain for the largest mechanisms in `b33643d`/`646f769`**: the
  off-poll-thread `ListenerWorker` (finding #5's core fix) and `ds-helper`'s
  `BackendCache.active` concurrency guard (the race `7bd2392` itself introduced and `646f769`
  closed) are both real, correct, and both untested — proving them needs either real
  Parakeet model files (SHA256-pinned, not fakeable with placeholder files) or a mock
  `StreamingStt`/capture backend that doesn't exist in the test harness yet. Out of scope for
  a review pass; worth its own follow-up if this pipeline sees more churn.
