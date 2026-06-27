# Per-terminal narration: focus-gated TTS (single queue, session-tagged)

Status: SHIPPED (Phases 1–4 implemented + deployed). The `UserPromptSubmit` →
mark-active signal rides the `dontspeak notify` hook, which routes on `hook_event_name`
to a `MarkActive` RPC.

> Renamed in spirit from "per-terminal *queues*": the adversarial review (see §10)
> showed per-session **queues** add lifecycle/GC cost to solve a problem we don't have
> (fair interleave of two simultaneously-talking terminals — there's one audio output,
> only one plays anyway). The simpler, equivalent design is **one queue, items tagged by
> session, worker plays only the active session's items.** Same user-visible behavior,
> ~no new machinery.

## 1. What the user wants
- Tab away from a terminal → its narration **pauses**, doesn't drop; tab back → **resumes
  at the latest** (no stale backlog).
- Works whether you tab to a **browser** (no terminal frontmost) or to **another
  terminal**.
- **Cross-platform** (macOS/Windows/Linux), not an AppleScript/Terminal.app special case.

## 2. The focus signal (load-bearing — see §3 research)
There is **no portable session-level focus API**. So we use two cheap, portable layers,
both published by the engine **poll thread** into atomics the TTS **worker thread** reads
(the worker can't call NSWorkspace — it's poll/main-thread affine):

1. **Is *a* terminal frontmost** — `Platform::terminal_frontmost` (macOS `NSWorkspace`,
   Windows `GetForegroundWindow`+exe, Linux X11/Wayland). Coarse but portable. → pause
   **all** when you're in a browser/other app.
2. **Which session is active** — **the session you last submitted a prompt to**
   (`UserPromptSubmit` hook → `MarkActive { session }` RPC). NOT audio-production recency
   (that lets a background agent steal your voice — §10 P3). → among terminals, play only
   the active session's items; hold the rest.

### ⚠️ Caveat: which window you SEE ≠ which session you HEAR (focus vs active-session)

This is a known, accepted limitation — write it on your hand so it stops surprising you:

- The two signals are **independent**. Layer 1 only knows "is *a* terminal frontmost?",
  **not which one**. Layer 2 ("which session plays") is **the last session you submitted a
  prompt to** — *not* the window you bring to the foreground.
- **Concrete consequence:** terminals A and B both have speech queued and both are
  backgrounded. You click **A** to the front to listen. You may hear **B** — because B was
  the session you last prompted (or, if the `UserPromptSubmit`→`mark-active` hook isn't
  wired, simply the most-recent producer). Foregrounding A does **not** switch the voice to A.
- **Why:** there is no cheap, portable "which tab/window is focused → which Claude session"
  mapping. NSWorkspace is app-level (can't tell two tabs/windows of the same terminal app
  apart); only *distinct terminal apps* are distinguishable cheaply. Same-tab/window focus
  needs AppleScript/Accessibility (deliberately avoided per-tick).
- **To make focus drive playback you'd need either:** (a) the `mark-active` hook wired so
  "active" at least tracks the window you last *typed* in; or (b) "model B" — a focus→session
  resolver (lazy AppleScript/AX for same-app tabs), which is the only thing that makes
  *clicking* a window switch the voice to it. Neither is implemented; "active" = last-prompted.

(Unrelated knob: `pause_in_background` (config, default false) only controls whether playback
*pauses at all* while no terminal is frontmost — it does **not** change *which* session plays.)

## 3. One queue, one kind (no reply/narration asymmetry)
Speech is a SINGLE FIFO with no "reply vs narration" kind and no cap. Whatever the
`narrate` setting enqueues is played in order and treated identically — pause/resume,
barge, focus-hold all apply the same to every item, and nothing is ever dropped to a cap.
(The old design tagged items Reply vs Narration and capped narration with `NARRATION_MAX`;
that split is gone — it had also caused interrupted *narration* to be dropped on a
record-barge instead of resumed.)

## 4. Design (single queue, session-tagged)
The queue tags each `Item` only with `session: Option<String>` (no `kind`). Two
poll-thread-published atomics gate the worker:

```
worker may play the dequeued item  ⇔
    !(half_duplex==false && mic_active)        // existing mic gate
 && !(terminal_seen && !terminal_front)        // §2.1  (Phase 1 — DONE)
 && item.session == active_session             // §2.2  (Phase 2)
```
A held item is **not dropped** — the worker holds it (sleep-poll, breaks on a generation
bump) and plays it when the gate clears. Everything eventually plays; there is no cap.

## 5. Phase status — ALL DONE
1. **DONE — `terminal_frontmost` pause-all gate.** Poll thread publishes
   `set_terminal_front` each tick; worker holds while no terminal is frontmost.
   **Self-arming** (`terminal_seen`): the gate only engages after a terminal has been
   seen once, so an unrecognized emulator degrades to always-play, never mute.
   Files: `ttsq.rs` (atomics + `set_terminal_front` + worker hold), `lib.rs` (`tick`).
2. **DONE — active-session selection.** `TtsQueue.active: Mutex<ActiveSel>`
   (`explicit` from the prompt-hook, `recent` fallback). Worker dequeues via
   `select_pos` — the active session's items (+ untagged globals); other sessions are
   held in place (never dropped, no cap). Lock order `items` → `active`;
   `set_active_session` takes `items` to avoid lost wakeups. Files: `ttsq.rs`.
3. **DONE — active-session signal.** `MarkActive { session }` RPC (`ds-ipc`) →
   engine handler (`lib.rs`). The `UserPromptSubmit` `notify` hook (in
   `canonical_hook_groups`, `ds-config`) routes on `hook_event_name` → `MarkActive`.
   **Needs a one-time hook re-wire** on the user's machine (installer / `wire_client`).
4. **DONE — AppleScript `terminal_focused` deleted.** The `MessageDisplay` `notify` hook
   forwards narration tagged by session (mic-suppression kept); `terminal_focused` +
   `resolve_tty` + the osascript call removed entirely.

## 5a. Deploy checklist
- `apps/macos/bundle.sh` → rebuilds engine binaries (incl. new `dontspeak`) +
  staticlib + re-signs `~/Applications/DontSpeak.app`.
- Re-wire hooks so `UserPromptSubmit` → `mark-active` lands in `~/.claude/settings.json`
  (installer or the `wire_client` MCP tool). Without it, the engine falls back to
  `recent` (recency) — no interleave, just less precise on multi-terminal.
- By-ear test: two terminals; narrate in both; prompt in A then B → voice follows;
  tab to a browser → all pause; tab back → resume. Confirm single-terminal unchanged.

## 6. The narrate-drop relocation (important)
The old narrate hook computed `gate_on = !mic_active() && terminal_focused(my_tty)` and
**returned early** (dropped) when false — using AppleScript, Terminal.app-only. Phase 2
**moved this policy into the engine**: the hook always forwards narration (tagged by
session); the engine decides play/hold via `active_session` + `terminal_front`. Net: the
focus decision is portable and lossless, and the AppleScript path is deleted (Phase 4).

## 7. Concurrency (corrected from review §10 P1/P2)
`TtsQueue` is a bag of independent locks/atomics across **four** threads (IPC server,
worker, mic-barge watcher, engine poll). `terminal_front`/`terminal_seen`/`active_session`
are **new cross-thread atomics**, written by the poll thread (and the IPC handler for
`MarkActive`), read by the worker — same shape as the existing `mic_active` gate, NOT
"behind one Mutex." `terminal_frontmost` is sampled **only** on the poll thread (macOS
main-thread affinity); the worker reads the published atomic.

## 8. Lifecycle
With a single queue there is **no per-session map, no GC, no `DropSession` RPC, no
leaked-session handling** — items drain themselves. `active_session` falls back to
**None → silence** (not "most-recent session", which would resurrect the steal bug) when
its session goes quiet; the next `MarkActive` repoints it.

## 9. Tests
- Unit: worker play-predicate truth table over (terminal_seen, terminal_front,
  mic_active, full_duplex, session==active). Phase 1: `terminal_seen && !terminal_front`
  holds; unseen never holds.
- Integration (by ear): two terminals narrating; tab between → pause/resume-at-latest;
  tab to a browser → all pause; tab back → resume. Verify the unrecognized-emulator
  fail-open (never mute).

## 10. Review findings folded in (2026-06-21 adversarial audit)
- **P1/P2** concurrency: there is no single shared Mutex; `terminal_frontmost` can't be
  called from the worker → poll-thread-published atomics. **Applied.**
- **P3** recency steals the voice → active = last-prompted, not audio-production.
  **Applied** (Phase 3 hook).
- **P4** reply vs narration asymmetry → **removed**: one queue, one kind, no cap;
  pause/resume/barge treat every item identically.
- **P8** per-session queues are over-engineered → single queue + session-tag filter.
  **Applied** (whole redesign).
- Fact-check: there is no skip-ahead / cap at all (the old `NARRATION_MAX` is gone);
  `GreetSession`/`Speak`/`SpeakNarration` carry session; `UserPromptSubmit`/`MarkActive`/
  `DropSession` do not exist yet. **Corrected in text.**
