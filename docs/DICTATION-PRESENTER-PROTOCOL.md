# Dictation presenter protocol

## Goal

Keep dictation behavior in the Don’t Speak engine while allowing more than one
UI to render the same live transcript. The native overlay remains the guaranteed
fallback. A terminal presenter may temporarily take over rendering, but it never
receives microphone, paste, Accessibility, Keychain, or other privileged
capabilities.

This is a presenter protocol precursor, not a daemon extraction. It reuses the
existing engine socket, model-status sequence, and native host polling.

## Ownership

The engine owns:

- the dictation session identity and lifecycle;
- recording, partial, final, refused, and hidden state;
- the transcript and paste-target availability;
- Caps Lock gestures, cancel, insert-only, and insert-plus-Enter behavior; and
- selection of the active presenter with native fallback.

Each presenter owns only rendering. Native hosts read model status as before. An
external presenter must acquire a session-scoped lease, render an initial
snapshot, mark the lease ready, renew it while visible, and release it on close.

## Wire contract

Every visible `dictation` status includes an opaque `session_id`. It is stable
from recording start through final confirmation and absent while hidden.
`ModelStatus.seq` orders snapshots and updates.

The engine embeds an internal monotonic presentation generation inside the
opaque identifier. Callers must treat the whole identifier as opaque. The
registry uses only that authenticated local generation to distinguish a real
new turn from a delayed observation of an older turn: a stale acquire, ready, or
renew request can never revoke a newer live lease.

The engine socket provides four external-presenter operations:

1. **Acquire** takes a presenter identifier, the current `session_id`, and a
   bounded TTL. It returns an opaque `lease_id`. Acquiring does not hide the
   native overlay.
2. **Ready** takes the `lease_id` and `session_id` after the presenter has
   rendered its first snapshot. Only then does `external_ui_active` become true.
3. **Renew** extends the same ready, scoped lease. A reservation that never
   renders cannot be kept alive, and a token cannot be reused for a later
   dictation session.
4. **Release** explicitly relinquishes the lease.

Because renewal is available only after Ready, the bounded TTL supplied to
Acquire is also the presenter's initial render budget.

Unknown, stale, expired, mismatched, or superseded lease tokens fail closed.
Only one external presenter can own a session. A second acquisition cannot
replace a live lease, even when it reports the same presenter identifier.

Lease identifiers are local UI-routing credentials only. Presenter identity is
self-asserted; the trust boundary is access to the local engine socket, not the
`presenter_id` string. A live local holder can renew a ready lease indefinitely,
so clients with socket access are trusted to render while ready and release on
ineligibility. No operation accepts text to paste, requests key injection, opens
the microphone, or exposes a host capability token.

## Failure semantics

The native overlay is hidden only while a ready lease is live and matches the
current dictation session. It becomes the presenter again when:

- the external presenter releases;
- renewal stops and the lease expires;
- the presenter process disconnects or exits, causing renewal to stop;
- the dictation session changes; or
- the external presenter never reaches ready.

A session end deactivates the lease immediately; expiry or a later monotonic
session generation clears it without allowing delayed observations to remove a
newer lease. Ready-state removal bumps the existing status gate so native hosts
refresh. A crashed or disconnected presenter can therefore hide the native
overlay for at most the bounded TTL plus one native status-wait interval.

## Update transport

The first vertical slice uses the existing `WaitModelStatus` sequence gate. It
returns a complete initial snapshot, then blocks until `seq` changes before
returning the next complete snapshot. This is server-pushed wakeup over bounded
long polling: it has ordered resynchronization and no idle polling loop, while
avoiding a second event protocol before the service/client split is designed.

A future persistent subscription may reduce process and connection churn, but
it must preserve the same initial-snapshot, monotonic-sequence, reconnect, and
lease semantics. Long polling remains a compatibility transport, not UI
ownership.

## Terminal presenter flow

The long-lived plugin bridge watches status only to decide when to open a popup.
It does not acquire or renew UI ownership.
The text-only popup is used for recording and awaiting-confirm states; the
native host retains refusal cues that carry non-text visual meaning.

The popup process:

1. verifies that its host UI explicitly reports itself foreground;
2. reads the current visible snapshot and session;
3. acquires a lease for that session;
4. renders the snapshot and flushes its terminal output;
5. rechecks foreground eligibility and marks the lease ready;
6. rechecks eligibility before every renewal while consuming ordered status
   updates; and
7. releases on focus loss or normal shutdown, with TTL expiry as the crash
   fallback.

Missing or false host-focus state fails closed: the external presenter does not
take over and the native overlay remains visible. Focus reporting is a
correctness eligibility signal, not a new capability. A presenter cannot gain
microphone, paste, key-injection, or Accessibility access by reporting focus.

When state becomes hidden, the popup exits. Don’t Speak continues to perform the
original paste and Enter behavior; the plugin never inserts text itself.

## Compatibility and rollout

- Native-only use is unchanged because no external lease exists.
- Existing Caps Lock gestures and paste behavior remain engine-owned.
- The old receiver-on-status shortcut is removed from this pre-release feature
  branch; callers move to the explicit presenter lifecycle.
- Native host DTOs gain the optional session identifier and
  `external_ui_active` presenter-selection flag.
- The terminal plugin is updated after the engine contract, then both sides are
  tested together. Older plugins simply fail to acquire a lease and the native
  overlay remains visible.
