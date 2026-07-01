//! The `notify`-side narration handlers — speak meaningful assistant prose as Claude
//! *works*. Dispatched from [`crate::hook_core::notify`].
//!
//! [`message_display`] (`MessageDisplay`): runs once per streaming batch. Accumulates the
//! `delta` chunk per `message_id`, and — when `narrate` contains "digests" — speaks EVERY
//! top-level blockquote Claude writes (the dedicated spoken lines, read VERBATIM, each once,
//! in document order). When `narrate` contains "shorts", a SHORT plain message with NO
//! blockquote (no code fence / path / URL) is instead voiced whole, lightly cleaned. Fast +
//! fire-and-forget so it never delays the display.
//!
//! [`speak_reply`] (`Stop`): the non-streaming analogue for OpenAI Codex — voices the whole
//! final reply, guarded so it never double-speaks what `MessageDisplay` already streamed.
//! [`mark_streaming_session`] (`SessionStart`) seeds that guard's witness.
//!
//! [`barge_session`] (`SessionEnd`): barge THIS session's engine playback (a scoped
//! `StopSpeech{session}`) so closing a window silences its OWN reply, not another's. No
//! payload → `None` session → the global barge.
//!
//! Settings (ds-config VoiceConfig): `narrate` is a SET of "digests"/"shorts".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ds_config::{NarrateKind, Paths, VoiceConfig};
use serde::Deserialize;

/// SessionEnd notify: barge ONLY this session's engine playback (so closing one window
/// never silences another's reply). The `payload` is the hook JSON; no payload / no
/// session id → `None` → the global barge.
pub fn barge_session(paths: &Paths, payload: &str) {
    let session = session_id_of(payload).filter(|s| !s.trim().is_empty());
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::SessionEnd {
            session: session.clone(),
        },
    );
    // SessionEnd is terminal for this session (this path fires ONLY on SessionEnd — a
    // mid-session barge uses `StopSpeech`), so the engine reclaims this session's voice
    // maps, and here we reclaim its per-session display-state file and its lock/tmp
    // siblings. Without this they accumulate one `narrate-display-<session>.json` per
    // distinct session in the data dir forever.
    if let Some(s) = &session {
        let path = display_state_path(paths, s);
        let _ = std::fs::remove_file(path.with_extension("lock"));
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        let _ = std::fs::remove_file(&path);
    }
}

/// SessionStart notify (the streaming-witness seed): pre-create THIS session's MessageDisplay
/// state file so [`speak_reply`]'s `streamed` guard is reliably true before the first `Stop`,
/// closing the only timing gap in the double-narration fix. The discriminator is the event
/// wiring itself: Claude Code wires `SessionStart`, so this seeds the witness at session open;
/// OpenAI Codex wires NEITHER SessionStart nor MessageDisplay (its set is `UserPromptSubmit` +
/// `Stop` only — see `wire/codex.rs`), so a Codex session never seeds it and its `Stop` still
/// narrates. Idempotent + non-destructive: never clobbers real in-progress state (a re-fired
/// SessionStart is a no-op), and the seeded default reads exactly like "no file yet" (fresh
/// `Accum`), so streaming is unaffected.
pub fn mark_streaming_session(paths: &Paths, payload: &str) {
    let Some(session) = session_id_of(payload).filter(|s| !s.trim().is_empty()) else {
        return; // no session id ⇒ can't scope a witness (the per-batch write still covers it)
    };
    let path = display_state_path(paths, &session);
    if path.exists() {
        return; // don't overwrite real state from a prior turn / resumed session
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    atomic_write(
        &path,
        &serde_json::to_string(&DisplayState::default()).unwrap_or_default(),
    );
}

/// Pull the `session_id` out of any session-scoped hook payload (SessionStart / SessionEnd).
/// `None` on absent/unparseable — every caller treats that as "unscoped".
fn session_id_of(payload: &str) -> Option<String> {
    serde_json::from_str::<SessionHook>(payload.trim())
        .ok()
        .and_then(|h| h.session_id)
}

/// Session-scoped hook payload (subset): the Claude session id, shared by the SessionStart
/// witness seed and the SessionEnd barge.
#[derive(Debug, Deserialize, Default)]
struct SessionHook {
    #[serde(default)]
    session_id: Option<String>,
}

// ── Stop hook (speak the FINAL reply — OpenAI Codex) ─────────────────────────────

/// Stop hook payload (subset): the final assistant text + the session id. BOTH Claude Code
/// and OpenAI Codex deliver `last_assistant_message` on the `Stop` event (this was once
/// assumed CC-empty — it is NOT), so the field alone can't tell the clients apart; the
/// streaming witness in [`speak_reply`] does that instead.
#[derive(Debug, Deserialize, Default)]
struct StopHook {
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Witness that a `MessageDisplay` streaming pass ran for this session: its per-session state
/// file (which [`message_display`] writes on EVERY batch) exists. The deterministic CC-vs-Codex
/// discriminator the `Stop` path needs (see [`speak_reply`]):
///   • Claude Code wires `MessageDisplay` and streams every turn, so the file is present when
///     `Stop` fires ⇒ the reply was ALREADY narrated; `Stop` must not re-speak it.
///   • OpenAI Codex wires NO `MessageDisplay` hook (see `wire/codex.rs` `CODEX_HOOKS`), so the
///     file is NEVER written ⇒ `streamed = false`, and `Stop` is Codex's only narration path.
/// [`mark_streaming_session`] also SEEDS this file at `SessionStart` (which Codex doesn't wire),
/// so the witness is present from session open — closing the timing edge of a `Stop` racing the
/// first batch's write, while its absence for Codex keeps that case correct too.
fn streamed_via_message_display(paths: &Paths, session: &str) -> bool {
    display_state_path(paths, session).exists()
}

/// Stop notify: speak the FINAL assistant reply, once — the NON-STREAMING analogue of
/// [`message_display`] for OpenAI Codex, whose hooks fire only at end-of-turn with the whole
/// `last_assistant_message` and no `MessageDisplay` stream. Claude Code ALSO wires `Stop` and
/// delivers `last_assistant_message` on it, so without a guard we'd re-voice every reply
/// MessageDisplay already streamed (heard twice). Guard: [`streamed_via_message_display`] — a
/// session with a MessageDisplay state file already narrated ⇒ stay silent. Pure decision in
/// [`stop_utterances`]; this is the IO wrapper (config load, mic probe, witness, engine send).
pub fn speak_reply(paths: &Paths, payload: &str) {
    let cfg = VoiceConfig::load(paths);
    let messages_on = cfg.narrates(NarrateKind::Digests);
    let short_on = cfg.narrates(NarrateKind::Shorts);
    if !messages_on && !short_on {
        return; // narration off ⇒ stay silent (skip parsing + the witness stat)
    }
    let Ok(hook) = serde_json::from_str::<StopHook>(payload.trim()) else {
        return;
    };
    let session = hook.session_id.clone().filter(|s| !s.trim().is_empty());
    let streamed = streamed_via_message_display(paths, session.as_deref().unwrap_or_default());

    let speak = stop_utterances(
        hook.last_assistant_message.as_deref(),
        messages_on,
        short_on,
        ds_platform::mic_active(),
        streamed,
    );
    for line in speak {
        let _ = ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text: line,
                session: session.clone(),
            },
        );
    }
}

/// Decide what the `Stop` hook should voice, PURELY (no IO) so it is exhaustively
/// unit-testable — the seam the double-narration regression tests drive. Returns the
/// blockquote / short utterances to speak, in order, or EMPTY when `Stop` must stay silent:
///   • narration off (`!messages_on && !short_on`),
///   • mid-dictation (`mic_active` — don't talk over the user, mirrors the MessageDisplay gate),
///   • `streamed` — a MessageDisplay pass already narrated this turn (Claude Code); re-voicing
///     here is the double-narration bug, so we suppress it,
///   • no usable final text.
/// Otherwise the whole reply is fed through a fresh `Accum` as ONE final batch, yielding the
/// exact runs the streaming path would emit (every top-level blockquote in order; or, under
/// `short`, a brief blockquote-less reply whole) — so a Codex reply is voiced just like a
/// Claude Code one.
fn stop_utterances(
    last_assistant_message: Option<&str>,
    messages_on: bool,
    short_on: bool,
    mic_active: bool,
    streamed: bool,
) -> Vec<String> {
    if !messages_on && !short_on {
        return Vec::new();
    }
    if mic_active {
        return Vec::new();
    }
    if streamed {
        return Vec::new(); // MessageDisplay already narrated this session ⇒ never double-speak
    }
    let Some(text) = last_assistant_message
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    crate::narrate::Accum::default().feed(0, text, None, true, messages_on, short_on)
}

// ── MessageDisplay hook (speak-as-it-streams) ───────────────────────────────────

/// The MessageDisplay hook payload (Claude Code ≥ 2.1.x). The hook fires repeatedly
/// while a message streams: CC 2.1.183 sends an incremental `delta` chunk per batch
/// (we accumulate it per `message_id` into the cumulative text), while some versions
/// are documented to send a cumulative `displayedText` instead — we accept either and
/// emit each top-level blockquote run as it completes.
#[derive(Debug, Deserialize, Default, Clone)]
struct MessageDisplayHook {
    // Forward-compat: some CC versions are documented to send the CUMULATIVE text.
    // 2.1.183 does NOT — it streams `delta` only (verified against a live payload).
    #[serde(default, rename = "displayedText")]
    displayed_text: Option<String>,
    // The incremental text chunk for THIS streaming batch (what CC actually sends).
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    // Stable per-message id — the new-message KEY (replacing the old first-48-chars
    // fingerprint). NOT used for ordering; that's `index`.
    #[serde(default)]
    message_id: Option<String>,
    // Content-block index within the message. NOT a key (keying on it would split one
    // message into many false "new messages"), but ESSENTIAL for ORDER: Claude Code spawns
    // a process per batch and they race, so they can reach us out of order. We store each
    // delta keyed by its index and reconstruct the cumulative text in index order, making
    // accumulation independent of arrival order.
    #[serde(default)]
    index: Option<u64>,
    // True on the last batch of a message → the final blockquote run counts as complete
    // even with no terminating blank line after it.
    #[serde(default, rename = "final")]
    is_final: Option<bool>,
}

/// Per-session state for the MessageDisplay diff: how many blockquote utterances of the
/// current message we've already spoken (`offset` = spoken count), plus a short key to
/// detect when a NEW message starts (cumulative text resets).
#[derive(Debug, Deserialize, serde::Serialize, Default, Clone, PartialEq)]
struct DisplayState {
    /// Count of this message's top-level blockquotes already voiced. Each batch speaks any
    /// newly-completed run beyond this count and advances it; a new message resets it to 0.
    offset: usize,
    key: String,
    // Delta mode: each batch's chunk keyed by its content-block `index`, so the cumulative
    // text reconstructs in INDEX order regardless of the order the racing batch-processes
    // reached us. Empty in cumulative (`displayedText`) mode.
    #[serde(default)]
    parts: BTreeMap<u64, String>,
    // Sticky "a batch with final=true has been seen" — the terminating flag must survive
    // even when that batch is processed BEFORE the one carrying the blockquote (out of order).
    #[serde(default)]
    seen_final: bool,
    // Sticky latch for the "shorts" fallback (a blockquote-less final reply voiced whole, once)
    // — maps to `Accum::short_done`, so a late duplicate batch never re-speaks it.
    #[serde(default)]
    short_done: bool,
    // The mic gate is decided ONCE per assistant message (keyed by `message_id`) and
    // cached here, so a mid-stream mic flap can't strand the tail of a message we
    // already started narrating — nor start one we decided to skip. `gate_msg` is the
    // message_id the decision belongs to; `gate_on` is whether it narrates.
    #[serde(default)]
    gate_msg: String,
    #[serde(default)]
    gate_on: bool,
}

/// First ~48 chars of the message — a cheap fingerprint to detect a new message
/// stream (each message's opening text differs), so we reset the offset.
fn message_key(s: &str) -> String {
    s.chars().take(48).collect()
}

fn display_state_path(paths: &Paths, session: &str) -> PathBuf {
    // Sibling of narrate.pid (in the data dir). Session ids are uuid-like; keep only
    // filename-safe chars defensively.
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let dir = paths
        .narrate_pid
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("narrate-display-{safe}.json"))
}

/// MessageDisplay notify: narrate this streamed batch. `payload` is the hook JSON (already
/// read by the transport, not stdin). Accumulate the `delta` chunks (or take `displayedText`
/// if a CC version sends it) into the cumulative text and enqueue each newly-completed
/// blockquote run on the warm engine.
///
/// Gates ONLY on `narrate` (on/off) and not-mid-recording — NOT on focus. Narration is
/// forwarded TAGGED BY SESSION regardless of which app is frontmost; the ENGINE WORKER holds
/// an inactive/backgrounded terminal's items (never dropped here) and plays them when that
/// terminal is active + frontmost. Fast + fire-and-forget so it never delays CC's display.
pub fn message_display(paths: &Paths, payload: &str) {
    let cfg = VoiceConfig::load(paths);
    let messages_on = cfg.narrates(NarrateKind::Digests); // voice the blockquotes Claude writes
    let short_on = cfg.narrates(NarrateKind::Shorts); // voice a short blockquote-less reply whole
    if !messages_on && !short_on {
        return; // narration off for messages ⇒ stay silent
    }
    let Ok(hook) = serde_json::from_str::<MessageDisplayHook>(payload.trim()) else {
        return;
    };
    let session = hook.session_id.clone().unwrap_or_default();

    let state_path = display_state_path(paths, &session);

    // Claude Code spawns a SEPARATE `notify` process per streamed batch, and they all
    // read-modify-write this one per-session state file. When batches arrive fast enough to
    // OVERLAP, those processes race: the file is corrupted, or the accumulated blockquote is
    // clobbered, so the spoken line never finishes assembling and the reply is silently
    // dropped (measured: sequential batches narrate 15/15, overlapping 0/30). Serialize the
    // read-modify-write under a per-session lock so the batches take turns, and write
    // atomically (temp + rename) so a reader never sees a half-written file. The engine
    // forward stays OUTSIDE the lock — no socket round-trip held under the mutex.
    let speak = with_state_lock(&state_path, || {
        let prev: DisplayState = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // The diff/gate/blockquote logic is pure (`step_display`); persist its decision and
        // hand back any blockquote utterances that came ready THIS batch.
        let step = step_display(
            &prev,
            &hook,
            ds_platform::mic_active(),
            messages_on,
            short_on,
        );
        if let Some(next) = step.write {
            atomic_write(
                &state_path,
                &serde_json::to_string(&next).unwrap_or_default(),
            );
        }
        step.speak
    });
    // Each completed blockquote is forwarded as its OWN narration item, in order — the
    // engine's per-session worker plays them sequentially with a natural pause between, so
    // a multi-point spoken digest is heard point by point rather than in one breath.
    let session = Some(session).filter(|s| !s.is_empty());
    for text in speak {
        let _ = ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text,
                session: session.clone(),
            },
        );
    }
}

/// Serialize the per-session state read-modify-write across the independent processes Claude
/// Code spawns per streamed batch. Without it, overlapping batches race on the state file and
/// the accumulated blockquote is lost → the spoken line is silently dropped. A lock file
/// beside the state file is the mutex: `create_new` is atomic, so exactly one process holds
/// it and the rest spin briefly. Bounded so narration can never wedge (it proceeds without
/// the lock after the ceiling), and a stale lock from a crashed holder is broken by age —
/// batches are sub-second, so a 2 s floor never trips during normal streaming.
fn with_state_lock<T>(state_path: &Path, f: impl FnOnce() -> T) -> T {
    let lock_path = state_path.with_extension("lock");
    const SPIN_TRIES: u32 = 400; // ×2 ms ≈ 800 ms ceiling, then proceed anyway
    const STALE_MS: u128 = 2000;
    let mut held = false;
    for _ in 0..SPIN_TRIES {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => {
                held = true;
                break;
            }
            Err(_) => {
                // Break a stale lock left by a crashed holder, else wait and retry.
                let stale = std::fs::metadata(&lock_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .map(|age| age.as_millis() > STALE_MS)
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    let out = f();
    if held {
        let _ = std::fs::remove_file(&lock_path);
    }
    out
}

/// Write `contents` to `path` atomically: write a sibling temp file, then rename over the
/// target (atomic on the same filesystem), so a concurrent reader never observes a torn or
/// empty file — only the previous or the new complete contents.
fn atomic_write(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, contents).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// One MessageDisplay batch's effect, decided PURELY (no IO) so it is unit-testable —
/// this is the seam the streaming-accumulation regression tests drive. `write = None`
/// means "leave the state file untouched" (a no-op batch); `speak` holds the blockquote
/// utterances that became COMPLETE this batch (one per top-level `>` run), in order, each
/// voiced once. Usually empty or one item; a batch that completes several runs at once
/// (out-of-order delivery, or a body line that terminates the last run) yields several.
struct DisplayStep {
    write: Option<DisplayState>,
    speak: Vec<String>,
}

/// Decide what a single streamed batch does, given the previous per-session state and
/// whether the mic is live. Pure: same inputs → same outputs, no disk/socket/platform.
fn step_display(
    prev: &DisplayState,
    hook: &MessageDisplayHook,
    mic_active: bool,
    messages_on: bool,
    short_on: bool,
) -> DisplayStep {
    let is_final = hook.is_final.unwrap_or(false);

    // Per-MESSAGE mic gate. CC streams a message as many `delta` batches across its
    // content blocks; checking the gate on EACH batch lets a momentary mic blip strand
    // the rest of a message we already began narrating — observed as "only the first
    // sentence spoke." So decide ONCE, when a message first appears (by `message_id`),
    // and cache it: every later batch of the same message inherits that decision.
    //
    // FOCUS is NOT gated here: narration is forwarded TAGGED BY SESSION, and the engine
    // speaks only the ACTIVE terminal's items, holding the rest until they become active
    // (see docs/PER-TERMINAL-QUEUES.md). We still suppress narration sent WHILE the user is
    // recording — no reason to stream fresh chatter into a dictation.
    let msg_id = hook.message_id.clone().unwrap_or_default();
    let gate_on = if !msg_id.is_empty() && prev.gate_msg == msg_id {
        prev.gate_on
    } else {
        !mic_active
    };
    if !gate_on {
        // Remember the skip so later batches of this message skip too (no re-check), while
        // still advancing the new-message key so the NEXT message is detected.
        return DisplayStep {
            write: Some(DisplayState {
                offset: 0,
                key: String::new(),
                parts: BTreeMap::new(),
                seen_final: false,
                short_done: false,
                gate_msg: msg_id,
                gate_on: false,
            }),
            speak: Vec::new(),
        };
    }

    // New-message key: the stable `message_id` ALONE (NOT `#index`). MessageDisplay streams
    // one assistant message as many batches with an incrementing content-block `index`; keying
    // on the index made every batch look like a new message, resetting the accumulator each
    // time — so the leading blockquote (in an early batch, not yet "complete") was wiped before
    // its terminating body line arrived and never got spoken. message_id is per-message, so it
    // alone detects a genuinely new message while letting all batches accumulate. Fall back to a
    // text fingerprint when no id is present (older CC).
    let cur_key = match hook.message_id.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => message_key(
            hook.delta
                .as_deref()
                .or(hook.displayed_text.as_deref())
                .unwrap_or_default(),
        ),
    };
    let same = prev.key == cur_key;

    // Drive the accumulator core (`crate::narrate::Accum`) — the reconstruction +
    // every-blockquote emit logic, kept pure so it is exhaustively unit-testable and a fix
    // lands in one place. The per-session state FILE is this path's cross-process persistence
    // (CC spawns one process per streamed batch), so we hydrate an `Accum` from the prior
    // state (for the same message) or fresh, step it, and write it back.
    // `offset` ⇆ `Accum::emitted` (runs already voiced); `parts`/`seen_final` map 1:1.
    let mut accum = if same {
        crate::narrate::Accum {
            parts: prev.parts.clone(),
            seen_final: prev.seen_final,
            emitted: prev.offset,
            short_done: prev.short_done,
        }
    } else {
        crate::narrate::Accum::default()
    };
    // No `index` (older CC) → append after the highest seen, preserving arrival order.
    let index = hook
        .index
        .unwrap_or_else(|| accum.parts.keys().next_back().map_or(0, |k| k + 1));
    let speak = accum.feed(
        index,
        hook.delta.as_deref().unwrap_or_default(),
        hook.displayed_text.as_deref(),
        is_final,
        messages_on,
        short_on,
    );
    let next = DisplayState {
        offset: accum.emitted,
        key: cur_key.clone(),
        parts: accum.parts,
        seen_final: accum.seen_final,
        short_done: accum.short_done,
        gate_msg: msg_id.clone(),
        gate_on: true,
    };
    DisplayStep {
        write: Some(next),
        speak,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_key_is_first_48_chars() {
        let long = "x".repeat(100);
        assert_eq!(message_key(&long).chars().count(), 48);
        assert_eq!(message_key("short"), "short");
    }

    #[test]
    fn state_lock_serializes_concurrent_read_modify_write() {
        // Reproduces the batch-process race that silently dropped narration: many writers
        // doing read-modify-write on one state file. Under `with_state_lock` every increment
        // must land (final == N); without the lock the widened window loses updates.
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("smnarrate-locktest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let state = Arc::new(dir.join("narrate-display-x.json"));
        atomic_write(&state, "0");
        const N: usize = 24;
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let sp = Arc::clone(&state);
                std::thread::spawn(move || {
                    with_state_lock(&sp, || {
                        let cur: u64 = std::fs::read_to_string(&*sp)
                            .ok()
                            .and_then(|s| s.trim().parse().ok())
                            .unwrap_or(0);
                        // Widen the critical section so an UNLOCKED version would lose updates.
                        std::thread::sleep(Duration::from_millis(1));
                        atomic_write(&sp, &(cur + 1).to_string());
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let final_v: u64 = std::fs::read_to_string(&*state)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            final_v, N as u64,
            "lock must serialize all increments — no lost updates"
        );
    }

    // ── MessageDisplay streaming: the leading blockquote must survive batching ───────
    //
    // CC fires MessageDisplay repeatedly as a message streams. The spoken line (leading
    // blockquote) often lands in an EARLY batch and only becomes "complete" once a later
    // batch adds the body line that terminates it. These tests pin that across-batch
    // accumulation so the regression — keying state by `message_id#index`, which reset the
    // accumulator every batch and silently dropped the blockquote — can't come back.

    /// One delta-mode batch: the incremental chunk CC actually sends (no cumulative text).
    fn delta(id: &str, chunk: &str, is_final: bool) -> MessageDisplayHook {
        MessageDisplayHook {
            delta: Some(chunk.into()),
            message_id: Some(id.into()),
            is_final: Some(is_final),
            ..Default::default()
        }
    }

    /// Feed a sequence of batches through the pure step, threading state as the real hook
    /// would. Auto-assigns a per-message sequential `index` to any batch that lacks one (so
    /// the in-order delta tests read naturally), mirroring how CC numbers content blocks.
    /// Returns the final state and every line that would have been spoken, in order.
    fn drive(batches: &[MessageDisplayHook], mic_active: bool) -> (DisplayState, Vec<String>) {
        use std::collections::HashMap;
        let mut state = DisplayState::default();
        let mut spoken = Vec::new();
        let mut counters: HashMap<String, u64> = HashMap::new();
        for hook in batches {
            let mut hook = hook.clone();
            if hook.index.is_none() {
                let c = counters
                    .entry(hook.message_id.clone().unwrap_or_default())
                    .or_insert(0);
                hook.index = Some(*c);
                *c += 1;
            }
            let step = step_display(&state, &hook, mic_active, true, false);
            if let Some(next) = step.write {
                state = next;
            }
            spoken.extend(step.speak);
        }
        (state, spoken)
    }

    #[test]
    fn out_of_order_batches_still_assemble_and_speak() {
        // The race fix's core: batch-processes can reach us in ANY order. Reconstruction is
        // keyed by `index` and `final` is sticky, so even body-first / quote-last / preamble-
        // middle assembles the right cumulative text and speaks the line exactly once.
        let b = |idx: u64, chunk: &str, fin: bool| MessageDisplayHook {
            delta: Some(chunk.into()),
            message_id: Some("m".into()),
            index: Some(idx),
            is_final: Some(fin),
            ..Default::default()
        };
        // Index order is preamble(0), quote(1), body(2,final); DELIVER them 2, 0, 1.
        let batches = [
            b(2, "\n\nBody after the quote.", true),
            b(0, "Prose preamble first.", false),
            b(1, "\n\n> The spoken line.", false),
        ];
        let (state, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["The spoken line.".to_string()]);
        assert_eq!(state.offset, 1, "spoke exactly once after assembly");
    }

    #[test]
    fn blockquote_split_across_batches_is_spoken_once() {
        // THE regression: blockquote in batch 1, its terminating body line in batch 2.
        // Must speak exactly once, when the body arrives. (Pre-fix: silence.)
        let batches = [
            delta("m1", "> Spoken line here.", false),
            delta("m1", "\n\nNow the body of the reply.", false),
            delta("m1", " More body.", true),
        ];
        let (state, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken line here.".to_string()]);
        assert_eq!(state.offset, 1, "should latch after speaking once");
    }

    #[test]
    fn blockquote_streamed_char_by_char_still_completes() {
        // Even when the blockquote itself is split mid-line across batches, accumulation
        // must reassemble it and speak the whole line once the body terminates it.
        let batches = [
            delta("m1", "> Spoken ", false),
            delta("m1", "line ", false),
            delta("m1", "here.", false),
            delta("m1", "\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken line here.".to_string()]);
    }

    #[test]
    fn prose_preamble_before_blockquote_is_spoken_once() {
        // A reply that opens with a little prose preamble BEFORE its spoken line must still
        // narrate the topmost blockquote — and exactly once, when the body terminates it.
        let batches = [
            delta("m1", "Okay, here's what I found.", false),
            delta("m1", "\n\n> The spoken line.", false),
            delta("m1", "\n\nNow the body of the reply.", true),
        ];
        let (state, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["The spoken line.".to_string()]);
        assert_eq!(state.offset, 1, "should latch after speaking once");
    }

    #[test]
    fn preamble_then_blockquote_streamed_char_by_char() {
        // Preamble AND the quote both split across batches → reassemble and speak once.
        let batches = [
            delta("m1", "Let me ", false),
            delta("m1", "check.\n", false),
            delta("m1", "> Spoken ", false),
            delta("m1", "line.", false),
            delta("m1", "\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken line.".to_string()]);
    }

    #[test]
    fn preamble_only_until_final_stays_silent() {
        // Preamble that never resolves into a blockquote → silence, even though early
        // batches had no quote yet (must not latch silence prematurely, must not speak).
        let batches = [
            delta("m1", "Thinking about it", false),
            delta("m1", " some more", false),
            delta("m1", " — done, no spoken line.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert!(
            spoken.is_empty(),
            "no blockquote ever ⇒ silence, got {spoken:?}"
        );
    }

    #[test]
    fn reply_without_blockquote_is_silent() {
        // A reply that doesn't OPEN with a blockquote is never voiced — we never read raw
        // replies. (This is the "it didn't play" case when Claude forgot the spoken line.)
        let batches = [
            delta("m1", "Just a plain reply, ", false),
            delta("m1", "no spoken line at all.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert!(
            spoken.is_empty(),
            "no leading blockquote ⇒ silence, got {spoken:?}"
        );
    }

    #[test]
    fn cumulative_displayed_text_mode_speaks() {
        // Forward-compat: a CC version that sends cumulative `displayedText` instead of
        // deltas must also reach the spoken line.
        let cum = |id: &str, text: &str, f: bool| MessageDisplayHook {
            displayed_text: Some(text.into()),
            message_id: Some(id.into()),
            is_final: Some(f),
            ..Default::default()
        };
        let batches = [
            cum("m1", "> Spoken.", false),
            cum("m1", "> Spoken.\n\nBody text.", false),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken.".to_string()]);
    }

    #[test]
    fn final_flag_flushes_blockquote_with_no_body() {
        // A reply that is ONLY a blockquote (no body) completes on the final batch.
        let batches = [
            delta("m1", "> Just the spoken line.", false),
            delta("m1", "", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Just the spoken line.".to_string()]);
    }

    #[test]
    fn spoken_line_voiced_at_most_once_per_message() {
        // Once spoken, every later batch of the same message is a no-op (no double-speak).
        let batches = [
            delta("m1", "> Hello.\n\nBody.", false),
            delta("m1", " more body.", false),
            delta("m1", " end.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Hello.".to_string()]);
    }

    #[test]
    fn new_message_id_resets_and_speaks_again() {
        // Dropping the `#index` must NOT merge two separate messages: a new `message_id`
        // still resets the accumulator so the next message's spoken line is voiced too.
        let batches = [
            delta("m1", "> First.\n\nBody.", true),
            delta("m2", "> Second.\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["First.".to_string(), "Second.".to_string()]);
    }

    #[test]
    fn multiple_blockquotes_speak_each_in_order() {
        // A multi-point spoken digest: three top-level blockquotes separated by body prose.
        // Each becomes its own utterance, voiced once, in order — including the closing one.
        let batches = [
            delta(
                "m1",
                "> First point.\n\nDetail about the first point.",
                false,
            ),
            delta("m1", "\n\n> Second point.\n\nMore detail.", false),
            delta(
                "m1",
                "\n\n> And the closing question?\n\nClosing detail.",
                true,
            ),
        ];
        let (state, spoken) = drive(&batches, false);
        assert_eq!(
            spoken,
            vec![
                "First point.".to_string(),
                "Second point.".to_string(),
                "And the closing question?".to_string(),
            ]
        );
        assert_eq!(state.offset, 3, "all three runs voiced");
    }

    #[test]
    fn final_blockquote_with_no_body_after_it_still_speaks() {
        // The last point ends the message with no trailing body line — it completes on the
        // final batch, and must still be voiced (the "closing question went silent" guard).
        let batches = [
            delta("m1", "> Opening point.\n\nBody.", false),
            delta("m1", "\n\n> Closing point.", false),
            delta("m1", "", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(
            spoken,
            vec!["Opening point.".to_string(), "Closing point.".to_string()]
        );
    }

    #[test]
    fn mic_active_at_message_start_gates_whole_message() {
        // If the mic was live when the message first appeared, the whole message stays
        // gated even after the blockquote completes (decided once, cached per message_id).
        let batches = [
            delta("m1", "> Spoken.", false),
            delta("m1", "\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, true);
        assert!(
            spoken.is_empty(),
            "mic live at start ⇒ message gated, got {spoken:?}"
        );
    }

    // ── Stop hook: the double-narration guard (regression) ───────────────────────────
    //
    // The bug: Claude Code ALSO delivers `last_assistant_message` on `Stop`, so the Stop
    // handler re-voiced every reply that MessageDisplay had already streamed — heard twice.
    // The fix is the `streamed` discriminator: a session whose MessageDisplay state file
    // exists already narrated, so Stop stays silent; a non-streaming client (Codex) never
    // writes that file, so its Stop still voices the whole reply. These pin both halves.

    /// A reply whose digest is two blockquotes plus body — the exact shape MessageDisplay
    /// streams and the shape Stop would re-speak if the guard regressed.
    const DIGEST_REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.";

    #[test]
    fn stop_is_silent_when_already_streamed() {
        // THE regression guard: streamed = true (Claude Code, MessageDisplay already narrated)
        // ⇒ Stop voices NOTHING, even though the reply is full of speakable blockquotes.
        let spoken = stop_utterances(
            Some(DIGEST_REPLY),
            true,
            true,
            false,
            /*streamed*/ true,
        );
        assert!(
            spoken.is_empty(),
            "streamed reply must not be re-voiced on Stop, got {spoken:?}"
        );
    }

    #[test]
    fn stop_voices_whole_reply_when_not_streamed() {
        // Codex (streamed = false): the entire reply is voiced from Stop — every top-level
        // blockquote, in order, exactly as the streaming path would have emitted them.
        let spoken = stop_utterances(
            Some(DIGEST_REPLY),
            true,
            false,
            false,
            /*streamed*/ false,
        );
        assert_eq!(
            spoken,
            vec!["First point.".to_string(), "Second point.".to_string()],
            "non-streaming Stop voices each blockquote once, in order"
        );
    }

    #[test]
    fn stop_short_fallback_only_when_not_streamed() {
        // A brief blockquote-less reply: under `short` it's voiced whole — but ONLY when not
        // already streamed. The same input with streamed = true stays silent (no double).
        let reply = "Done — all three tests pass.";
        assert_eq!(
            stop_utterances(Some(reply), false, true, false, /*streamed*/ false),
            vec!["Done — all three tests pass.".to_string()],
        );
        assert!(
            stop_utterances(Some(reply), false, true, false, /*streamed*/ true).is_empty(),
            "short reply already streamed ⇒ Stop silent"
        );
    }

    #[test]
    fn stop_silent_when_off_muted_or_empty() {
        // The other silence gates, independent of `streamed`.
        assert!(
            stop_utterances(Some(DIGEST_REPLY), false, false, false, false).is_empty(),
            "narration off ⇒ silent"
        );
        assert!(
            stop_utterances(Some(DIGEST_REPLY), true, true, /*mic*/ true, false).is_empty(),
            "mid-dictation ⇒ silent"
        );
        assert!(
            stop_utterances(None, true, true, false, false).is_empty(),
            "no final text ⇒ silent"
        );
        assert!(
            stop_utterances(Some("   \n  "), true, true, false, false).is_empty(),
            "blank final text ⇒ silent"
        );
    }

    #[test]
    fn streamed_witness_tracks_the_message_display_state_file() {
        // The IO half of the guard: `streamed_via_message_display` is true exactly when this
        // session's MessageDisplay state file (the one `message_display` writes) exists, and is
        // SESSION-SCOPED — a different session id (a fresh Codex session) reads false. Uses the
        // SAME `display_state_path` the writer uses, so the witness can't drift from the writer.
        let dir = std::env::temp_dir().join(format!("smnarrate-witness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = ds_config::Paths::rooted_at(&dir);
        let cc = "cc-session-aaaa";
        let codex = "codex-session-bbbb";

        // No state file yet ⇒ not streamed (the pre-first-batch / Codex case).
        assert!(!streamed_via_message_display(&paths, cc));

        // After a MessageDisplay batch persisted this session's state, the witness flips true —
        // for THAT session only; an unrelated session still reads false.
        let sp = display_state_path(&paths, cc);
        if let Some(parent) = sp.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        atomic_write(&sp, "{}");
        assert!(
            streamed_via_message_display(&paths, cc),
            "CC session streamed"
        );
        assert!(
            !streamed_via_message_display(&paths, codex),
            "a different (Codex) session is never marked streamed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_start_seed_closes_the_first_turn_race() {
        // The hardening: SessionStart seeds the witness so `streamed` is already true before the
        // first Stop, even if no MessageDisplay batch has landed yet — and it's SESSION-SCOPED
        // (a Codex session, which never fires SessionStart, is never seeded).
        let dir = std::env::temp_dir().join(format!("smnarrate-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = ds_config::Paths::rooted_at(&dir);
        let session = "cc-session-cccc";

        // Pre-seed: false until SessionStart runs.
        assert!(!streamed_via_message_display(&paths, session));
        mark_streaming_session(&paths, &format!(r#"{{"session_id":"{session}"}}"#));
        assert!(
            streamed_via_message_display(&paths, session),
            "SessionStart must seed the witness before any MessageDisplay batch"
        );
        // A Stop arriving right after SessionStart (before any batch) is now correctly silent.
        assert!(
            stop_utterances(Some(DIGEST_REPLY), true, true, false, true).is_empty(),
            "seeded session ⇒ Stop stays silent"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_start_seed_is_non_destructive_and_needs_a_session() {
        // It must NOT clobber real in-progress state (a re-fired SessionStart on an existing
        // session), and a payload with no session id is a no-op (nothing to scope).
        let dir = std::env::temp_dir().join(format!("smnarrate-seed2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = ds_config::Paths::rooted_at(&dir);
        let session = "cc-session-dddd";

        // Existing real state must survive a re-fired SessionStart verbatim.
        let path = display_state_path(&paths, session);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let sentinel = r#"{"offset":2,"key":"real-message-state"}"#;
        atomic_write(&path, sentinel);
        mark_streaming_session(&paths, &format!(r#"{{"session_id":"{session}"}}"#));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sentinel,
            "seed must not clobber real in-progress message state"
        );

        // No session id ⇒ no file created (nothing to scope a witness to).
        mark_streaming_session(&paths, "{}");
        assert!(
            !streamed_via_message_display(&paths, ""),
            "a session-less SessionStart seeds nothing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
