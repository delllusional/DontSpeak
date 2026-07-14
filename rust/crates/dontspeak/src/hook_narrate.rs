//! The `notify`-side narration handlers — the CLAUDE CODE / QWEN CODE ADAPTER over the
//! shared streaming-narration core (`ds-narrate`). Dispatched from
//! [`crate::hook_core::notify`]. This file only: parses the hook payload, builds a
//! client-neutral `ds_narrate::StreamBatch`, runs the shared file-backed step
//! ([`ds_narrate::narrate_batch`]), and forwards each utterance to the engine.
//!
//! [`message_display`] (`MessageDisplay`): runs once per streaming batch. Claude Code
//! sends an incremental `delta` chunk keyed by content-block `index` (+ a sticky `final`
//! flag); Qwen Code's sketched hook (QwenLM/qwen-code#6488) sends CUMULATIVE
//! `displayed_text` snapshots + `is_final` — BOTH parse through the same
//! [`MessageDisplayHook`] (serde aliases) and the same core. When `narrate` contains
//! "digests", every top-level blockquote is spoken (verbatim, each once, in document
//! order); "shorts" voices a short blockquote-less final reply whole. Fast +
//! fire-and-forget so it never delays the display.
//!
//! [`speak_reply`] (`Stop`): the non-streaming analogue — voices the whole final reply,
//! guarded by the streaming WITNESS so it never double-speaks what a streaming pass
//! (MessageDisplay here, or the engine's Codex app-server subscriber —
//! `dontspeakd::codex_stream`) already narrated. [`mark_streaming_session`]
//! (`SessionStart`) seeds that witness for streaming hook clients (Claude Code);
//! non-streaming clients pass `--greet-only` to skip it. A Codex session gets its
//! witness seeded BY THE ENGINE on a successful app-server `thread/resume` instead — a
//! plain-TUI Codex session (not on the shared app-server) never seeds, so its `Stop`
//! still speaks exactly as before.
//!
//! [`barge_session`] (`SessionEnd`): barge THIS session's engine playback (a scoped
//! `StopSpeech{session}`) so closing a window silences its OWN reply, not another's. No
//! payload → `None` session → the global barge.
//!
//! Settings (ds-config VoiceConfig): `narrate` is a SET of "digests"/"shorts".

use ds_config::{ClientSource, NarrateKind, Paths, VoiceConfig};
use ds_narrate::{BatchPayload, StreamBatch};
use serde::Deserialize;
use std::borrow::Cow;

/// SessionEnd notify: barge ONLY this session's engine playback (so closing one window
/// never silences another's reply). The `payload` is the hook JSON; no payload / no
/// session id → `None` → the global barge. `client` is the `--client` token the wiring
/// stamped, carried onto the request so the engine's log names who closed the window.
pub fn barge_session(paths: &Paths, payload: &str, client: ClientSource) {
    let session = crate::hook_core::session_id_from_payload(payload);
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::SessionEnd {
            session: session.clone(),
            source: client,
        },
    );
    // SessionEnd is terminal for this session (this path fires ONLY on SessionEnd — a
    // mid-session barge uses `StopSpeech`), so the engine reclaims this session's voice
    // maps, and here we reclaim its per-session display-state file and its lock/tmp
    // siblings. Without this they accumulate one `narrate-display-<session>.json` per
    // distinct session in the data dir forever. (Codex wires NO SessionEnd hook — its
    // cleanup is owned by the engine's codex_stream supervisor.)
    if let Some(s) = &session {
        ds_narrate::clear_session_state(paths, s);
    }
}

/// SessionStart notify (the streaming-witness seed): pre-create THIS session's streaming
/// state file so [`speak_reply`]'s `streamed` guard is reliably true before the first
/// `Stop`, closing the only timing gap in the double-narration fix. The discriminator is
/// the event wiring + the `--greet-only` flag (see [`crate::hook_core::notify`]):
///   • Claude Code (streaming) wires `SessionStart` with plain `notify` → seeds the witness.
///   • Qwen Code (non-streaming) wires `SessionStart` with `--greet-only` → greet runs but the
///     seed is SKIPPED — otherwise the witness would suppress Qwen Code's only narration path.
///   • OpenAI Codex likewise wires `SessionStart` with `--greet-only` (see `wire/codex.rs`):
///     greet runs, seed skipped — its witness is instead seeded by the ENGINE on a successful
///     app-server `thread/resume` (`dontspeakd::codex_stream`), so only sessions that are
///     actually streamed mid-turn silence their `Stop`; a plain-TUI session keeps `Stop`.
/// Idempotent + non-destructive: never clobbers real in-progress state (a re-fired
/// SessionStart is a no-op), and the seeded default reads exactly like "no file yet" (fresh
/// `Accum`), so streaming is unaffected.
pub fn mark_streaming_session(paths: &Paths, payload: &str) {
    let Some(session) = crate::hook_core::session_id_from_payload(payload) else {
        return; // no session id ⇒ can't scope a witness (the per-batch write still covers it)
    };
    ds_narrate::seed_witness(paths, &session);
}

// ── Stop hook (speak the FINAL reply — non-streaming clients) ───────────────────

/// Stop hook payload subset. Claude Code, Codex, and Qwen Code supply
/// `last_assistant_message`, which supports full-reply voicing for non-streaming clients.
///
/// Grok's Stop (live-verified) is metadata-only (`sessionId`, `reason`, `transcriptPath`,
/// etc.) with no `lastAssistantMessage`. We fall back to reading the final assistant turn
/// from the file named in `transcriptPath` (typically the session's chat_history.jsonl).
/// The camelCase alias remains for forward-compat with any client that does supply direct text.
#[derive(Debug, Deserialize, Default)]
struct StopHook {
    // Alias accepts camelCase alongside snake_case for clients that supply it.
    #[serde(default, alias = "lastAssistantMessage")]
    last_assistant_message: Option<String>,
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default, alias = "transcriptPath")]
    transcript_path: Option<String>,
}

/// Lightweight transcript entry for extracting the last assistant turn from a Grok
/// chat_history.jsonl (or similar JSONL pointed at by transcriptPath).
#[derive(Debug, Deserialize, Default)]
struct TranscriptEntry {
    #[serde(default, rename = "type")]
    r#type: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

impl StopHook {
    /// Return the best available final assistant text: direct field if present,
    /// otherwise Grok's last non-empty assistant content from `transcriptPath` (if any).
    fn last_assistant_text(&self, client: ClientSource) -> Option<Cow<'_, str>> {
        if let Some(t) = self
            .last_assistant_message
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            return Some(Cow::Borrowed(t));
        }
        if client == ClientSource::Grok {
            return self
                .transcript_path
                .as_deref()
                .and_then(read_last_assistant_from_transcript)
                .map(Cow::Owned);
        }
        None
    }
}

/// Tail the transcript file (JSONL) and return the content of the last "assistant" entry
/// that has non-empty text. This lets Grok's Stop hook narrate without the direct text
/// field. Only the last complete JSONL entries within a bounded tail are considered, and
/// they are scanned newest-first so unrelated conversation history is neither parsed nor
/// returned. The partial first line is discarded byte-wise because the seek may split a
/// UTF-8 code point.
fn read_last_assistant_from_transcript(path: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    const TAIL_BYTES: u64 = 256 * 1024;
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;

    let mut tail = Vec::with_capacity((len - start) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
    let complete_lines = if start == 0 {
        tail.as_slice()
    } else {
        let first_newline = tail.iter().position(|byte| *byte == b'\n')?;
        &tail[first_newline + 1..]
    };

    complete_lines
        .split(|byte| *byte == b'\n')
        .rev()
        .filter_map(|line| serde_json::from_slice::<TranscriptEntry>(line).ok())
        .find_map(|entry| {
            (entry.r#type.as_deref() == Some("assistant"))
                .then_some(entry.content)
                .flatten()
                .filter(|content| !content.trim().is_empty())
        })
}

/// Witness that a streaming pass ran for this session: its per-session state file exists
/// (delegates to [`ds_narrate::witness_exists`]). The deterministic client-discriminator
/// the `Stop` path needs (see [`speak_reply`]):
///   • Claude Code (streaming) wires `MessageDisplay` and streams every turn, so the file is
///     present when `Stop` fires ⇒ the reply was ALREADY narrated; `Stop` must not re-speak it.
///   • Qwen Code (non-streaming) wires `SessionStart` with `--greet-only` (no witness seed)
///     and NO `MessageDisplay` hook, so the file is NEVER written ⇒ `streamed = false`, and
///     `Stop` is Qwen Code's only narration path.
///   • OpenAI Codex wires NO `MessageDisplay` hook; its file appears ONLY when the engine's
///     app-server subscriber resumed this session's thread (mid-turn narration active) —
///     otherwise `streamed = false` and `Stop` is its narration path, exactly as before.
/// [`mark_streaming_session`] also SEEDS this file at `SessionStart` for STREAMING hook
/// clients (Claude Code), so the witness is present from session open — closing the timing
/// edge of a `Stop` racing the first batch's write.
/// `pub(crate)` so `hook_core`'s greet-only tests can probe the witness directly.
pub(crate) fn streamed_via_message_display(paths: &Paths, session: &str) -> bool {
    ds_narrate::witness_exists(paths, session)
}

/// The pure Stop decision — re-exported from the shared core so `hook_core`'s tests keep
/// driving it through this module (the seam the double-narration regression tests use).
pub(crate) use ds_narrate::stop_utterances;

/// Stop notify: speak the FINAL assistant reply, once — the NON-STREAMING analogue of
/// [`message_display`] for clients whose replies weren't streamed this session (Qwen Code,
/// plain-TUI Codex), whose hooks fire only at end-of-turn with the whole
/// `last_assistant_message`. Claude Code ALSO wires `Stop` and delivers
/// `last_assistant_message` on it, so without a guard we'd re-voice every reply the
/// streaming path already narrated (heard twice). Guard: [`streamed_via_message_display`]
/// — a session with a streaming state file already narrated ⇒ stay silent. Pure decision
/// in [`stop_utterances`]; this is the IO wrapper (config load, mic probe, witness,
/// engine send).
pub fn speak_reply(paths: &Paths, payload: &str, client: ClientSource) {
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
        hook.last_assistant_text(client).as_deref(),
        messages_on,
        short_on,
        ds_platform::is_mic_active(),
        streamed,
    );
    for line in speak {
        // Surface a rejected enqueue (queue admission caps): the narration state has
        // already advanced past this text, so stderr is the only place the drop shows.
        if let Ok(ds_ipc::Response::Error { message }) = ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text: line,
                session: session.clone(),
                source: client,
            },
        ) {
            eprintln!("dontspeak: narration rejected: {message}");
        }
    }
}

// ── MessageDisplay hook (speak-as-it-streams) ───────────────────────────────────

/// The MessageDisplay hook payload — TWO clients' shapes through ONE struct:
///   • Claude Code ≥ 2.1.x fires repeatedly while a message streams: an incremental
///     `delta` chunk per batch (2.1.183, verified against a live payload), with some
///     versions documented to send a cumulative `displayedText` instead — we accept either.
///   • Qwen Code's sketched hook (QwenLM/qwen-code#6488) sends snake_case CUMULATIVE
///     snapshots — `displayed_text` + `is_final` — which the serde ALIASES below parse
///     through the SAME fields, so the whole future Qwen flip is registry gating
///     (`hook_streaming: true`), zero handler changes.
#[derive(Debug, Deserialize, Default, Clone)]
struct MessageDisplayHook {
    // Cumulative whole-text snapshot. CC's documented camelCase name, plus Qwen's
    // sketched snake_case alias.
    #[serde(default, rename = "displayedText", alias = "displayed_text")]
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
    // a process per batch and they race, so they can reach us out of order.
    #[serde(default)]
    index: Option<u64>,
    // True on the last batch of a message. CC's name is `final`; Qwen's sketch says
    // `is_final` — both accepted.
    #[serde(default, rename = "final", alias = "is_final")]
    is_final: Option<bool>,
}

/// First ~48 chars of the message — a cheap fingerprint to detect a new message
/// stream (each message's opening text differs) when the client sends no `message_id`,
/// so the core resets its accumulation.
fn message_key(s: &str) -> String {
    s.chars().take(48).collect()
}

/// Translate one parsed hook payload into the client-neutral [`StreamBatch`] the shared
/// core consumes: the stable `message_id` (or the fingerprint fallback) becomes the key;
/// a non-empty cumulative `displayed_text` wins over the per-index `delta`.
fn batch_from_hook(hook: &MessageDisplayHook) -> StreamBatch {
    let key = match hook.message_id.as_deref().filter(|s| !s.is_empty()) {
        Some(id) => id.to_string(),
        None => message_key(
            hook.delta
                .as_deref()
                .or(hook.displayed_text.as_deref())
                .unwrap_or_default(),
        ),
    };
    let payload = match hook.displayed_text.as_deref() {
        Some(dt) if !dt.trim().is_empty() => BatchPayload::Cumulative {
            text: dt.to_string(),
        },
        _ => BatchPayload::Delta {
            index: hook.index,
            text: hook.delta.clone().unwrap_or_default(),
        },
    };
    StreamBatch {
        key,
        payload,
        is_final: hook.is_final.unwrap_or(false),
    }
}

/// MessageDisplay notify: narrate this streamed batch. `payload` is the hook JSON (already
/// read by the transport, not stdin). The shared core accumulates the chunks into the
/// cumulative text and returns each newly-completed blockquote run; we enqueue them on the
/// warm engine.
///
/// Gates ONLY on `narrate` (on/off) and not-mid-recording — NOT on focus. Narration is
/// forwarded TAGGED BY SESSION regardless of which app is frontmost; the ENGINE WORKER holds
/// an inactive/backgrounded terminal's items (never dropped here) and plays them when that
/// terminal is active + frontmost. Fast + fire-and-forget so it never delays the display.
pub fn message_display(paths: &Paths, payload: &str, client: ClientSource) {
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
    let batch = batch_from_hook(&hook);

    // The whole cross-process dance — the per-session lock, the state-file
    // read-modify-write, the atomic write — lives in the shared core; racing per-batch
    // hook processes take turns there. The engine forward stays OUTSIDE the lock — no
    // socket round-trip held under the mutex.
    let speak = ds_narrate::narrate_batch(
        paths,
        &session,
        &batch,
        ds_platform::is_mic_active(),
        messages_on,
        short_on,
    );
    // Each completed blockquote is forwarded as its OWN narration item, in order — the
    // engine's per-session worker plays them sequentially with a natural pause between, so
    // a multi-point spoken digest is heard point by point rather than in one breath.
    let session = Some(session).filter(|s| !s.is_empty());
    for text in speak {
        // See speak_reply: a rejected narration is otherwise untraceable.
        if let Ok(ds_ipc::Response::Error { message }) = ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text,
                session: session.clone(),
                source: client,
            },
        ) {
            eprintln!("dontspeak: narration rejected: {message}");
        }
    }
}

/// The test seam kept from the pre-extraction shape: hook payload in → the shared core's
/// pure step. Production goes through [`ds_narrate::narrate_batch`] instead (same
/// translation via [`batch_from_hook`]).
#[cfg(test)]
fn step_display(
    prev: &ds_narrate::DisplayState,
    hook: &MessageDisplayHook,
    mic_active: bool,
    messages_on: bool,
    short_on: bool,
) -> ds_narrate::DisplayStep {
    ds_narrate::step(
        prev,
        &batch_from_hook(hook),
        mic_active,
        messages_on,
        short_on,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_narrate::DisplayState;

    #[test]
    fn message_key_is_first_48_chars() {
        let long = "x".repeat(100);
        assert_eq!(message_key(&long).chars().count(), 48);
        assert_eq!(message_key("short"), "short");
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

    // ── Qwen Code payload seam (QwenLM/qwen-code#6488) ────────────────────────────────
    //
    // Qwen's sketched MessageDisplay payload is snake_case and CUMULATIVE:
    // `{hook_event_name, message_id, displayed_text, is_final}` — debounced whole-text
    // snapshots from ONE sequential in-process loop (no cross-process races; the lock
    // idles). The serde aliases on `displayed_text` AND `is_final` route it through the
    // SAME handler; these tests drive the exact sketched JSON.

    /// Parse a raw Qwen-shaped payload string — through serde, exactly as
    /// `message_display` would.
    fn qwen(payload: &str) -> MessageDisplayHook {
        serde_json::from_str(payload).expect("qwen payload parses")
    }

    #[test]
    fn qwen_snake_case_payload_parses_through_the_aliases() {
        let hook = qwen(
            r#"{"hook_event_name":"MessageDisplay","session_id":"q1","message_id":"qm1",
                "displayed_text":"> Spoken.\n\nBody.","is_final":true}"#,
        );
        assert_eq!(hook.displayed_text.as_deref(), Some("> Spoken.\n\nBody."));
        assert_eq!(hook.is_final, Some(true));
        assert_eq!(hook.message_id.as_deref(), Some("qm1"));
    }

    #[test]
    fn qwen_cumulative_sequence_speaks_blockquote_once() {
        // The sketched flow: debounced cumulative snapshots ending is_final=true.
        let batches = [
            qwen(r#"{"message_id":"qm1","displayed_text":"> Spoken","is_final":false}"#),
            qwen(r#"{"message_id":"qm1","displayed_text":"> Spoken line.","is_final":false}"#),
            qwen(
                r#"{"message_id":"qm1","displayed_text":"> Spoken line.\n\nBody.","is_final":true}"#,
            ),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken line.".to_string()]);
    }

    #[test]
    fn qwen_duplicate_final_batch_is_a_noop() {
        let fin =
            qwen(r#"{"message_id":"qm1","displayed_text":"> Once.\n\nBody.","is_final":true}"#);
        let (_, spoken) = drive(&[fin.clone(), fin], false);
        assert_eq!(spoken, vec!["Once.".to_string()], "spoken exactly once");
    }

    #[test]
    fn qwen_shorts_fallback_voices_a_blockquoteless_final_whole() {
        let fin = qwen(
            r#"{"message_id":"qm1","displayed_text":"Done — build is green.","is_final":true}"#,
        );
        let step = step_display(
            &DisplayState::default(),
            &fin,
            false,
            /*digests*/ false,
            /*shorts*/ true,
        );
        assert_eq!(step.speak, vec!["Done — build is green.".to_string()]);
    }

    // ── Stop hook: the double-narration guard (regression) ───────────────────────────
    //
    // The pure decision (`stop_utterances`) lives in ds-narrate with its own tests; here
    // we pin the WITNESS half through this module's delegates against a tempdir.

    /// A reply shaped like a spoken digest — what Stop would voice (or wrongly suppress).
    const DIGEST_REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.";

    #[test]
    fn streamed_witness_tracks_the_message_display_state_file() {
        // The IO half of the guard: `streamed_via_message_display` is true exactly when this
        // session's streaming state file exists, and is SESSION-SCOPED — a different session
        // id (a fresh Codex session) reads false. Uses the SAME `display_state_path` the
        // writer uses, so the witness can't drift from the writer.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let cc = "cc-session-aaaa";
        let codex = "codex-session-bbbb";

        // No state file yet ⇒ not streamed (the pre-first-batch / Codex case).
        assert!(!streamed_via_message_display(&paths, cc));

        // After a streamed batch persisted this session's state, the witness flips true —
        // for THAT session only; an unrelated session still reads false.
        let batch = StreamBatch {
            key: "m1".into(),
            payload: BatchPayload::Delta {
                index: Some(0),
                text: "> Hi.".into(),
            },
            is_final: false,
        };
        let _ = ds_narrate::narrate_batch(&paths, cc, &batch, false, true, false);
        assert!(
            streamed_via_message_display(&paths, cc),
            "CC session streamed"
        );
        assert!(
            !streamed_via_message_display(&paths, codex),
            "a different (Codex) session is never marked streamed"
        );
    }

    #[test]
    fn session_start_seed_closes_the_first_turn_race() {
        // The hardening: SessionStart seeds the witness so `streamed` is already true before
        // the first Stop, even if no MessageDisplay batch has landed yet — and it's
        // SESSION-SCOPED (a Codex session's SessionStart is greet-only, so it never reaches
        // `mark_streaming_session` and is never seeded).
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
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
    }

    #[test]
    fn session_start_seed_is_non_destructive_and_needs_a_session() {
        // It must NOT clobber real in-progress state (a re-fired SessionStart on an existing
        // session), and a payload with no session id is a no-op (nothing to scope).
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-dddd";

        // Existing real state must survive a re-fired SessionStart verbatim.
        let path = ds_narrate::display_state_path(&paths, session);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let sentinel = r#"{"offset":2,"key":"real-message-state"}"#;
        std::fs::write(&path, sentinel).unwrap();
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
    }

    #[test]
    fn barge_session_clears_the_state_file_trio() {
        // SessionEnd reclaims the per-session display-state file and its lock/tmp siblings
        // (the engine ping is a best-effort no-op against the tempdir's nonexistent socket).
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-eeee";
        ds_narrate::seed_witness(&paths, session);
        let path = ds_narrate::display_state_path(&paths, session);
        std::fs::write(path.with_extension("lock"), "").unwrap();
        std::fs::write(path.with_extension("tmp"), "").unwrap();

        barge_session(
            &paths,
            &format!(r#"{{"session_id":"{session}"}}"#),
            ClientSource::ClaudeCode,
        );
        assert!(!path.exists(), "state file removed");
        assert!(!path.with_extension("lock").exists(), "lock removed");
        assert!(!path.with_extension("tmp").exists(), "tmp removed");
    }

    // ── Grok Stop payload (live-verified field names + event casing) ─────────────────
    #[test]
    fn camelcase_stop_aliases_remain_forward_compatible() {
        let payload = r#"{"hookEventName":"stop","sessionId":"g1","lastAssistantMessage":"> Hi.\n\nDetail."}"#;
        assert_eq!(crate::hook_core::event_name(payload), "Stop");
        let hook: StopHook =
            serde_json::from_str(payload).expect("Stop payload parses (aliases + normalization)");
        assert_eq!(hook.session_id.as_deref(), Some("g1"));
        assert_eq!(
            hook.last_assistant_message.as_deref(),
            Some("> Hi.\n\nDetail.")
        );
        assert_eq!(
            hook.last_assistant_text(ClientSource::Grok).as_deref(),
            Some("> Hi.\n\nDetail.")
        );
    }

    #[test]
    fn real_grok_stop_is_metadata_only() {
        // Sanitized from a live Grok 0.2.93 capture on 2026-07-13.
        let payload = r#"{"hookEventName":"stop","sessionId":"g-real","cwd":"C:\\Users\\usr","transcriptPath":"...","promptId":"p1","reason":"end_turn"}"#;
        assert_eq!(crate::hook_core::event_name(payload), "Stop");
        let hook: StopHook = serde_json::from_str(payload).expect("real Grok Stop parses");
        assert_eq!(hook.session_id.as_deref(), Some("g-real"));
        assert!(
            hook.last_assistant_message.is_none(),
            "Grok Stop carries no last* text"
        );
        // transcriptPath "..." does not exist → no text extracted
        assert!(hook.last_assistant_text(ClientSource::Grok).is_none());
        // Consequently Stop contributes no narration text (earcon may still ring via the arm).
        assert!(
            stop_utterances(
                hook.last_assistant_text(ClientSource::Grok).as_deref(),
                true,
                false,
                false,
                false
            )
            .is_empty()
        );
    }

    #[test]
    fn grok_transcript_path_fallback_extracts_last_assistant() {
        // Fixture: sanitized live-style transcript (JSONL) as requested in #49.
        // We only need a realistic tail with a final assistant turn containing a digest.
        let dir = tempfile::tempdir().unwrap();
        let tx_path = dir.path().join("chat_history.jsonl");
        let transcript = r#"{"type":"user","content":"do the thing"}
{"type":"assistant","content":"> Point one.\n\nDetails about point one.\n\n> And the question?","model_id":"grok-build"}
{"type":"tool_result","content":"ok"}
"#;
        std::fs::write(&tx_path, transcript).unwrap();

        let payload = format!(
            r#"{{"hookEventName":"stop","sessionId":"g-tx","transcriptPath":"{}"}}"#,
            tx_path.to_string_lossy().replace('\\', "\\\\")
        );
        let hook: StopHook = serde_json::from_str(&payload).expect("parses with transcriptPath");
        let text = hook
            .last_assistant_text(ClientSource::Grok)
            .expect("extracted from transcript");
        assert!(text.contains("> Point one."));
        assert!(text.contains("> And the question?"));

        // Feeding the extracted text through stop_utterances should yield the spoken lines.
        let spoken = stop_utterances(Some(&text), true, false, false, false);
        assert_eq!(
            spoken,
            vec!["Point one.".to_string(), "And the question?".to_string()]
        );
    }

    #[test]
    fn transcript_path_fallback_is_grok_only() {
        let dir = tempfile::tempdir().unwrap();
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"assistant","content":"> Private file content."}"#,
        )
        .unwrap();
        let hook = StopHook {
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };

        assert!(
            hook.last_assistant_text(ClientSource::ClaudeCode).is_none(),
            "non-Grok hook payloads must not cause arbitrary transcript reads"
        );
    }

    #[test]
    fn transcript_tail_handles_a_seek_inside_utf8() {
        const TAIL_BYTES: usize = 256 * 1024;
        let assistant = b"\n{\"type\":\"assistant\",\"content\":\"> Final digest.\"}\n";
        let start = assistant.len();
        let mut transcript = vec![b'x'; TAIL_BYTES];
        transcript[start - 1] = 0xc3;
        transcript[start] = 0xa9;
        transcript.extend_from_slice(assistant);

        let dir = tempfile::tempdir().unwrap();
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(&tx_path, transcript).unwrap();

        assert_eq!(
            read_last_assistant_from_transcript(tx_path.to_str().unwrap()).as_deref(),
            Some("> Final digest.")
        );
    }
}
