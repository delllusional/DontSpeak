# Per-terminal narration: focus-gated TTS (single queue, session-tagged)

Status: SHIPPED. Focus-gated per-terminal narration is a **single, session-tagged
queue**: the worker plays only the **active session's** items and pauses **all** when no
terminal is frontmost. The active session is set by a `MarkActive` RPC carried on the
`UserPromptSubmit` hook (via `dontspeak notify`, routed on `hook_event_name`). There are
no per-session queues — items are tagged by session and filtered, same user-visible
behavior with no per-session lifecycle/GC machinery.

## 1. What the user wants
- Tab away from a terminal → its narration **pauses**, doesn't drop; tab back → **resumes
  at the latest** (no stale backlog).
- Works whether you tab to a **browser** (no terminal frontmost) or to **another
  terminal**.
- **Cross-platform** (macOS/Windows/Linux), not an AppleScript/Terminal.app special case.

## 2. The focus signal (load-bearing)
There is **no portable session-level focus API**. So we use two cheap, portable layers,
both published by the engine **poll thread** into atomics the TTS **worker thread** reads
(the worker can't call NSWorkspace — it's poll/main-thread affine):

1. **Is *a* terminal frontmost** — `Platform::terminal_frontmost` (macOS `NSWorkspace`,
   Windows `GetForegroundWindow`+exe, Linux X11/Wayland). Coarse but portable. → pause
   **all** when you're in a browser/other app.
2. **Which session is active** — **the session you last submitted a prompt to**
   (`UserPromptSubmit` hook → `MarkActive { session }` RPC). NOT audio-production recency
   (that lets a background agent steal your voice). → among terminals, play only
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

## 3. One queue, one kind (no cap)
Speech is a SINGLE FIFO with no "reply vs narration" kind and no cap. Whatever the
`narrate` setting enqueues is played in order and treated identically — pause/resume,
barge, focus-hold all apply the same to every item, and nothing is ever dropped to a cap.
Each `Item` is tagged only with `session: Option<String>`; the worker holds (sleep-poll,
breaks on a generation bump) any item whose session isn't active or while no terminal is
frontmost, and plays it once the gate clears. The active session falls back to **None →
silence** when its session goes quiet; the next `MarkActive` repoints it.
