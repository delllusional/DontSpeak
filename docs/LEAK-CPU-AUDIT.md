# DontSpeak — Memory-Leak & Wasted-CPU Audit

**Date:** 2026-06-27
**Scope:** Whole application — all 15 Rust crates + the SwiftUI macOS app.
**Method:** Static source audit (multi-agent fan-out, one auditor per area), every
finding independently cross-checked by an adversarial reviewer that re-read the code and
tried to refute it. Only findings that survived cross-check are listed. Severities shown
are the **post-cross-check adjusted** values.

> Why static, not Instruments: the runtime profilers (Xcode Instruments Leaks / Time
> Profiler / Energy Log, `tokio-console`, jemalloc heap dumps) are the right tools to
> *confirm magnitude* on a running build, but the defects below are all structural and
> identifiable in source. Recommend a follow-up Instruments/`tokio-console` pass on a real
> build to put numbers on the daemon poll loops and the Swift idle wake-ups.

## Coverage

| Area | Crates / paths | Result |
|------|----------------|--------|
| TTS engine | `ds-tts` | 3 findings |
| Daemon | `dontspeakd` | 6 findings |
| MCP + IPC | `dontspeak`, `ds-ipc`, `ds-status` | 2 findings |
| SwiftUI app | `apps/macos/Sources/DontSpeak`, `SmKokoro/Sources` | 3 findings |
| STT + AEC | `ds-stt`, `ds-aec` | clean |
| Core + model | `ds-core`, `ds-model`, `ds-engines`, `ds-proc` | clean |
| Config + platform | `ds-config`, `ds-platform`, `ds-tools`, `ds-i18n` | clean |

**14 confirmed findings.** Cross-check downgraded every one to **low** except the SwiftUI
~1 Hz idle re-publish, which stands at **medium**. Nothing critical or high. The codebase
is structurally healthy: worker threads idle on Condvars (not spins), the warm-helper and
listener loops block on notifications, and no Arc/Rc cycles or leaked tokio tasks were
found in the Rust engine.

---

## Approach validation (push vs poll) — mid-2026 research

The fixes were sanity-checked against current platform guidance, specifically "can the poll
be replaced by a push/subscription?":

- **Permission checks (Accessibility trust, mic authorization) — polling is correct; no push
  API exists.** Apple provides **no** notification/KVO for `AXIsProcessTrusted` changes
  ([confirmed](https://developer.apple.com/forums/thread/735204)) and none for
  `AVCaptureDevice.authorizationStatus`. Polling is the documented workaround, so the #13
  poll-on-change-at-3 s approach is the right shape and can't be made event-driven.
- **Mic *in-use* state (#6 barge watcher, #8 worker hold) — a real push alternative exists.**
  CoreAudio's `kAudioDevicePropertyDeviceIsRunningSomewhere` property listener fires exactly
  when any app starts/stops the input device, so the 150 ms / 120 ms `mic_active()` polls
  *could* be replaced by an event-driven listener. This is a worthwhile follow-up (see below)
  beyond the conservative gating already applied.
- **Config file watch (#5) — a push alternative exists.** The `notify` crate (FSEvents on
  macOS) fires on actual change with low overhead, and would replace the throttled `stat()`
  entirely. Throttling was applied now as the low-risk step; FSEvents is the better long-term
  shape.
- **Swift status stream (#12) — already push-based** (blocks in `WaitModelStatus`, engine
  bumps a sequence). The fix only suppresses the 1 s safety-timeout's redundant wake-ups; the
  model is already correct.

## Fixed in this pass (13)

All changes build clean (`cargo build` workspace-wide, `swift build`) and the full test
suites pass (`dontspeakd` 72, `ds-tts` 67 + 6, `ds-platform` 9, `dontspeak` 71).
Of the 14 confirmed findings, 13 are fixed; only #11 is left (intentionally — see below).

| # | Severity | Area | File | Fix |
|---|----------|------|------|-----|
| 12 | medium | swift | `DontSpeakCore.swift` (status producer) | Yield a status snapshot only when the engine's gate sequence advances **or `engineRunning` flips** (the latter is an external pidfile/launchd probe NOT carried in the seq) — the blocking wait returns every ~1 s on timeout with an unchanged seq, and re-applying it churned every `@Observable` reader ~1×/s forever while idle. |
| 7 | low | daemon | `protocol.rs`, `ipc.rs`, `ttsq.rs`, `hook_narrate.rs` | Added a dedicated `SessionEnd` IPC request (distinct from the shared mid-session `StopSpeech`); on it the daemon barges the window *and* evicts the session's `pool_assignments` + `session_voice` entries, so those maps no longer grow one entry per session until restart. The MCP `stop` tool's `StopSpeech` path is untouched, preserving per-session voice stability. |
| 4 | low | daemon | `engine.rs` (`tick`) | Skip the `NSWorkspace` frontmost probe (~33×/s) when `pause_in_background` is off — it is the sole consumer; publish `true` so the focus gate is unaffected. |
| 5 | low | daemon | `boot.rs` (poll loop) | Throttle the `settings.json` `stat()` from every 30 ms tick to every 500 ms (`MTIME_CHECK_INTERVAL`). SIGHUP/Reload-RPC path unchanged. |
| 6 | low | daemon | `barge.rs` (watcher) | Skip the CoreAudio `mic_active()` device query in full-duplex, where `barge_step` discards it anyway. Behaviour-identical. |
| 9 | low | daemon | `boot.rs` (signal watcher) | Raise the signal-flag watcher poll from 30 ms to 250 ms — it only propagates rare SIGHUP/SIGTERM flips. |
| 10 | low | mcp | `hook_narrate.rs` (`barge_session`) | On SessionEnd, delete the session's `narrate-display-<session>.json` (+ `.lock`/`.tmp`). They were created per session and never removed → unbounded disk growth. |
| 13 | low | swift | `DontSpeakCore.swift` (`permsTask`) | Assign `perms` only on change and slow the poll 1.5 s → 3 s. `refresh()` already covers the return-from-Settings path. |
| 14 | low | swift | `TrayAnimator.swift` | Make the `core` back-reference `unowned` to break the Core ↔ TrayAnimator retain cycle (benign today since Core is the app-lifetime singleton, but a real leak if Core is ever recreated). |
| 8 | low | daemon | `ttsq.rs`, `barge.rs`, `boot.rs` | Worker focus-hold no longer queries the audio device every 120 ms while holding — it reads the shared `MicWatcher`'s cached state (one watcher now feeds both the worker-hold and the barge watcher; see follow-up #1). The hold loop still ticks (its self-heal bound is tick-based) but the per-iteration syscall is gone. |
| 1 | low | tts | `synth.rs` | Store voices as `HashMap<String, Arc<Vec<f32>>>` so `synthesize()` clones a pointer, not the ~522 KB style array, once per streaming batch. |
| 2 | low | tts | `g2p.rs`, `serve.rs` | Probe `espeak_available()` ONCE per utterance (gated on `needs_espeak`, skipped for English) and thread it into a new `phonemize_for_with`, instead of re-spawning `espeak-ng --version` per text chunk. |

(Numbering matches the working tracker; #6 in the table is the barge gate.)

---

## Reported, not auto-fixed (1) — rationale

| # | Severity | Area | File | Why deferred |
|---|----------|------|------|--------------|
| 11 | low | ipc | `server.rs` (accept loop) | Detached thread per connection, never joined. **Cross-check concluded acceptable as-is:** the 120 s read timeout makes threads self-reaping and serve() never returns, so the OS reaps at exit. Optional hardening (a short handshake timeout + bounded worker pool) was deliberately left out — shortening the read timeout risks cutting off a legitimately slow/persistent client for a non-issue. |

## Noted as design choice (1)

| # | Area | File | Note |
|---|------|------|------|
| 3 | tts | `listen.rs` (live preview) | Live-preview re-transcribes the open tail (bounded to ~7 s by the VAD force-split) every 180 ms during dictation. Heavy repeated model work, but a **documented, intentional latency tradeoff** and properly bounded/deduped. Only revisit if sustained-CPU complaints appear (cap the re-pass to the last N seconds, or stream a partial decode). |

## Cross-check rejections (transparency)

Two candidate findings were **refuted** by cross-check and are *not* defects: (a) the
mic-barge / TtsQueue worker threads being "orphaned on `engine_run` return" — no in-process
restart path exists, so the OS reaps them at process exit and nothing accumulates; (b) the
narration-hook file-lock "spin-wait" — it is `sleep(2 ms)`-throttled and bounded, not a hot
spin.

## Post-implementation correctness audit

All of today's changes were independently re-reviewed (adversarial, per area). The unsafe
CoreAudio FFI (`mic_watch.rs`) was found **sound** — the `start()` error path frees the
context only when no listener is registered, the leak-on-drop genuinely prevents a
use-after-free from an in-flight callback, the `Send`/`Sync` impls are justified (all `Ctx`
access is via atomics / an immutable `Fn`), and the callback signature matches the
`AudioObjectPropertyListenerProc` typedef exactly. The SessionEnd eviction, key consistency,
TTS (`Arc` voices, per-utterance espeak), and `unowned` TrayAnimator all verified correct.

Two issues were caught and fixed:
- **#12 (regression):** the status producer's seq-only dedup suppressed the engine **down**
  transition — `engineRunning` is an external pidfile probe, not in the gate seq, so a
  stop/crash froze the seq and left the menu-bar dot stale "running". Fixed by also yielding
  when `engineRunning` flips.
- **#5 (wart):** a plain settings.json edit fired **two** reloads ~0–3 s apart (push watcher
  set `reload_requested`, the hup reload didn't advance `last_seen`, then the stat backstop
  re-fired). Fixed by statting once at reload time on a hup-only reload.

## Follow-ups — push conversions

Validated against mid-2026 platform guidance before implementing. Key finding: macOS exposes
**no** notification API for permission state (`AXIsProcessTrusted`, `AVCaptureDevice`
authorization), so those polls (#13, daemon AX) are correct as-is and cannot be made
event-driven. Mic *in-use* state and the config file watch **can** be push, and now are:

1. ✅ **Event-driven mic state** (`ds-platform::MicWatcher`) — native CoreAudio
   `kAudioDevicePropertyDeviceIsRunningSomewhere` property listener on macOS (zero polling,
   re-registered on default-device change), centralized poll thread on Windows/Linux. The
   barge watcher now reads the cached state instead of querying the device each tick.
   Implementation note: uses the **function-pointer** listener API, not the block API —
   `AudioObjectRemovePropertyListenerBlock` is known-unreliable and clean drop-time removal is
   required. The macOS listener is compile-verified and warrants a quick on-device check.
   (The #8 worker-hold loop can adopt the same watcher next; left as-is for now.)
2. ✅ **Push config watch** (`dontspeakd::config_watch`) — `notify` (FSEvents / inotify /
   ReadDirectoryChangesW) on `settings.json`; the `stat()` is now only a 3 s coarse backstop.
3. **Runtime confirmation pass** on a real build: Instruments (Time Profiler + Energy Log) for
   the Swift idle path, `tokio-console`/`sample` for the daemon poll loops, to quantify the
   wake-up reduction; and an on-device check of the CoreAudio mic listener (mic start/stop and
   default-device switch).
