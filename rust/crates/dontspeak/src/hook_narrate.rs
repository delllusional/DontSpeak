//! The `notify`-side narration handlers — the CLAUDE CODE / QWEN CODE ADAPTER over the
//! shared streaming-narration core (`ds-narrate`). Dispatched from
//! [`crate::hook_core::notify`]. This file only: parses the hook payload, builds a
//! client-neutral `ds_narrate::StreamBatch`, runs the shared file-backed step
//! ([`ds_narrate::deliver_batch`]), and forwards each utterance to the engine.
//!
//! [`message_display`] (`MessageDisplay`): runs once per streaming batch. Claude Code
//! sends an incremental `delta` chunk keyed by content-block `index` (+ a sticky `final`
//! flag); Qwen Code sends CUMULATIVE
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
//! (`SessionStart`) seeds that witness for streaming hook clients (Claude Code and Qwen Code);
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
    // Grok Stop digests are admitted under a sticky session tag (see `grok_stop_session_tag`)
    // so MarkActive `input_clears=[current]` cannot prune them. SessionEnd must still barge
    // that sticky queue — plain `SessionEnd{session}` only clears the real session id.
    if client == ClientSource::Grok {
        let sticky = session
            .as_deref()
            .map(grok_stop_session_tag)
            .unwrap_or_else(|| "grok-stop".into());
        // SessionEnd (not only StopSpeech) so sticky tags reclaim pool assignment /
        // forget_narration_session state the same way the real session does.
        let _ = ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SessionEnd {
                session: Some(sticky),
                source: client,
            },
        );
        if let Some(s) = &session {
            let _ = std::fs::remove_file(last_spoken_fingerprint_path(paths, s));
        }
    }
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
///   • Claude Code and Qwen Code wire `SessionStart` with plain `notify` and seed the witness.
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
    /// Working directory Grok reports on Stop (live: `"cwd":"C:\\Users\\usr"`). Used to
    /// reconstruct `~/.grok/sessions/<encoded-cwd>/<sessionId>/chat_history.jsonl` when
    /// `transcriptPath` is missing or does not point at a readable file.
    #[serde(default)]
    cwd: Option<String>,
}

/// Lightweight transcript entry for extracting the last assistant turn from a Grok
/// chat_history.jsonl (or similar JSONL pointed at by transcriptPath).
#[derive(Debug, Deserialize, Default)]
struct TranscriptEntry {
    #[serde(default, rename = "type")]
    r#type: Option<String>,
    /// Grok stores plain string content; accept other JSON shapes without failing the line.
    #[serde(default)]
    content: Option<serde_json::Value>,
}

impl TranscriptEntry {
    fn text_content(&self) -> Option<String> {
        match &self.content {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
            Some(serde_json::Value::Array(parts)) => {
                // Content-block arrays: join text parts if present.
                let mut out = String::new();
                for part in parts {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                    } else if let Some(t) = part.as_str() {
                        out.push_str(t);
                    }
                }
                (!out.trim().is_empty()).then_some(out)
            }
            _ => None,
        }
    }
}

impl StopHook {
    /// Return the best available final assistant text: direct field if present,
    /// otherwise Grok's last non-empty assistant content from `transcriptPath` (if any).
    fn last_assistant_text(&self, client: ClientSource, paths: &Paths) -> Option<Cow<'_, str>> {
        if let Some(t) = self
            .last_assistant_message
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            return Some(Cow::Borrowed(t));
        }
        if client == ClientSource::Grok {
            let session = self
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("-");
            // Digests preferred when enabled; this helper is used by tests and non-`speak_reply`
            // paths that do not load VoiceConfig — assume digests on (live default).
            // Re-resolves the transcript path on every retry attempt (updates.jsonl may
            // exist before sibling chat_history.jsonl appears).
            return select_grok_stop_text(self, paths, session, true)
                .map(|(text, _fp)| Cow::Owned(text));
        }
        None
    }
}

/// Session key used to admit Grok Stop digests (and the co-queued reply_done earcon) so
/// they survive MarkActive `input_clears=[current]` on the *real* session id, while
/// remaining barge-able on SessionEnd via an extra `SessionEnd` for this tag.
fn grok_stop_session_tag(session: &str) -> String {
    format!("grok-stop:{session}")
}

/// Percent-encode a cwd the way Grok names session folders (`C:\Users\usr` →
/// `C%3A%5CUsers%5Cusr`). Unreserved ASCII is left alone; everything else is `%HH`.
fn encode_grok_session_cwd(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len() * 3);
    for &b in cwd.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// If Grok points `transcriptPath` at a non-chat file (live: `updates.jsonl` — the ACP
/// event stream, which has no `type:assistant` lines), prefer a sibling `chat_history.jsonl`.
fn prefer_chat_history_transcript(path: std::path::PathBuf) -> std::path::PathBuf {
    let is_updates = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("updates.jsonl"));
    if is_updates {
        let chat = path.with_file_name("chat_history.jsonl");
        if chat.is_file() {
            return chat;
        }
    }
    path
}

/// Resolve the on-disk Grok chat transcript for a Stop payload.
///
/// Order:
///   1. `transcriptPath` when it names an existing file — but if that file is
///      `updates.jsonl` (live Grok 0.2.x Stop), use sibling `chat_history.jsonl` instead
///   2. `~/.grok/sessions/<encoded-cwd>/<sessionId>/chat_history.jsonl` from `cwd`+`sessionId`
///   3. Any `~/.grok/sessions/*/<sessionId>/chat_history.jsonl` (cwd missing / encoding skew)
fn resolve_grok_transcript_path(hook: &StopHook, paths: &Paths) -> Option<std::path::PathBuf> {
    if let Some(raw) = hook
        .transcript_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let p = std::path::PathBuf::from(raw);
        if p.is_file() {
            return Some(prefer_chat_history_transcript(p));
        }
        // Path may be stale or mis-named; still try the chat_history sibling.
        let chat = p.with_file_name("chat_history.jsonl");
        if chat.is_file() {
            return Some(chat);
        }
    }
    let session = hook
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let sessions_root = paths.grok_dir.join("sessions");
    if let Some(cwd) = hook.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let candidate = sessions_root
            .join(encode_grok_session_cwd(cwd))
            .join(session)
            .join("chat_history.jsonl");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Encoding / cwd skew: scan one level of session parents for this session id.
    // Prefer the newest mtime when multiple cwd folders share the same session id.
    let Ok(entries) = std::fs::read_dir(&sessions_root) else {
        return None;
    };
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let candidate = entry.path().join(session).join("chat_history.jsonl");
        if !candidate.is_file() {
            continue;
        }
        let modified = candidate
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let take = best
            .as_ref()
            .map(|(_, t)| modified > *t)
            .unwrap_or(true);
        if take {
            best = Some((candidate, modified));
        }
    }
    best.map(|(p, _)| p)
}

/// Whether `text` has at least one top-level `>` digest run (the same extractor Stop uses
/// for digests mode). Used to prefer a digest-bearing assistant turn over a newer tool-status
/// line that would otherwise win a pure "last non-empty" scan.
fn has_digest_blockquote(text: &str) -> bool {
    !ds_config::all_blockquotes(text).is_empty()
}

/// Fingerprint of a spoken digest for this turn so a re-fired Stop cannot re-voice the
/// same digests while they remain on disk after a successful enqueue.
///
/// Includes the last user text and its position in the transcript tail so a **later**
/// turn that happens to emit identical digest body is not treated as already spoken.
fn digest_fingerprint(last_user_text: &str, last_user_pos: Option<usize>, digest_text: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    last_user_text.trim().hash(&mut h);
    last_user_pos.hash(&mut h);
    digest_text.trim().hash(&mut h);
    h.finish()
}

fn last_spoken_fingerprint_path(paths: &Paths, session: &str) -> std::path::PathBuf {
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    paths
        .engine_pid
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("grok-stop-last-{safe}.fp"))
}

fn load_last_spoken_fingerprint(paths: &Paths, session: &str) -> Option<u64> {
    let raw = std::fs::read_to_string(last_spoken_fingerprint_path(paths, session)).ok()?;
    raw.trim().parse().ok()
}

fn store_last_spoken_fingerprint(paths: &Paths, session: &str, fp: u64) {
    let path = last_spoken_fingerprint_path(paths, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Atomic replace: concurrent Stop hooks may race; temp+rename is best-effort on Windows.
    let tmp = path.with_extension("fp.tmp");
    if std::fs::write(&tmp, fp.to_string()).is_ok()
        && std::fs::rename(&tmp, &path).is_err()
    {
        // Windows: rename over existing may fail; fall back to write-in-place.
        let _ = std::fs::write(&path, fp.to_string());
        let _ = std::fs::remove_file(&tmp);
    }
}

/// One JSONL chat entry we care about for turn scoping and turn-keyed fingerprints.
#[derive(Debug)]
enum ChatRole {
    /// User message text (may be empty after trim if the line had no usable content).
    User(String),
    Assistant(String),
}

/// Tail the transcript file (JSONL) and return chat roles in chronological order (oldest
/// first within the tail). Only the last complete JSONL entries within a bounded tail are
/// considered so a long history is not fully parsed; the partial first line is discarded
/// byte-wise because the seek may split a UTF-8 code point.
fn chat_roles_chronological(path: &std::path::Path) -> Vec<ChatRole> {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let len = meta.len();
    const TAIL_BYTES: u64 = 256 * 1024;
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }

    let mut tail = Vec::with_capacity((len - start) as usize);
    if file.take(TAIL_BYTES).read_to_end(&mut tail).is_err() {
        return Vec::new();
    }
    let complete_lines = if start == 0 {
        tail.as_slice()
    } else {
        let Some(first_newline) = tail.iter().position(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        &tail[first_newline + 1..]
    };

    let mut out = Vec::new();
    for line in complete_lines.split(|byte| *byte == b'\n') {
        let Ok(entry) = serde_json::from_slice::<TranscriptEntry>(line) else {
            continue;
        };
        match entry.r#type.as_deref() {
            Some("user") => {
                // Keep the role even when text is empty so the turn boundary still moves.
                out.push(ChatRole::User(
                    entry.text_content().unwrap_or_default(),
                ));
            }
            Some("assistant") => {
                if let Some(text) = entry.text_content() {
                    out.push(ChatRole::Assistant(text));
                }
            }
            _ => {}
        }
    }
    out
}

/// Assistants after the last user message (newest first) plus turn identity for fingerprints.
#[derive(Debug, Default)]
struct CurrentTurn {
    /// Text of the last user message in the scanned tail (empty if none).
    last_user_text: String,
    /// Index of that user role in the chronological tail, when present.
    last_user_pos: Option<usize>,
    /// Non-empty assistant texts for this turn only, newest first.
    assistants_newest_first: Vec<String>,
}

/// Current-turn assistants only (after the last user message), newest first.
/// Never crosses into a previous user turn — that was the "played previous digests" live
/// bug when Stop fired before the current assistant line was flushed.
fn current_turn_from_path(path: &std::path::Path) -> CurrentTurn {
    let roles = chat_roles_chronological(path);
    let last_user_pos = roles
        .iter()
        .rposition(|r| matches!(r, ChatRole::User(_)));
    let after = last_user_pos.map(|i| i + 1).unwrap_or(0);
    let last_user_text = last_user_pos
        .and_then(|i| match &roles[i] {
            ChatRole::User(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let assistants_newest_first = roles[after..]
        .iter()
        .rev()
        .filter_map(|r| match r {
            ChatRole::Assistant(t) => Some(t.clone()),
            ChatRole::User(_) => None,
        })
        .collect();
    CurrentTurn {
        last_user_text,
        last_user_pos,
        assistants_newest_first,
    }
}

/// Full retry budget while the path is missing or the current turn has no assistant yet
/// (waiting for a late `chat_history` flush). Tests use a small count so they stay fast.
fn stop_retry_attempts() -> usize {
    #[cfg(test)]
    {
        3
    }
    #[cfg(not(test))]
    {
        20
    }
}

/// Extra attempts after a non-digest current-turn assistant is already visible. Waiting the
/// full budget always would delay shorts-only Stop (and the ding) by ~2s even when digests
/// will never appear. Digests that land shortly after a status line still get a short window.
fn stop_retry_after_shorts_seen() -> usize {
    #[cfg(test)]
    {
        1
    }
    #[cfg(not(test))]
    {
        3
    }
}

fn stop_retry_delay() -> std::time::Duration {
    #[cfg(test)]
    {
        std::time::Duration::from_millis(0)
    }
    #[cfg(not(test))]
    {
        std::time::Duration::from_millis(100)
    }
}

/// Selected Grok Stop text plus an optional digest fingerprint to commit **only after**
/// successful enqueue (mic gate / engine-down must not permanently skip a digest).
struct GrokStopSelection {
    text: String,
    /// Present when `text` is a digest-bearing assistant message.
    digest_fp: Option<u64>,
    /// Resolved transcript path used for the selection (logging).
    path: std::path::PathBuf,
}

/// Grok Stop has no `lastAssistantMessage`; narration comes from `transcriptPath`.
///
/// Selection rules (live-hardened):
///   1. Re-resolve the transcript path every attempt (updates.jsonl may exist before sibling
///      chat_history.jsonl appears).
///   2. Only consider assistants after the last user message (current turn). Never re-voice
///      a previous turn's digests when Stop races the chat_history flush.
///   3. When digests mode is on, prefer the newest current-turn assistant with a top-level
///      `>` digest (skip tool-status lines that follow digests in agentic turns).
///   4. Skip digests whose *turn-scoped* fingerprint matches the last *successfully spoken*
///      Stop for this session; retry for a newer flush rather than re-playing.
///   5. Fall back to the newest non-empty current-turn assistant for shorts-only replies.
///   6. Full retry budget only while the path/turn is empty; once a non-digest assistant is
///      visible, use a short secondary budget (or return immediately when digests are off).
///
/// Does **not** persist fingerprints — the caller commits after enqueue success.
fn select_grok_stop_text(
    hook: &StopHook,
    paths: &Paths,
    session: &str,
    messages_on: bool,
) -> Option<(String, Option<u64>)> {
    select_grok_stop_text_detailed(hook, paths, session, messages_on)
        .map(|s| (s.text, s.digest_fp))
}

/// Cheap (len, mtime) signature so fingerprint-match retries stop once the file is stable.
fn transcript_file_sig(path: &std::path::Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = path.metadata().ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Pick the best shorts-fallback assistant body for the current turn.
/// When digests mode is off, prefer a non-digest line so a status/short body wins over a
/// digest-bearing final that would yield silence under shorts-only `stop_utterances`.
fn shorts_fallback_text(turn: &CurrentTurn, messages_on: bool) -> Option<String> {
    if messages_on {
        return turn.assistants_newest_first.first().cloned();
    }
    turn.assistants_newest_first
        .iter()
        .find(|t| !has_digest_blockquote(t))
        .cloned()
        .or_else(|| turn.assistants_newest_first.first().cloned())
}

fn select_grok_stop_text_detailed(
    hook: &StopHook,
    paths: &Paths,
    session: &str,
    messages_on: bool,
) -> Option<GrokStopSelection> {
    let last_fp = load_last_spoken_fingerprint(paths, session);
    let mut shorts_fallback: Option<GrokStopSelection> = None;
    let mut prev_fp_match_sig: Option<(u64, std::time::SystemTime)> = None;
    let mut shorts_seen_at: Option<usize> = None;
    let attempts = stop_retry_attempts();
    let shorts_extra = stop_retry_after_shorts_seen();
    let delay = stop_retry_delay();

    for attempt in 0..attempts {
        let Some(path) = resolve_grok_transcript_path(hook, paths) else {
            if attempt + 1 < attempts {
                std::thread::sleep(delay);
            }
            continue;
        };
        let turn = current_turn_from_path(&path);

        // Digests mode: prefer newest digest-bearing assistant in the current turn.
        if messages_on
            && let Some(digest) = turn
                .assistants_newest_first
                .iter()
                .find(|t| has_digest_blockquote(t))
        {
            let fp =
                digest_fingerprint(&turn.last_user_text, turn.last_user_pos, digest);
            if last_fp != Some(fp) {
                return Some(GrokStopSelection {
                    text: digest.clone(),
                    digest_fp: Some(fp),
                    path,
                });
            }
            // Same digests as last spoken for this turn: only keep waiting if the transcript
            // is still changing (a newer flush may replace them). Stable file → give up.
            let sig = transcript_file_sig(&path);
            if sig.is_some() && sig == prev_fp_match_sig {
                return None;
            }
            prev_fp_match_sig = sig;
        } else if let Some(text) = shorts_fallback_text(&turn, messages_on) {
            if shorts_fallback.is_none() {
                shorts_fallback = Some(GrokStopSelection {
                    text,
                    digest_fp: None,
                    path: path.clone(),
                });
                shorts_seen_at = Some(attempt);
                // Digests disabled: no reason to wait for a `>` line we would ignore.
                if !messages_on {
                    return shorts_fallback;
                }
            }
            if let Some(seen_at) = shorts_seen_at
                && attempt.saturating_sub(seen_at) >= shorts_extra
            {
                return shorts_fallback;
            }
        }

        if attempt + 1 < attempts {
            std::thread::sleep(delay);
        }
    }
    shorts_fallback
}

/// Witness that a streaming pass ran for this session: its per-session state file exists
/// (delegates to [`ds_narrate::witness_exists`]). The deterministic client-discriminator
/// the `Stop` path needs (see [`speak_reply`]):
///   • Claude Code and Qwen Code wire `MessageDisplay` and seed the file at SessionStart, so
///     `Stop` must not repeat the streamed reply.
///   • OpenAI Codex wires NO `MessageDisplay` hook; its file appears ONLY when the engine's
///     app-server subscriber resumed this session's thread (mid-turn narration active) —
///     otherwise `streamed = false` and `Stop` is its narration path, exactly as before.
/// [`mark_streaming_session`] also SEEDS this file at `SessionStart` for STREAMING hook
/// clients, so the witness is present from session open — closing the timing
/// edge of a `Stop` racing the first batch's write.
/// `pub(crate)` so `hook_core`'s greet-only tests can probe the witness directly.
pub(crate) fn streamed_via_message_display(paths: &Paths, session: &str) -> bool {
    ds_narrate::witness_exists(paths, session)
}

/// The pure Stop decision — re-exported from the shared core so `hook_core`'s tests keep
/// driving it through this module (the seam the double-narration regression tests use).
pub(crate) use ds_narrate::stop_utterances;

/// Stop notify: speak the FINAL assistant reply, once — the NON-STREAMING analogue of
/// [`message_display`] for clients whose replies weren't streamed this session (notably
/// plain-TUI Codex), whose hooks fire only at end-of-turn with the whole
/// `last_assistant_message`. Claude Code and Qwen Code also wire `Stop`, so without a guard
/// we'd re-voice every reply the
/// streaming path already narrated (heard twice). Guard: [`streamed_via_message_display`]
/// — a session with a streaming state file already narrated ⇒ stay silent. Pure decision
/// in [`stop_utterances`]; this is the IO wrapper (config load, mic probe, witness,
/// engine send).
///
/// Returns `Some(session)` when the caller should enqueue the reply_done earcon under that
/// session (Grok sticky tag). Returns `None` to use the payload session via
/// [`crate::hook_speak::engine_earcon`].
pub fn speak_reply(paths: &Paths, payload: &str, client: ClientSource) -> Option<Option<String>> {
    let cfg = VoiceConfig::load(paths);
    let messages_on = cfg.narrates(NarrateKind::Digests);
    let short_on = cfg.narrates(NarrateKind::Shorts);
    if !messages_on && !short_on {
        return None; // narration off ⇒ stay silent (skip parsing + the witness stat)
    }
    let Ok(hook) = serde_json::from_str::<StopHook>(payload.trim()) else {
        return None;
    };
    let session = hook.session_id.clone().filter(|s| !s.trim().is_empty());
    let streamed = streamed_via_message_display(paths, session.as_deref().unwrap_or_default());

    // Stop is the hook route's final retry opportunity for work rejected while the queue
    // was full. It retries the identified pending utterance, then the ordinary streaming
    // witness still suppresses the whole-reply fallback.
    if streamed {
        let session_id = session.as_deref().unwrap_or_default();
        if let Err(message) = ds_narrate::retry_pending(paths, session_id, |utterance| {
            admit_narration(paths, session.clone(), client, utterance)
        }) {
            eprintln!("dontspeak: narration rejected: {message}");
        }
    }

    let mic_active = ds_platform::is_mic_active();

    // Grok: prefer a direct lastAssistantMessage when present (forward-compat); otherwise
    // re-resolve path + turn-scoped selection with deferred fingerprint commit.
    // Other clients: last_assistant_message (or None).
    let (assistant_text, grok_selection): (Option<String>, Option<GrokStopSelection>) =
        if client == ClientSource::Grok {
            if let Some(direct) = hook
                .last_assistant_message
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                (Some(direct.to_owned()), None)
            } else {
                let sess = session.as_deref().unwrap_or("-");
                match select_grok_stop_text_detailed(&hook, paths, sess, messages_on) {
                    Some(sel) => (Some(sel.text.clone()), Some(sel)),
                    None => (None, None),
                }
            }
        } else {
            (
                hook.last_assistant_text(client, paths)
                    .map(|c| c.into_owned()),
                None,
            )
        };

    let speak = stop_utterances(
        assistant_text.as_deref(),
        messages_on,
        short_on,
        mic_active,
        streamed,
    );
    // Grok Stop digests use a sticky session tag so MarkActive `input_clears=[current]` on
    // the real session id cannot prune them (ding-only race). SessionEnd barges the sticky
    // tag explicitly in `barge_session`. Non-Grok keeps the real session.
    let admit_session = if client == ClientSource::Grok {
        session
            .as_deref()
            .map(grok_stop_session_tag)
            .or_else(|| Some("grok-stop".into()))
    } else {
        session.clone()
    };
    if client == ClientSource::Grok {
        let path_disp = grok_selection
            .as_ref()
            .map(|s| s.path.display().to_string());
        if speak.is_empty() && !streamed {
            let detail = match (path_disp.as_deref(), assistant_text.as_deref()) {
                (None, None) => {
                    "no readable transcript or empty current-turn assistants".to_string()
                }
                (Some(p), None) => format!("transcript {p} had no current-turn assistant text"),
                (_, Some(t)) if !has_digest_blockquote(t) => {
                    format!(
                        "assistant text had no > digests ({} chars; shorts={short_on}; mic={mic_active})",
                        t.chars().count()
                    )
                }
                _ => format!(
                    "stop_utterances empty (mic={mic_active}; streamed={streamed}; digests_on={messages_on}; shorts_on={short_on})"
                ),
            };
            ds_log::log_from(
                &paths.log_file,
                ds_log::LogLevel::Info,
                "hook",
                client,
                &format!(
                    "grok Stop: no speech session={} ({detail})",
                    session.as_deref().unwrap_or("-")
                ),
            );
        } else if !speak.is_empty() {
            ds_log::log_from(
                &paths.log_file,
                ds_log::LogLevel::Info,
                "hook",
                client,
                &format!(
                    "grok Stop: speaking {} utterance(s) from {} session={} admit={}",
                    speak.len(),
                    path_disp.as_deref().unwrap_or("-"),
                    session.as_deref().unwrap_or("-"),
                    admit_session.as_deref().unwrap_or("-")
                ),
            );
        }
    }
    let mut any_enqueued = false;
    let mut any_failed = false;
    for line in speak {
        // Surface a rejected enqueue from the non-streaming fallback.
        match ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text: line,
                session: admit_session.clone(),
                narration_id: None,
                source: client,
            },
        ) {
            Ok(ds_ipc::Response::Error { message }) => {
                any_failed = true;
                eprintln!("dontspeak: narration rejected: {message}");
            }
            Ok(_) => any_enqueued = true,
            Err(e) => {
                any_failed = true;
                eprintln!("dontspeak: narration request failed: {e}");
            }
        }
    }
    // Commit digest fingerprint only after the full utterance list is admitted — a partial
    // multi-line enqueue must not permanently skip remaining unspoken digests.
    if any_enqueued
        && !any_failed
        && let (Some(sel), Some(s)) = (grok_selection.as_ref(), session.as_deref())
        && let Some(fp) = sel.digest_fp
    {
        store_last_spoken_fingerprint(paths, s, fp);
    }
    // Grok: co-queue reply_done under the sticky admit session so digests play before the ding.
    if client == ClientSource::Grok {
        Some(admit_session)
    } else {
        None
    }
}

// ── MessageDisplay hook (speak-as-it-streams) ───────────────────────────────────

/// The MessageDisplay hook payload — TWO clients' shapes through ONE struct:
///   • Claude Code ≥ 2.1.x fires repeatedly while a message streams: an incremental
///     `delta` chunk per batch (2.1.183, verified against a live payload), with some
///     versions documented to send a cumulative `displayedText` instead — we accept either.
///   • Qwen Code sends snake_case CUMULATIVE
///     snapshots — `displayed_text` + `is_final` — which the serde ALIASES below parse
///     through the SAME fields.
#[derive(Debug, Deserialize, Default, Clone)]
struct MessageDisplayHook {
    // Cumulative whole-text snapshot. CC's documented camelCase name, plus Qwen's
    // snake_case alias.
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
    // True on the last batch of a message. CC's name is `final`; Qwen uses
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
    let messages_on = cfg.narrates(NarrateKind::Digests); // voice model-written summaries
    let short_on = cfg.narrates(NarrateKind::Shorts); // voice a short blockquote-less reply whole
    if !messages_on && !short_on {
        return; // narration off for messages ⇒ stay silent
    }
    let Ok(hook) = serde_json::from_str::<MessageDisplayHook>(payload.trim()) else {
        return;
    };
    let session = hook.session_id.clone().unwrap_or_default();
    let batch = batch_from_hook(&hook);

    // The state transaction stays locked through the local admission round-trip: racing
    // hook processes cannot both offer the same checkpoint, and rejected work remains
    // pending instead of advancing the delivered high-water mark.
    let session_tag = Some(session.clone()).filter(|s| !s.is_empty());
    if let Err(message) = ds_narrate::deliver_batch(
        paths,
        &session,
        &batch,
        ds_platform::is_mic_active(),
        messages_on,
        short_on,
        |utterance| admit_narration(paths, session_tag.clone(), client, utterance),
    ) {
        eprintln!("dontspeak: narration rejected: {message}");
    }
}

fn admit_narration(
    paths: &Paths,
    session: Option<String>,
    client: ClientSource,
    utterance: &ds_narrate::NarrationUtterance,
) -> Result<(), String> {
    match ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::SpeakNarration {
            text: utterance.text.clone(),
            session,
            narration_id: Some(utterance.id.clone()),
            source: client,
        },
    ) {
        Ok(ds_ipc::Response::Done) => Ok(()),
        Ok(ds_ipc::Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected engine response: {other:?}")),
        Err(error) => Err(format!("engine request failed: {error}")),
    }
}

/// The test seam kept from the pre-extraction shape: hook payload in → the shared core's
/// pure step. Production goes through [`ds_narrate::deliver_batch`] instead (same
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

    // ── Qwen Code payload seam ───────────────────────────────────────────────────────
    //
    // Qwen's MessageDisplay payload is snake_case and cumulative:
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
        ds_narrate::deliver_batch(&paths, cc, &batch, false, true, false, |_| Ok(())).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
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
            hook.last_assistant_text(ClientSource::Grok, &paths)
                .as_deref(),
            Some("> Hi.\n\nDetail.")
        );
    }

    #[test]
    fn real_grok_stop_is_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
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
        assert!(hook
            .last_assistant_text(ClientSource::Grok, &paths)
            .is_none());
        // Consequently Stop contributes no narration text (earcon may still ring via the arm).
        assert!(
            stop_utterances(
                hook.last_assistant_text(ClientSource::Grok, &paths)
                    .as_deref(),
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
        let paths = Paths::rooted_at(dir.path());
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
            .last_assistant_text(ClientSource::Grok, &paths)
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
    fn grok_resolves_chat_history_from_cwd_and_session_when_path_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let cwd = r"C:\Users\usr";
        let session = "019f6c8a-e074-71b1-ae8a-0b66c9f4183f";
        let tx_dir = paths
            .grok_dir
            .join("sessions")
            .join(encode_grok_session_cwd(cwd))
            .join(session);
        std::fs::create_dir_all(&tx_dir).unwrap();
        let tx_path = tx_dir.join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"assistant","content":"> Resolved via cwd fallback.\n\nBody."}"#,
        )
        .unwrap();

        let hook = StopHook {
            session_id: Some(session.into()),
            cwd: Some(cwd.into()),
            // Missing / unusable path — live regressions omit or mis-point this field.
            transcript_path: Some("...".into()),
            ..StopHook::default()
        };
        let text = hook
            .last_assistant_text(ClientSource::Grok, &paths)
            .expect("cwd+session fallback must open chat_history.jsonl");
        assert!(text.contains("> Resolved via cwd fallback."));
    }

    #[test]
    fn grok_transcript_prefers_digest_assistant_over_newer_status_line() {
        // Live agentic shape: digests final message, then a tool-status assistant without
        // blockquotes. A pure "last non-empty" scan would silence digests (only the ding).
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let transcript = r#"{"type":"user","content":"check digests"}
{"type":"assistant","content":"> This is a DontSpeak digest check.\n> If Stop narration works, you should hear these two lines.\n\nPlain body.","model_id":"grok-4.5"}
{"type":"assistant","content":"Stop fires (ding only) - digging into why digests aren't spoken.","tool_calls":[{"id":"1","name":"run_terminal_command","arguments":"{}"}]}
"#;
        std::fs::write(&tx_path, transcript).unwrap();

        let hook = StopHook {
            session_id: Some("sess".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (text, _) = select_grok_stop_text(&hook, &paths, "sess", true)
            .expect("must find the digest-bearing assistant");
        assert!(
            text.contains("> This is a DontSpeak digest check."),
            "must skip the newer status line, got: {text}"
        );
        let spoken = stop_utterances(Some(&text), true, true, false, false);
        assert_eq!(spoken.len(), 1, "adjacent > lines form one digest run: {spoken:?}");
        assert!(
            spoken[0].contains("This is a DontSpeak digest check.")
                && spoken[0].contains("If Stop narration works"),
            "spoken={spoken:?}"
        );
    }

    #[test]
    fn grok_stop_does_not_revoice_previous_turn_digests() {
        // Live: Stop races the chat_history flush and previously selected the prior turn's
        // digests. Only assistants AFTER the last user message are eligible.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let transcript = r#"{"type":"user","content":"first"}
{"type":"assistant","content":"> Previous turn digests only."}
{"type":"user","content":"second"}
{"type":"assistant","content":"tool status without digests yet"}
"#;
        std::fs::write(&tx_path, transcript).unwrap();

        // No digests in the current turn → must NOT fall back to previous turn.
        let hook = StopHook {
            session_id: Some("sess-prev".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let text = select_grok_stop_text(&hook, &paths, "sess-prev", true);
        assert_eq!(
            text.as_ref().map(|(t, _)| t.as_str()),
            Some("tool status without digests yet"),
            "previous turn digests must not be selected, got {text:?}"
        );
        assert!(
            !text.unwrap().0.contains("Previous turn"),
            "must not re-voice previous digests"
        );

        // After current digests land, they win.
        let transcript2 = r#"{"type":"user","content":"first"}
{"type":"assistant","content":"> Previous turn digests only."}
{"type":"user","content":"second"}
{"type":"assistant","content":"> Current turn digests."}
"#;
        std::fs::write(&tx_path, transcript2).unwrap();
        let text2 = select_grok_stop_text(&hook, &paths, "sess-prev", true);
        assert_eq!(
            text2.as_ref().map(|(t, _)| t.as_str()),
            Some("> Current turn digests.")
        );
    }

    #[test]
    fn grok_stop_fingerprint_not_committed_by_selection_alone() {
        // Selecting digests must not write the fingerprint; only successful enqueue does.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"user","content":"q"}
{"type":"assistant","content":"> Fingerprint gate digests."}
"#,
        )
        .unwrap();
        let hook = StopHook {
            session_id: Some("fp-sess".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (text, fp) = select_grok_stop_text(&hook, &paths, "fp-sess", true).expect("select");
        assert!(text.contains("Fingerprint gate"));
        assert!(fp.is_some());
        assert!(
            load_last_spoken_fingerprint(&paths, "fp-sess").is_none(),
            "selection alone must not commit the fingerprint"
        );
        store_last_spoken_fingerprint(&paths, "fp-sess", fp.unwrap());
        // After commit, same digests are skipped (shorts fallback empty / wait then status).
        let again = select_grok_stop_text(&hook, &paths, "fp-sess", true);
        // Only digests in the turn; after fingerprint match, shorts_fallback stays None
        // when the only assistant is digest-bearing (no non-digest fallback).
        assert!(
            again.is_none()
                || again
                    .as_ref()
                    .is_some_and(|(t, _)| !t.contains("Fingerprint gate")),
            "committed fingerprint must not re-select the same digests: {again:?}"
        );
    }

    #[test]
    fn grok_stop_fingerprint_allows_identical_digests_on_a_new_turn() {
        // Same digest body on a later user turn must still speak (turn-scoped fingerprint).
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"user","content":"first prompt"}
{"type":"assistant","content":"> Done."}
"#,
        )
        .unwrap();
        let hook = StopHook {
            session_id: Some("same-body".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (_text, fp) =
            select_grok_stop_text(&hook, &paths, "same-body", true).expect("first turn");
        store_last_spoken_fingerprint(&paths, "same-body", fp.unwrap());

        std::fs::write(
            &tx_path,
            r#"{"type":"user","content":"first prompt"}
{"type":"assistant","content":"> Done."}
{"type":"user","content":"second prompt"}
{"type":"assistant","content":"> Done."}
"#,
        )
        .unwrap();
        let (text2, fp2) =
            select_grok_stop_text(&hook, &paths, "same-body", true).expect("second turn");
        assert_eq!(text2, "> Done.");
        assert_ne!(
            fp2, fp,
            "new turn with identical digest body must not reuse the prior fingerprint"
        );
    }

    #[test]
    fn grok_stop_shorts_only_does_not_wait_for_digests_when_digests_off() {
        // With digests mode off, a non-digest status line is returned immediately
        // (production otherwise used a short secondary budget; digests-off is zero wait).
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"user","content":"q"}
{"type":"assistant","content":"plain short reply without digests"}
"#,
        )
        .unwrap();
        let hook = StopHook {
            session_id: Some("shorts-sess".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (text, fp) =
            select_grok_stop_text(&hook, &paths, "shorts-sess", false).expect("shorts");
        assert_eq!(text, "plain short reply without digests");
        assert!(fp.is_none());
    }

    #[test]
    fn grok_stop_prefers_non_digest_when_digests_mode_off() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(
            &tx_path,
            r#"{"type":"user","content":"q"}
{"type":"assistant","content":"> Digest only."}
{"type":"assistant","content":"status without digests"}
"#,
        )
        .unwrap();
        let hook = StopHook {
            session_id: Some("shorts-pref".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (text, _) =
            select_grok_stop_text(&hook, &paths, "shorts-pref", false).expect("status");
        assert_eq!(
            text, "status without digests",
            "digests-off must prefer non-digest body over a digest-bearing final"
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

        let paths = Paths::rooted_at(dir.path());
        assert!(
            hook.last_assistant_text(ClientSource::ClaudeCode, &paths)
                .is_none(),
            "non-Grok hook payloads must not cause arbitrary transcript reads"
        );
    }

    #[test]
    fn encode_grok_session_cwd_matches_live_folder_names() {
        assert_eq!(
            encode_grok_session_cwd(r"C:\Users\usr"),
            "C%3A%5CUsers%5Cusr"
        );
    }

    #[test]
    fn grok_stop_redirects_updates_jsonl_transcript_to_chat_history() {
        // Live Grok Stop (2026-07-16): transcriptPath ends in updates.jsonl — the ACP
        // event stream — which has no type:assistant lines. Digests live in the sibling
        // chat_history.jsonl.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let sess = dir.path().join("sess");
        std::fs::create_dir_all(&sess).unwrap();
        let updates = sess.join("updates.jsonl");
        let chat = sess.join("chat_history.jsonl");
        std::fs::write(&updates, r#"{"timestamp":1,"method":"session/update"}"#).unwrap();
        std::fs::write(
            &chat,
            r#"{"type":"assistant","content":"> From chat_history not updates.\n\nBody."}"#,
        )
        .unwrap();

        let hook = StopHook {
            session_id: Some("sess".into()),
            transcript_path: Some(updates.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let resolved = resolve_grok_transcript_path(&hook, &paths).expect("resolve");
        assert_eq!(resolved, chat);
        let text = hook
            .last_assistant_text(ClientSource::Grok, &paths)
            .expect("digests from chat_history");
        assert!(text.contains("> From chat_history not updates."));
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
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        std::fs::write(&tx_path, transcript).unwrap();

        let hook = StopHook {
            session_id: Some("utf8".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        assert_eq!(
            select_grok_stop_text(&hook, &paths, "utf8", true)
                .as_ref()
                .map(|(t, _)| t.as_str()),
            Some("> Final digest.")
        );
    }

    #[test]
    fn grok_stop_session_tag_is_distinct_from_real_session() {
        assert_eq!(
            grok_stop_session_tag("abc-123"),
            "grok-stop:abc-123"
        );
        assert_ne!(grok_stop_session_tag("abc-123"), "abc-123");
    }
}
