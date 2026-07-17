//! Notify-side narration — Claude/Qwen adapter over `ds-narrate`. Dispatched from
//! [`crate::hook_core::notify`]. Parses hook payload → client-neutral `StreamBatch` →
//! [`ds_narrate::deliver_batch`] → engine.
//!
//! [`message_display`]: Claude sends per-batch `delta` + `index` + sticky `final`; Qwen
//! sends cumulative `displayed_text` + `is_final` — same [`MessageDisplayHook`] (serde
//! aliases). digests → top-level blockquotes once each; shorts → short blockquote-less
//! final whole. Fire-and-forget.
//!
//! [`speak_reply`] (Stop): non-streaming final reply, gated by streaming witness so it
//! never double-speaks MessageDisplay / Codex app-server narration. [`mark_streaming_session`]
//! seeds the witness at SessionStart for streaming clients; non-streaming pass `--greet-only`.
//! Codex witness is seeded by the engine on app-server `thread/resume` — plain-TUI keeps Stop.
//!
//! [`barge_session`] (SessionEnd): scoped barge for this session only (`None` → global).
//! `narrate` is a set of "digests"/"shorts" (VoiceConfig).

use ds_config::{ClientSource, NarrateKind, Paths, VoiceConfig};
use ds_narrate::{BatchPayload, StreamBatch};
use serde::Deserialize;
use std::borrow::Cow;

/// SessionEnd: barge this session only (no id → global). Stamps `client` on the request.
pub fn barge_session(paths: &Paths, payload: &str, client: ClientSource) {
    let session = crate::hook_core::session_id_from_payload(payload);
    let _ = ds_ipc::request(
        &paths.engine_sock,
        &ds_ipc::Request::SessionEnd {
            session: session.clone(),
            source: client,
        },
    );
    // Grok Stop digests use sticky session tag (see `grok_stop_session_tag`) so MarkActive
    // cannot prune them; SessionEnd must barge that tag too (SessionEnd, not only StopSpeech,
    // so pool / forget_narration_session state is reclaimed).
    if client == ClientSource::Grok {
        let sticky = session
            .as_deref()
            .map(grok_stop_session_tag)
            .unwrap_or_else(|| "grok-stop".into());
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
    // Terminal for this session: reclaim display-state + lock/tmp or they accumulate forever.
    // Codex has no SessionEnd hook — cleanup is engine codex_stream.
    if let Some(s) = &session {
        ds_narrate::clear_session_state(paths, s);
    }
}

/// SessionStart: seed streaming witness so Stop's `streamed` guard is true before first Stop
/// (closes double-narration race). Streaming clients wire plain notify; Codex uses
/// `--greet-only` and engine seeds on app-server resume instead. Idempotent, non-destructive.
pub fn mark_streaming_session(paths: &Paths, payload: &str) {
    let Some(session) = crate::hook_core::session_id_from_payload(payload) else {
        return; // no session id ⇒ can't scope a witness (per-batch write still covers it)
    };
    ds_narrate::seed_witness(paths, &session);
}

// ── Stop (final reply — non-streaming) ──────────────────────────────────────────

/// Stop payload subset. CC/Codex/Qwen supply `last_assistant_message`. Grok is metadata-only
/// (live-verified) — fall back to `transcriptPath` chat_history.jsonl. CamelCase aliases
/// for forward-compat.
#[derive(Debug, Deserialize, Default)]
struct StopHook {
    #[serde(default, alias = "lastAssistantMessage")]
    last_assistant_message: Option<String>,
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default, alias = "transcriptPath")]
    transcript_path: Option<String>,
    /// Live Grok per-turn id — dedupe direct text without suppressing a later identical turn.
    #[serde(default, alias = "promptId")]
    prompt_id: Option<String>,
    /// Reconstruct `~/.grok/sessions/<encoded-cwd>/<sessionId>/chat_history.jsonl` when path missing.
    #[serde(default)]
    cwd: Option<String>,
}

/// JSONL assistant/user line from Grok chat_history (or similar).
#[derive(Debug, Deserialize, Default)]
struct TranscriptEntry {
    #[serde(default, rename = "type")]
    r#type: Option<String>,
    /// Plain string or content-block array; other shapes skip the line.
    #[serde(default)]
    content: Option<serde_json::Value>,
}

impl TranscriptEntry {
    fn text_content(&self) -> Option<String> {
        match &self.content {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
            Some(serde_json::Value::Array(parts)) => {
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
    /// Direct field, else Grok transcript fallback.
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
            // Tests / non-speak_reply paths assume digests on (live default).
            return select_grok_stop_text(self, paths, session, true)
                .map(|(text, _fp)| Cow::Owned(text));
        }
        None
    }
}

/// Sticky admit key for Grok Stop digests + reply_done so MarkActive cannot prune them;
/// SessionEnd barges this tag explicitly.
fn grok_stop_session_tag(session: &str) -> String {
    format!("grok-stop:{session}")
}

/// Grok chat transcript path. Order: (1) transcriptPath, remapping updates.jsonl → sibling
/// chat_history (bare updates is non-terminal — fall through); (2) encoded-cwd+session under
/// `~/.grok/sessions`; (3) scan `sessions/*/sessionId/chat_history` (newest mtime on skew).
fn resolve_grok_transcript_path(hook: &StopHook, paths: &Paths) -> Option<std::path::PathBuf> {
    if let Some(raw) = hook
        .transcript_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let p = std::path::PathBuf::from(raw);
        if p.is_file() {
            let preferred = ds_config::prefer_chat_history_transcript(p);
            // Bare updates.jsonl must not shadow a valid sessions/.../chat_history for the budget.
            if !ds_config::is_updates_jsonl(&preferred) {
                return Some(preferred);
            }
        } else {
            let chat = p.with_file_name("chat_history.jsonl");
            if chat.is_file() {
                return Some(chat);
            }
        }
    }
    let session = hook
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let cwd = hook.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
    ds_config::resolve_grok_chat_history(paths, session, cwd)
}

/// Prefer digest-bearing assistant over a newer tool-status line in "last non-empty" scans.
fn has_digest_blockquote(text: &str) -> bool {
    !ds_config::all_blockquotes(text).is_empty()
}

/// Turn fingerprint so re-fired Stop does not re-voice after successful enqueue. Transcript
/// selections use absolute assistant-line byte offset so identical body on a later turn still
/// speaks, and a sliding 256 KiB tail cannot rewrite identity when the user line leaves the window.
fn digest_fingerprint(
    last_user_text: &str,
    turn_byte_offset: Option<u64>,
    digest_text: &str,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    last_user_text.trim().hash(&mut h);
    turn_byte_offset.hash(&mut h);
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
    // Concurrent Stop may race; temp+rename is best-effort (Windows rename-over may fail).
    let tmp = path.with_extension("fp.tmp");
    if std::fs::write(&tmp, fp.to_string()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::write(&path, fp.to_string());
        let _ = std::fs::remove_file(&tmp);
    }
}

#[derive(Debug)]
enum ChatRole {
    User {
        text: String,
        byte_offset: u64,
    },
    Assistant {
        text: String,
        /// Fallback turn identity when a large tool result pushes the user line out of the tail.
        byte_offset: u64,
    },
}

/// Bounded-tail JSONL roles (oldest first). Discards partial first line after seek (UTF-8 split).
/// Offsets are absolute file positions for stable fingerprints.
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
    let (complete_lines, complete_start_abs) = if start == 0 {
        (tail.as_slice(), 0u64)
    } else {
        let Some(first_newline) = tail.iter().position(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        (
            &tail[first_newline + 1..],
            start + (first_newline as u64) + 1,
        )
    };

    let mut out = Vec::new();
    let mut line_abs = complete_start_abs;
    for line in complete_lines.split(|byte| *byte == b'\n') {
        let line_start = line_abs;
        line_abs += line.len() as u64 + 1;
        let Ok(entry) = serde_json::from_slice::<TranscriptEntry>(line) else {
            continue;
        };
        match entry.r#type.as_deref() {
            Some("user") => {
                // Keep empty text so the turn boundary still moves.
                out.push(ChatRole::User {
                    text: entry.text_content().unwrap_or_default(),
                    byte_offset: line_start,
                });
            }
            Some("assistant") => {
                if let Some(text) = entry.text_content() {
                    out.push(ChatRole::Assistant {
                        text,
                        byte_offset: line_start,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Debug, Default)]
struct CurrentTurn {
    last_user_text: String,
    last_user_byte_offset: Option<u64>,
    assistants_newest_first: Vec<CurrentAssistant>,
}

#[derive(Debug, Clone)]
struct CurrentAssistant {
    text: String,
    byte_offset: u64,
}

/// Assistants after the last user only (newest first). Never crosses into a previous turn —
/// live bug was re-playing prior digests when Stop raced the chat_history flush.
fn current_turn_from_path(path: &std::path::Path) -> CurrentTurn {
    let roles = chat_roles_chronological(path);
    let last_user_idx = roles
        .iter()
        .rposition(|r| matches!(r, ChatRole::User { .. }));
    let after = last_user_idx.map(|i| i + 1).unwrap_or(0);
    let (last_user_text, last_user_byte_offset) = last_user_idx
        .and_then(|i| match &roles[i] {
            ChatRole::User { text, byte_offset } => Some((text.clone(), Some(*byte_offset))),
            _ => None,
        })
        .unwrap_or_default();
    let assistants_newest_first = roles[after..]
        .iter()
        .rev()
        .filter_map(|r| match r {
            ChatRole::Assistant { text, byte_offset } => Some(CurrentAssistant {
                text: text.clone(),
                byte_offset: *byte_offset,
            }),
            ChatRole::User { .. } => None,
        })
        .collect();
    CurrentTurn {
        last_user_text,
        last_user_byte_offset,
        assistants_newest_first,
    }
}

/// Retry while path/turn empty (late chat_history flush). Tests use a small count.
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

fn stop_retry_delay() -> std::time::Duration {
    #[cfg(test)]
    {
        std::time::Duration::from_millis(10)
    }
    #[cfg(not(test))]
    {
        std::time::Duration::from_millis(100)
    }
}

/// Grok Stop selection; fingerprint committed only after enqueue success.
struct GrokStopSelection {
    text: String,
    digest_fp: Option<u64>,
    path: std::path::PathBuf,
}

/// Grok Stop narration from transcript. Live rules: re-resolve path each attempt; current
/// turn only; prefer digest-bearing assistant when digests on; skip last successfully-spoken
/// fingerprint; shorts fallback; full budget while empty. Does not persist fingerprints.
fn select_grok_stop_text(
    hook: &StopHook,
    paths: &Paths,
    session: &str,
    messages_on: bool,
) -> Option<(String, Option<u64>)> {
    select_grok_stop_text_detailed(hook, paths, session, messages_on).map(|s| (s.text, s.digest_fp))
}

/// Shorts fallback: when digests off, prefer non-digest line (digest-only would silence shorts).
fn shorts_fallback_text(turn: &CurrentTurn, messages_on: bool) -> Option<String> {
    if messages_on {
        return turn
            .assistants_newest_first
            .first()
            .map(|assistant| assistant.text.clone());
    }
    turn.assistants_newest_first
        .iter()
        .find(|assistant| !has_digest_blockquote(&assistant.text))
        .map(|assistant| assistant.text.clone())
        .or_else(|| {
            turn.assistants_newest_first
                .first()
                .map(|assistant| assistant.text.clone())
        })
}

/// Direct Grok reply fingerprint: promptId, else transcript offset, else text-only.
fn direct_grok_reply_fingerprint(hook: &StopHook, paths: &Paths, text: &str) -> u64 {
    if let Some(prompt_id) = hook
        .prompt_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return digest_fingerprint(&format!("prompt-id:{prompt_id}"), None, text);
    }
    if let Some(path) = resolve_grok_transcript_path(hook, paths) {
        let turn = current_turn_from_path(&path);
        if let Some(assistant) = turn
            .assistants_newest_first
            .iter()
            .find(|assistant| assistant.text.trim() == text.trim())
        {
            return digest_fingerprint("", Some(assistant.byte_offset), text);
        }
        let offset = turn.last_user_byte_offset.or_else(|| {
            turn.assistants_newest_first
                .first()
                .map(|assistant| assistant.byte_offset)
        });
        return digest_fingerprint(&turn.last_user_text, offset, text);
    }
    digest_fingerprint("direct-text-only", None, text)
}

fn select_grok_stop_text_detailed(
    hook: &StopHook,
    paths: &Paths,
    session: &str,
    messages_on: bool,
) -> Option<GrokStopSelection> {
    let delay = stop_retry_delay();
    select_grok_stop_text_detailed_with_retry(
        hook,
        paths,
        session,
        messages_on,
        stop_retry_attempts(),
        |_| std::thread::sleep(delay),
    )
}

fn select_grok_stop_text_detailed_with_retry(
    hook: &StopHook,
    paths: &Paths,
    session: &str,
    messages_on: bool,
    attempts: usize,
    mut retry_wait: impl FnMut(usize),
) -> Option<GrokStopSelection> {
    let last_fp = load_last_spoken_fingerprint(paths, session);
    let mut shorts_fallback: Option<GrokStopSelection> = None;

    for attempt in 0..attempts {
        let Some(path) = resolve_grok_transcript_path(hook, paths) else {
            if attempt + 1 < attempts {
                retry_wait(attempt);
            }
            continue;
        };
        let turn = current_turn_from_path(&path);

        if messages_on
            && let Some(digest) = turn
                .assistants_newest_first
                .iter()
                .find(|assistant| has_digest_blockquote(&assistant.text))
        {
            let fp = digest_fingerprint("", Some(digest.byte_offset), &digest.text);
            if last_fp != Some(fp) {
                return Some(GrokStopSelection {
                    text: digest.text.clone(),
                    digest_fp: Some(fp),
                    path,
                });
            }
            // Same as last spoken: keep full retry (newer flush may still land).
        } else if let Some(text) = shorts_fallback_text(&turn, messages_on)
            && shorts_fallback.is_none()
        {
            shorts_fallback = Some(GrokStopSelection {
                text,
                digest_fp: None,
                path: path.clone(),
            });
            if !messages_on {
                return shorts_fallback;
            }
        }

        if attempt + 1 < attempts {
            retry_wait(attempt);
        }
    }
    shorts_fallback
}

/// Streaming-pass witness for Stop: state file exists. CC/Qwen seed at SessionStart;
/// Codex file appears only when engine app-server resumed the thread — else Stop narrates.
/// `pub(crate)` for hook_core greet-only tests.
pub(crate) fn streamed_via_message_display(paths: &Paths, session: &str) -> bool {
    ds_narrate::witness_exists(paths, session)
}

/// Pure Stop decision (re-export so hook_core double-narration tests keep this seam).
pub(crate) use ds_narrate::stop_utterances;

/// Stop: voice final reply once when not already streamed. Guarded by
/// [`streamed_via_message_display`]; pure decision in [`stop_utterances`].
/// Returns `Some(session)` for reply_done under Grok sticky tag; `None` → payload session.
pub fn speak_reply(paths: &Paths, payload: &str, client: ClientSource) -> Option<Option<String>> {
    let cfg = VoiceConfig::load(paths);
    let messages_on = cfg.narrates(NarrateKind::Digests);
    let short_on = cfg.narrates(NarrateKind::Shorts);
    if !messages_on && !short_on {
        return None;
    }
    let Ok(hook) = serde_json::from_str::<StopHook>(payload.trim()) else {
        return None;
    };
    let session = hook.session_id.clone().filter(|s| !s.trim().is_empty());
    let streamed = streamed_via_message_display(paths, session.as_deref().unwrap_or_default());

    // Final retry for queue-full rejections; witness still suppresses whole-reply fallback.
    // Grok mid-turn (engine file-tail): also flush trailing digests/shorts with is_final.
    // Do NOT re-voice chat_history when the witness is present.
    if streamed {
        let session_id = session.as_deref().unwrap_or_default();
        let session_tag = session.clone();
        if let Err(message) = ds_narrate::retry_pending(paths, session_id, |utterance| {
            admit_narration(paths, session_tag.clone(), client, utterance)
        }) {
            eprintln!("dontspeak: narration rejected: {message}");
        }
        if client == ClientSource::Grok {
            let key = hook
                .prompt_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(session_id)
                .to_string();
            let final_batch = StreamBatch {
                key,
                payload: BatchPayload::Delta {
                    index: None,
                    text: String::new(),
                },
                is_final: true,
            };
            if let Err(message) = ds_narrate::deliver_batch(
                paths,
                session_id,
                &final_batch,
                ds_platform::is_mic_active(),
                messages_on,
                short_on,
                |utterance| admit_narration(paths, session_tag.clone(), client, utterance),
            ) {
                eprintln!("dontspeak: narration rejected: {message}");
            }
        }
    }

    let mic_active = ds_platform::is_mic_active();

    // Grok: direct lastAssistantMessage if present; else turn-scoped transcript + deferred fp.
    // When already streamed mid-turn, skip chat_history selection (witness owns silence).
    let (assistant_text, grok_selection, direct_fp): (
        Option<String>,
        Option<GrokStopSelection>,
        Option<u64>,
    ) = if client == ClientSource::Grok {
        if streamed {
            (None, None, None)
        } else if let Some(direct) = hook
            .last_assistant_message
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let sess = session.as_deref().unwrap_or("-");
            let fp = direct_grok_reply_fingerprint(&hook, paths, direct);
            if load_last_spoken_fingerprint(paths, sess) == Some(fp) {
                (None, None, None)
            } else {
                (Some(direct.to_owned()), None, Some(fp))
            }
        } else {
            let sess = session.as_deref().unwrap_or("-");
            match select_grok_stop_text_detailed(&hook, paths, sess, messages_on) {
                Some(sel) => (Some(sel.text.clone()), Some(sel), None),
                None => (None, None, None),
            }
        }
    } else {
        (
            hook.last_assistant_text(client, paths)
                .map(|c| c.into_owned()),
            None,
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
    // Sticky tag: MarkActive cannot prune digests (ding-only race); barge_session clears it.
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
    // Deterministic ids collapse concurrent Stop at engine admission (fp alone is sequential).
    let narration_fp = direct_fp.or_else(|| grok_selection.as_ref().and_then(|s| s.digest_fp));
    let real_sess = session.as_deref().unwrap_or("-");
    for (i, line) in speak.into_iter().enumerate() {
        let narration_id = narration_fp.map(|fp| format!("grok-stop:{real_sess}:{fp}:{i}"));
        match ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SpeakNarration {
                text: line,
                session: admit_session.clone(),
                narration_id,
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
    // Commit fingerprint only after full list admitted — partial multi-line must not skip rest.
    if any_enqueued
        && !any_failed
        && let (Some(fp), Some(s)) = (narration_fp, session.as_deref())
    {
        store_last_spoken_fingerprint(paths, s, fp);
    }
    // Grok: reply_done under sticky session so digests play before the ding.
    if client == ClientSource::Grok {
        Some(admit_session)
    } else {
        None
    }
}

// ── MessageDisplay (speak-as-it-streams) ────────────────────────────────────────

/// Two clients, one struct: CC incremental `delta` (+ optional cumulative displayedText);
/// Qwen cumulative snake_case via aliases.
#[derive(Debug, Deserialize, Default, Clone)]
struct MessageDisplayHook {
    #[serde(default, rename = "displayedText", alias = "displayed_text")]
    displayed_text: Option<String>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    // New-message key (not order — that's `index`).
    #[serde(default)]
    message_id: Option<String>,
    // Order only: CC spawns a process per batch; they race.
    #[serde(default)]
    index: Option<u64>,
    #[serde(default, rename = "final", alias = "is_final")]
    is_final: Option<bool>,
}

/// Fallback key when no `message_id` — opening text differs per message.
fn message_key(s: &str) -> String {
    s.chars().take(48).collect()
}

/// Hook → client-neutral [`StreamBatch`]. Cumulative `displayed_text` wins over `delta`.
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

/// MessageDisplay: narrate this streamed batch. Gates on `narrate` + not-mid-recording —
/// not focus; session-tagged, engine holds background terminals. Fire-and-forget.
pub fn message_display(paths: &Paths, payload: &str, client: ClientSource) {
    let cfg = VoiceConfig::load(paths);
    let messages_on = cfg.narrates(NarrateKind::Digests);
    let short_on = cfg.narrates(NarrateKind::Shorts);
    if !messages_on && !short_on {
        return;
    }
    let Ok(hook) = serde_json::from_str::<MessageDisplayHook>(payload.trim()) else {
        return;
    };
    let session = hook.session_id.clone().unwrap_or_default();
    let batch = batch_from_hook(&hook);

    // Lock held through admission so races don't double-offer; rejected work stays pending.
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

/// Test seam: hook → pure step. Production uses [`ds_narrate::deliver_batch`].
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

    // Adapter-only: hook payload → StreamBatch. Accum/stream speaking lives in ds-narrate.

    #[test]
    fn batch_from_hook_prefers_message_id_and_maps_delta() {
        let batch = batch_from_hook(&MessageDisplayHook {
            delta: Some("> Spoken.".into()),
            message_id: Some("m1".into()),
            index: Some(2),
            is_final: Some(true),
            ..Default::default()
        });
        assert_eq!(batch.key, "m1");
        assert!(batch.is_final);
        match batch.payload {
            BatchPayload::Delta { index, text } => {
                assert_eq!(index, Some(2));
                assert_eq!(text, "> Spoken.");
            }
            other => panic!("expected delta, got {other:?}"),
        }
    }

    #[test]
    fn batch_from_hook_falls_back_to_message_key_without_id() {
        let text = "x".repeat(80);
        let batch = batch_from_hook(&MessageDisplayHook {
            delta: Some(text.clone()),
            ..Default::default()
        });
        assert_eq!(batch.key, message_key(&text));
        assert!(!batch.is_final);
    }

    #[test]
    fn batch_from_hook_cumulative_wins_over_delta() {
        let batch = batch_from_hook(&MessageDisplayHook {
            displayed_text: Some("> Cumul.\n\nBody.".into()),
            delta: Some("ignored".into()),
            message_id: Some("c1".into()),
            is_final: Some(false),
            ..Default::default()
        });
        match batch.payload {
            BatchPayload::Cumulative { text } => assert_eq!(text, "> Cumul.\n\nBody."),
            other => panic!("expected cumulative, got {other:?}"),
        }
    }

    /// Pure step driver for adapter→ds-narrate smoke (Qwen serde path).
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

    // Qwen: snake_case cumulative snapshots via the same serde aliases.

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

    // Stop double-narration witness (pure stop_utterances lives in ds-narrate).

    const DIGEST_REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.";

    #[test]
    fn streamed_witness_tracks_the_message_display_state_file() {
        // Session-scoped; same path as writer so witness can't drift.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let cc = "cc-session-aaaa";
        let codex = "codex-session-bbbb";

        assert!(!streamed_via_message_display(&paths, cc));

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
        // Seed before first Stop even with no MessageDisplay yet (session-scoped).
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-cccc";

        assert!(!streamed_via_message_display(&paths, session));
        mark_streaming_session(&paths, &format!(r#"{{"session_id":"{session}"}}"#));
        assert!(
            streamed_via_message_display(&paths, session),
            "SessionStart must seed the witness before any MessageDisplay batch"
        );
        assert!(
            stop_utterances(Some(DIGEST_REPLY), true, true, false, true).is_empty(),
            "seeded session ⇒ Stop stays silent"
        );
    }

    #[test]
    fn session_start_seed_is_non_destructive_and_needs_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "cc-session-dddd";

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

        mark_streaming_session(&paths, "{}");
        assert!(
            !streamed_via_message_display(&paths, ""),
            "a session-less SessionStart seeds nothing"
        );
    }

    #[test]
    fn barge_session_clears_the_state_file_trio() {
        // SessionEnd reclaims state + lock/tmp (engine ping is no-op on missing socket).
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

    // Grok Stop (live-verified fields).
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
        // Sanitized live Grok 0.2.93 capture (2026-07-13).
        let payload = r#"{"hookEventName":"stop","sessionId":"g-real","cwd":"C:\\Users\\usr","transcriptPath":"...","promptId":"p1","reason":"end_turn"}"#;
        assert_eq!(crate::hook_core::event_name(payload), "Stop");
        let hook: StopHook = serde_json::from_str(payload).expect("real Grok Stop parses");
        assert_eq!(hook.session_id.as_deref(), Some("g-real"));
        assert_eq!(hook.prompt_id.as_deref(), Some("p1"));
        assert!(
            hook.last_assistant_message.is_none(),
            "Grok Stop carries no last* text"
        );
        assert!(
            hook.last_assistant_text(ClientSource::Grok, &paths)
                .is_none()
        );
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
        // #49: transcriptPath → last assistant digests.
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
            .join(ds_config::encode_grok_session_cwd(cwd))
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
        // Agentic: digests then tool-status — pure last-nonempty would ding-only.
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
        assert_eq!(
            spoken.len(),
            1,
            "adjacent > lines form one digest run: {spoken:?}"
        );
        assert!(
            spoken[0].contains("This is a DontSpeak digest check.")
                && spoken[0].contains("If Stop narration works"),
            "spoken={spoken:?}"
        );
    }

    #[test]
    fn grok_stop_uses_full_retry_budget_after_a_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let pending = r#"{"type":"user","content":"wait for final digests"}
{"type":"assistant","content":"tool status while the final answer flushes"}
"#;
        let complete = r#"{"type":"user","content":"wait for final digests"}
{"type":"assistant","content":"tool status while the final answer flushes"}
{"type":"assistant","content":"> Final digest after the status line."}
"#;
        std::fs::write(&tx_path, pending).unwrap();
        let hook = StopHook {
            session_id: Some("late-digest".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };

        let selected = select_grok_stop_text_detailed_with_retry(
            &hook,
            &paths,
            "late-digest",
            true,
            3,
            |attempt| {
                // Digest lands after second observation; short status budget used to return early.
                if attempt == 1 {
                    std::fs::write(&tx_path, complete).unwrap();
                }
            },
        )
        .expect("late digest must win over the provisional status fallback");
        assert_eq!(selected.text, "> Final digest after the status line.");
        assert!(selected.digest_fp.is_some());
    }

    #[test]
    fn grok_stop_uses_full_retry_budget_after_a_spoken_digest() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let first = r#"{"type":"user","content":"wait for the complete final"}
{"type":"assistant","content":"> Already spoken partial digest."}
"#;
        let complete = r#"{"type":"user","content":"wait for the complete final"}
{"type":"assistant","content":"> Already spoken partial digest."}
{"type":"assistant","content":"> New digest from the completed flush."}
"#;
        std::fs::write(&tx_path, first).unwrap();
        let hook = StopHook {
            session_id: Some("late-replacement".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (_, first_fp) =
            select_grok_stop_text(&hook, &paths, "late-replacement", true).expect("initial digest");
        store_last_spoken_fingerprint(&paths, "late-replacement", first_fp.unwrap());

        let selected = select_grok_stop_text_detailed_with_retry(
            &hook,
            &paths,
            "late-replacement",
            true,
            3,
            |attempt| {
                // Flush lands next boundary; old signature shortcut returned too early.
                if attempt == 1 {
                    std::fs::write(&tx_path, complete).unwrap();
                }
            },
        )
        .expect("newer digest must remain eligible for the full retry window");
        assert_eq!(selected.text, "> New digest from the completed flush.");
    }

    #[test]
    fn grok_stop_does_not_revoice_previous_turn_digests() {
        // Stop raced flush and re-played prior turn; only post-last-user assistants count.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let transcript = r#"{"type":"user","content":"first"}
{"type":"assistant","content":"> Previous turn digests only."}
{"type":"user","content":"second"}
{"type":"assistant","content":"tool status without digests yet"}
"#;
        std::fs::write(&tx_path, transcript).unwrap();

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
        // Fingerprint only after successful enqueue, not on select.
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
        let again = select_grok_stop_text(&hook, &paths, "fp-sess", true);
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
        // Same body, later user turn — turn-scoped fp must still speak.
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
    fn grok_stop_fingerprint_uses_assistant_offset_when_user_is_outside_tail() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let large_tool_line = serde_json::json!({
            "type": "tool_result",
            "content": "x".repeat(300 * 1024),
        })
        .to_string();
        let first_turn = format!(
            "{}\n{large_tool_line}\n{}\n",
            serde_json::json!({"type": "user", "content": "same prompt"}),
            serde_json::json!({"type": "assistant", "content": "> Same digest."}),
        );
        std::fs::write(&tx_path, &first_turn).unwrap();
        let hook = StopHook {
            session_id: Some("tail-turn".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (_, first_fp) =
            select_grok_stop_text(&hook, &paths, "tail-turn", true).expect("first turn");
        let first_fp = first_fp.expect("digest fingerprint");
        store_last_spoken_fingerprint(&paths, "tail-turn", first_fp);

        let second_turn = format!(
            "{}\n{large_tool_line}\n{}\n",
            serde_json::json!({"type": "user", "content": "same prompt"}),
            serde_json::json!({"type": "assistant", "content": "> Same digest."}),
        );
        std::fs::write(&tx_path, format!("{first_turn}{second_turn}")).unwrap();

        let (text, second_fp) = select_grok_stop_text(&hook, &paths, "tail-turn", true)
            .expect("identical digest in a later tail-truncated turn must speak");
        assert_eq!(text, "> Same digest.");
        assert_ne!(second_fp, Some(first_fp));
    }

    #[test]
    fn grok_stop_fingerprint_stays_stable_when_user_leaves_tail() {
        const TAIL_BYTES: usize = 256 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let tx_path = dir.path().join("chat_history.jsonl");
        let user_line = format!(
            "{}\n",
            serde_json::json!({"type": "user", "content": "stable turn"})
        );
        let digest_line = format!(
            "{}\n",
            serde_json::json!({"type": "assistant", "content": "> Stable digest."})
        );
        let initial = format!("{user_line}{digest_line}");
        std::fs::write(&tx_path, &initial).unwrap();
        let hook = StopHook {
            session_id: Some("tail-stability".into()),
            transcript_path: Some(tx_path.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let (_, first_fp) =
            select_grok_stop_text(&hook, &paths, "tail-stability", true).expect("initial digest");
        store_last_spoken_fingerprint(&paths, "tail-stability", first_fp.unwrap());

        // Grow the file until the tail begins inside the user line while the already-spoken
        // assistant line remains complete and at the same absolute offset.
        let tail_start = user_line.len() / 2;
        let filler_len = TAIL_BYTES + tail_start - initial.len();
        let mut grown = initial.into_bytes();
        grown.extend(std::iter::repeat_n(b'x', filler_len));
        std::fs::write(&tx_path, grown).unwrap();

        let repeated = select_grok_stop_text_detailed_with_retry(
            &hook,
            &paths,
            "tail-stability",
            true,
            1,
            |_| {},
        );
        assert!(
            repeated.is_none(),
            "tail movement must not make the same assistant line look like a new turn"
        );
    }

    #[test]
    fn direct_grok_reply_fingerprint_is_stable_per_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let mut hook = StopHook {
            prompt_id: Some("prompt-one".into()),
            ..StopHook::default()
        };
        let text = "> Same direct reply.";
        let first = direct_grok_reply_fingerprint(&hook, &paths, text);
        assert_eq!(first, direct_grok_reply_fingerprint(&hook, &paths, text));

        hook.prompt_id = Some("prompt-two".into());
        assert_ne!(first, direct_grok_reply_fingerprint(&hook, &paths, text));
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
        let (text, _) = select_grok_stop_text(&hook, &paths, "shorts-pref", false).expect("status");
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
            ds_config::encode_grok_session_cwd(r"C:\Users\usr"),
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
    fn grok_stop_updates_without_sibling_falls_through_to_cwd_chat_history() {
        // Bare updates.jsonl (sibling not flushed yet / mislocated) must not shadow a
        // valid ~/.grok/sessions/<cwd>/<session>/chat_history.jsonl.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let cwd = r"C:\Users\usr";
        let session = "sess-fallthrough";
        let orphan = dir.path().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        let updates = orphan.join("updates.jsonl");
        std::fs::write(&updates, r#"{"timestamp":1}"#).unwrap();

        let chat_dir = paths
            .grok_dir
            .join("sessions")
            .join(ds_config::encode_grok_session_cwd(cwd))
            .join(session);
        std::fs::create_dir_all(&chat_dir).unwrap();
        let chat = chat_dir.join("chat_history.jsonl");
        std::fs::write(
            &chat,
            r#"{"type":"assistant","content":"> Via cwd not orphan updates."}"#,
        )
        .unwrap();

        let hook = StopHook {
            session_id: Some(session.into()),
            cwd: Some(cwd.into()),
            transcript_path: Some(updates.to_string_lossy().into_owned()),
            ..StopHook::default()
        };
        let resolved = resolve_grok_transcript_path(&hook, &paths).expect("cwd fallthrough");
        assert_eq!(resolved, chat);
    }

    #[test]
    fn grok_streamed_stop_finalizes_without_voicing_chat_history() {
        // Mid-turn engine tail seeds the witness; Stop must flush is_final and stay silent
        // on whole chat_history (no double-speak of digests already admitted).
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let session = "streamed-sess";
        ds_narrate::seed_witness(&paths, session);
        // One incomplete digest in state: empty final batch should flush it.
        let batch = StreamBatch {
            key: "prompt-1".into(),
            payload: BatchPayload::Delta {
                index: Some(0),
                text: "> Trailing digest without blank line".into(),
            },
            is_final: false,
        };
        let mut spoken = Vec::new();
        ds_narrate::deliver_batch(&paths, session, &batch, false, true, true, |u| {
            spoken.push(u.text.clone());
            Ok(())
        })
        .unwrap();
        assert!(
            spoken.is_empty(),
            "incomplete digest waits for is_final/blank line, got {spoken:?}"
        );

        let payload = serde_json::json!({
            "session_id": session,
            "promptId": "prompt-1",
            "transcriptPath": dir.path().join("missing-chat.jsonl").to_string_lossy(),
        })
        .to_string();
        // Engine sock absent → admit fails soft; finalize still runs deliver_batch path.
        let _ = speak_reply(&paths, &payload, ClientSource::Grok);
        assert!(
            ds_narrate::witness_exists(&paths, session),
            "witness must remain so stop_utterances stay silent"
        );
        assert!(
            stop_utterances(
                Some("> Should not re-voice from chat_history"),
                true,
                true,
                false,
                true
            )
            .is_empty()
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
        assert_eq!(grok_stop_session_tag("abc-123"), "grok-stop:abc-123");
        assert_ne!(grok_stop_session_tag("abc-123"), "abc-123");
    }
}
