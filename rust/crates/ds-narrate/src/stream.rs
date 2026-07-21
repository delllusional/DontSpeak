//! Client-neutral streaming step + file-backed per-session state (the streaming WITNESS).
//! Pipeline: [`StreamBatch`] → [`step`] (pure) → [`deliver_batch`] (lock → read → step →
//! admit → commit). Shared state file gives every adapter [`witness_exists`] for free;
//! admission-committed `offset` + stable utterance IDs make reconnect/retry dedup-safe.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ds_config::Paths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::accum::{Accum, SelectedUtterance};

/// One streamed text batch — every adapter's payload maps onto this.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamBatch {
    /// Per-message key (client message/item id, or fingerprint fallback). New key resets.
    pub key: String,
    pub payload: BatchPayload,
    /// Last batch: final run counts complete without a trailing blank line; shorts may fire.
    pub is_final: bool,
}

/// How this batch carries its text.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchPayload {
    /// Incremental chunk. `index` orders racing batches (not a message key); `None` appends
    /// after the highest seen (ordered transports / older clients).
    Delta { index: Option<u64>, text: String },
    /// Whole text so far — wins over deltas (covers missed chunks before attach).
    Cumulative { text: String },
}

/// Per-session streaming state (`narrate-display-<session>.json`). Field names are the
/// ON-DISK contract across adapter processes — don't rename.
#[derive(Debug, Deserialize, Serialize, Default, Clone, PartialEq)]
pub struct DisplayState {
    /// Blockquotes admitted to the queue. `step` selects against it; `deliver_batch`
    /// advances only after successful admission (pending is a shadow mark).
    pub offset: usize,
    pub key: String,
    /// Delta chunks by content-block `index` (empty in cumulative mode).
    #[serde(default)]
    pub parts: BTreeMap<u64, String>,
    /// Sticky final flag (survives out-of-order: final batch before the quote batch).
    #[serde(default)]
    pub seen_final: bool,
    /// Shorts latch (`Accum::short_done`).
    #[serde(default)]
    pub short_done: bool,
    /// Mic gate decided ONCE per message key — mid-stream mic flap can't strand/start.
    #[serde(default)]
    pub gate_msg: String,
    #[serde(default)]
    pub gate_on: bool,
    /// True once a gate decision was cached (vs all-empty serde default).
    #[serde(default)]
    pub gate_set: bool,
    /// Selected but not yet admitted; same lock transaction as the high-water mark so
    /// rejection retries without re-selecting or skipping.
    #[serde(default)]
    pending: Vec<PendingUtterance>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct PendingUtterance {
    id: String,
    /// Message/item id — reused as the engine language-pin `message_key`.
    key: String,
    text: String,
    /// Cumulative so-far at selection; absent on old state files → detect on `text`.
    #[serde(default)]
    detection_text: String,
    after: DeliveryCheckpoint,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum DeliveryCheckpoint {
    Offset(usize),
    Short,
}

/// Utterance offered for delivery. Retry same `id` is intentional (engine dedups);
/// high-water mark advances only after successful admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrationUtterance {
    pub id: String,
    pub text: String,
    /// Cumulative so-far for language detection (may be empty on legacy pending).
    pub detection_text: String,
    /// Message/item id for per-turn language pin (`pending.key` / `batch.key`).
    pub message_key: String,
}

/// Pure batch effect (`write = None` ⇒ leave state file alone).
pub struct DisplayStep {
    pub write: Option<DisplayState>,
    pub speak: Vec<SelectedUtterance>,
}

/// Pure: same inputs → same outputs. Per-message mic gate is keyed by `batch.key`.
pub fn step(
    prev: &DisplayState,
    batch: &StreamBatch,
    mic_active: bool,
    digests_on: bool,
    shorts_on: bool,
) -> DisplayStep {
    // Mic gate once per message key (per-batch checks stranded mid-message). Focus stays
    // with the engine (PER-TERMINAL-QUEUES); we only suppress while recording.
    let gate_on = if prev.gate_set && prev.gate_msg == batch.key {
        prev.gate_on
    } else {
        !mic_active
    };
    if !gate_on {
        return DisplayStep {
            write: Some(DisplayState {
                offset: 0,
                key: String::new(),
                parts: BTreeMap::new(),
                seen_final: false,
                short_done: false,
                gate_msg: batch.key.clone(),
                gate_on: false,
                gate_set: true,
                pending: prev.pending.clone(),
            }),
            speak: Vec::new(),
        };
    }

    // Key is message/item id alone — never a per-batch index (that dropped leading quotes).
    let same = prev.key == batch.key;

    // Hydrate Accum from prior state (`offset` ⇆ `emitted`); pure core does the emit.
    let mut accum = if same {
        Accum {
            parts: prev.parts.clone(),
            seen_final: prev.seen_final,
            emitted: prev.offset,
            short_done: prev.short_done,
        }
    } else {
        Accum::default()
    };
    let speak = match &batch.payload {
        BatchPayload::Delta { index, text } => {
            let index =
                index.unwrap_or_else(|| accum.parts.keys().next_back().map_or(0, |k| k + 1));
            accum.feed(index, text, None, batch.is_final, digests_on, shorts_on)
        }
        BatchPayload::Cumulative { text } => {
            accum.feed(0, "", Some(text), batch.is_final, digests_on, shorts_on)
        }
    };
    let next = DisplayState {
        offset: accum.emitted,
        key: batch.key.clone(),
        parts: accum.parts,
        seen_final: accum.seen_final,
        short_done: accum.short_done,
        gate_msg: batch.key.clone(),
        gate_on: true,
        gate_set: true,
        pending: prev.pending.clone(),
    };
    DisplayStep {
        write: Some(next),
        speak,
    }
}

/// Admission is the commit point. Rejected work stays pending and is retried first next
/// batch. Treat `id` as idempotent: process can die after admit but before state write.
pub fn deliver_batch(
    paths: &Paths,
    session: &str,
    batch: &StreamBatch,
    mic_active: bool,
    digests_on: bool,
    shorts_on: bool,
    mut admit: impl FnMut(&NarrationUtterance) -> Result<(), String>,
) -> Result<(), String> {
    let state_path = display_state_path(paths, session);
    ensure_state_parent(&state_path);
    with_state_lock(&state_path, || {
        let prev = read_state(&state_path);

        // Selection mark (with pending as shadow); serialized `offset` is admission-committed.
        let mut selected = prev.clone();
        for pending in prev
            .pending
            .iter()
            .filter(|pending| pending.key == prev.key)
        {
            match pending.after {
                DeliveryCheckpoint::Offset(offset) => selected.offset = selected.offset.max(offset),
                DeliveryCheckpoint::Short => selected.short_done = true,
            }
        }
        let selected_before = if selected.key == batch.key {
            selected.offset
        } else {
            0
        };
        let short_before = selected.key == batch.key && selected.short_done;
        let decided = step(&selected, batch, mic_active, digests_on, shorts_on);
        let newly_selected = decided.speak;
        let Some(mut next) = decided.write else {
            return admit_pending(&state_path, prev, &mut admit);
        };
        let selected_after = next.offset;
        let short_after = next.short_done;

        next.pending = prev.pending.clone();
        next.offset = if prev.key == next.key { prev.offset } else { 0 };
        next.short_done = prev.key == next.key && prev.short_done;

        let block_count = selected_after.saturating_sub(selected_before);
        if block_count > 0 && !digests_on {
            debug_assert!(newly_selected.is_empty());
            next.offset = selected_after;
        } else if block_count == newly_selected.len() {
            for (index, selected) in newly_selected.into_iter().enumerate() {
                let after = DeliveryCheckpoint::Offset(selected_before + index + 1);
                next.pending
                    .push(pending_utterance(session, &next.key, selected, after));
            }
            if block_count == 0 {
                next.offset = selected_after;
            }
        } else if block_count == 0 && !short_before && short_after && newly_selected.len() == 1 {
            next.pending.push(pending_utterance(
                session,
                &next.key,
                newly_selected.into_iter().next().expect("length checked"),
                DeliveryCheckpoint::Short,
            ));
        } else {
            debug_assert!(
                false,
                "narration selections must map one-to-one to delivery checkpoints"
            );
            // Fail closed on unrepresentable checkpoints.
            return admit_pending(&state_path, prev, &mut admit);
        }
        if !short_after
            || short_before
            || next
                .pending
                .iter()
                .any(|p| p.key == next.key && matches!(p.after, DeliveryCheckpoint::Short))
        {
            // Empty-after-cleanup short: no admission.
        } else {
            next.short_done = true;
        }

        // Persist before offer so rejection/exit keeps retryable pending.
        write_state(&state_path, &next);
        admit_pending(&state_path, next, &mut admit)
    })
}

/// Retry already-persisted pending for `session` (Codex housekeeping when no new events).
pub fn retry_pending(
    paths: &Paths,
    session: &str,
    mut admit: impl FnMut(&NarrationUtterance) -> Result<(), String>,
) -> Result<(), String> {
    let state_path = display_state_path(paths, session);
    if !state_path.exists() {
        return Ok(());
    }
    with_state_lock(&state_path, || {
        let state = read_state(&state_path);
        if state.pending.is_empty() {
            return Ok(());
        }
        admit_pending(&state_path, state, &mut admit)
    })
}

fn admit_pending(
    state_path: &Path,
    mut state: DisplayState,
    admit: &mut impl FnMut(&NarrationUtterance) -> Result<(), String>,
) -> Result<(), String> {
    while let Some(pending) = state.pending.first().cloned() {
        let utterance = NarrationUtterance {
            id: pending.id.clone(),
            text: pending.text.clone(),
            detection_text: pending.detection_text.clone(),
            message_key: pending.key.clone(),
        };
        admit(&utterance)?;
        state.pending.remove(0);
        if state.key == pending.key {
            match pending.after {
                DeliveryCheckpoint::Offset(offset) => state.offset = state.offset.max(offset),
                DeliveryCheckpoint::Short => state.short_done = true,
            }
        }
        // Per-utterance commit; stable id covers the crash window after engine accept.
        write_state(state_path, &state);
    }
    Ok(())
}

fn pending_utterance(
    session: &str,
    key: &str,
    selected: SelectedUtterance,
    after: DeliveryCheckpoint,
) -> PendingUtterance {
    // Id = session|key|text|after — detection_text must not enter the hash (retry stability).
    let mut hash = Sha256::new();
    for part in [session.as_bytes(), key.as_bytes(), selected.text.as_bytes()] {
        hash.update(part.len().to_le_bytes());
        hash.update(part);
    }
    match after {
        DeliveryCheckpoint::Offset(offset) => {
            hash.update([0]);
            hash.update(offset.to_le_bytes());
        }
        DeliveryCheckpoint::Short => hash.update([1]),
    }
    let digest = hash.finalize();
    let id = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PendingUtterance {
        id,
        key: key.to_string(),
        text: selected.text,
        detection_text: selected.detection_text,
        after,
    }
}

fn ensure_state_parent(state_path: &Path) {
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}

fn read_state(state_path: &Path) -> DisplayState {
    match std::fs::read_to_string(state_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(state) => state,
            Err(e) => {
                log::warn!(
                    target: "narrate",
                    "ignoring corrupt narration state {}: {e}",
                    state_path.display()
                );
                DisplayState::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DisplayState::default(),
        Err(e) => {
            log::warn!(
                target: "narrate",
                "could not read narration state {}: {e}",
                state_path.display()
            );
            DisplayState::default()
        }
    }
}

fn write_state(state_path: &Path, state: &DisplayState) {
    atomic_write(
        state_path,
        &serde_json::to_string(state).unwrap_or_default(),
    );
}

// ── Streaming witness + session lifecycle ────────────────────────────────────────

/// Pre-create the session state file so `Stop`'s `streamed` guard is true before the
/// first batch (closes the short-turn race). Idempotent/`create_new` — never clobbers
/// in-progress state. Callers: Claude Code SessionStart; Codex on `thread/resume`.
pub fn seed_witness(paths: &Paths, session: &str) {
    let path = display_state_path(paths, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // create_new: two concurrent seeders can't both overwrite a real batch in between.
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        let _ = file.write_all(
            serde_json::to_string(&DisplayState::default())
                .unwrap_or_default()
                .as_bytes(),
        );
    }
}

/// State file exists ⇒ streaming already narrated (or seeded) ⇒ `Stop` must stay silent.
pub fn witness_exists(paths: &Paths, session: &str) -> bool {
    display_state_path(paths, session).exists()
}

/// Drop state + lock/tmp siblings (SessionEnd / Codex eviction — otherwise they accumulate).
pub fn clear_session_state(paths: &Paths, session: &str) {
    let path = display_state_path(paths, session);
    let _ = std::fs::remove_file(path.with_extension("lock"));
    let _ = std::fs::remove_file(path.with_extension("tmp"));
    let _ = std::fs::remove_file(&path);
}

/// Pure `Stop` decision — empty when narration is off, mic is live, `streamed` (double-
/// narration guard), or no final text. Else one final [`Accum`] feed matching the stream path.
pub fn stop_utterances(
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
        return Vec::new(); // already narrated mid-turn ⇒ never double-speak
    }
    let Some(text) = last_assistant_message
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    Accum::default()
        .feed(0, text, None, true, messages_on, short_on)
        .into_iter()
        .map(|u| u.text)
        .collect()
}

// ── File plumbing ────────────────────────────────────────────────────────────────

/// `narrate-display-<session>.json` (filename-safe). Public for the engine orphan sweep.
pub fn display_state_path(paths: &Paths, session: &str) -> PathBuf {
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

/// Cross-process RMW mutex (Claude Code: one process per batch). Without it overlapping
/// writers drop the spoken line. `create_new` lockfile; ~800 ms spin then proceed;
/// 2 s stale break. Codex is single-threaded so this rarely contends.
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

fn atomic_write(path: &Path, contents: &str) {
    if let Err(e) = ds_config::atomic_write_str(path, contents) {
        log::warn!(
            target: "narrate",
            "could not write narration state {}: {e}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(key: &str, index: u64, text: &str, is_final: bool) -> StreamBatch {
        StreamBatch {
            key: key.into(),
            payload: BatchPayload::Delta {
                index: Some(index),
                text: text.into(),
            },
            is_final,
        }
    }

    fn cumulative(key: &str, text: &str, is_final: bool) -> StreamBatch {
        StreamBatch {
            key: key.into(),
            payload: BatchPayload::Cumulative { text: text.into() },
            is_final,
        }
    }

    fn drive(batches: &[StreamBatch], mic_active: bool) -> (DisplayState, Vec<String>) {
        let mut state = DisplayState::default();
        let mut spoken = Vec::new();
        for b in batches {
            let decided = step(&state, b, mic_active, true, false);
            if let Some(next) = decided.write {
                state = next;
            }
            spoken.extend(decided.speak.into_iter().map(|u| u.text));
        }
        (state, spoken)
    }

    #[test]
    fn state_lock_serializes_concurrent_read_modify_write() {
        // Race that silently dropped narration: concurrent RMW without the lock.
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(dir.path().join("narrate-display-x.json"));
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
                        // Widen the critical section so unlocked RMW would lose updates.
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
        assert_eq!(
            final_v, N as u64,
            "lock must serialize all increments — no lost updates"
        );
    }

    // One core, three adapters: same reply via delta / cumulative / hybrid → same speak.

    const REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.\n\n> Closing ask?";
    const EXPECT: &[&str] = &["First point.", "Second point.", "Closing ask?"];

    #[test]
    fn parity_claude_style_deltas() {
        let batches = [
            delta("m", 0, "> First point.\n\nDetail.", false),
            delta("m", 1, "\n\n> Second point.\n\nMore.", false),
            delta("m", 2, "\n\n> Closing ask?", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, EXPECT);
    }

    #[test]
    fn parity_qwen_style_cumulative_snapshots() {
        let cuts = [
            "> First point.\n\nDetail.",
            "> First point.\n\nDetail.\n\n> Second point.\n\nMore.",
            REPLY,
        ];
        let batches: Vec<StreamBatch> = cuts
            .iter()
            .enumerate()
            .map(|(i, t)| cumulative("m", t, i == cuts.len() - 1))
            .collect();
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, EXPECT);
    }

    #[test]
    fn parity_codex_style_deltas_then_final_cumulative() {
        // Deltas then final cumulative (covers missed pre-attach deltas).
        let batches = [
            delta("item_1", 0, "> First point.\n\nDetail.", false),
            delta("item_1", 1, "\n\n> Second point.\n\nMore.", false),
            cumulative("item_1", REPLY, true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, EXPECT);
        // Late attach with only the final cumulative still voices everything.
        let (_, late) = drive(&[cumulative("item_1", REPLY, true)], false);
        assert_eq!(late, EXPECT);
    }

    #[test]
    fn parity_duplicate_final_batch_is_a_noop() {
        // Replayed final (reconnect / duplicate hook) emits nothing more.
        let mut state = DisplayState::default();
        let fin = cumulative("m", REPLY, true);
        let s1 = step(&state, &fin, false, true, false);
        state = s1.write.unwrap();
        assert_eq!(
            s1.speak.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            EXPECT
        );
        let s2 = step(&state, &fin, false, true, false);
        assert!(
            s2.speak.is_empty(),
            "duplicate final batch re-speaks nothing"
        );
    }

    #[test]
    fn selection_carries_cumulative_detection_text() {
        let batches = [
            delta("m", 0, "Preamble for language.\n\n> First quote.", false),
            delta("m", 1, "\n\nBody.\n\n> Second quote.\n\nTail.", true),
        ];
        let mut state = DisplayState::default();
        let mut selected = Vec::new();
        for b in &batches {
            let decided = step(&state, b, false, true, false);
            if let Some(next) = decided.write {
                state = next;
            }
            selected.extend(decided.speak);
        }
        assert_eq!(
            selected.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            ["First quote.", "Second quote."]
        );
        assert!(
            selected[0]
                .detection_text
                .contains("Preamble for language.")
        );
        assert!(selected[0].detection_text.contains("First quote."));
        assert!(
            selected[1].detection_text.len() >= selected[0].detection_text.len(),
            "later quote sees fuller so-far"
        );
        assert!(selected[1].detection_text.contains("Second quote."));
    }

    #[test]
    fn new_key_resets_accumulation_and_speaks_again() {
        let batches = [
            delta("m1", 0, "> First.\n\nBody.", true),
            delta("m2", 0, "> Second.\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["First.".to_string(), "Second.".to_string()]);
    }

    #[test]
    fn mic_active_at_message_start_gates_whole_message() {
        // Gate once per key — stays gated even after the blockquote completes.
        let batches = [
            delta("m1", 0, "> Spoken.", false),
            delta("m1", 1, "\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, true);
        assert!(spoken.is_empty(), "mic live at start ⇒ message gated");
    }

    #[test]
    fn final_arriving_first_does_not_recompute_the_message_gate() {
        // Final-before-quote race: `seen_final` must not re-open the mic gate mid-message.
        let final_first = delta("m1", 1, "\n\nBody.", true);
        let quote_late = delta("m1", 0, "> Spoken.", false);

        let first = step(&DisplayState::default(), &final_first, false, true, false);
        let state = first.write.expect("final batch persists state");
        let late = step(&state, &quote_late, true, true, false);
        assert_eq!(
            late.speak
                .iter()
                .map(|u| u.text.as_str())
                .collect::<Vec<_>>(),
            ["Spoken."]
        );
        assert!(late.write.expect("late batch persists state").gate_on);
    }

    #[test]
    fn delta_without_index_appends_after_highest_seen() {
        let no_index = |key: &str, text: &str, fin: bool| StreamBatch {
            key: key.into(),
            payload: BatchPayload::Delta {
                index: None,
                text: text.into(),
            },
            is_final: fin,
        };
        let batches = [
            no_index("m", "> Spoken ", false),
            no_index("m", "line.", false),
            no_index("m", "\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, vec!["Spoken line.".to_string()]);
    }

    #[test]
    fn deliver_batch_persists_across_processes_and_never_double_speaks() {
        // Two independent calls share the file high-water mark — second is silent.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "sess-a";
        let fin = cumulative("item_1", REPLY, true);
        let mut spoken = Vec::new();
        deliver_batch(&paths, session, &fin, false, true, false, |utt| {
            spoken.push(utt.text.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(spoken, EXPECT);
        deliver_batch(&paths, session, &fin, false, true, false, |_| {
            panic!("replayed batch after the state landed on disk must be silent")
        })
        .unwrap();
        assert!(witness_exists(&paths, session));
    }

    #[test]
    fn deliver_batch_scopes_state_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let fin = cumulative("m", "> Hi.\n\nBody.", true);
        let mut first = Vec::new();
        deliver_batch(&paths, "s1", &fin, false, true, false, |utt| {
            first.push(utt.text.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(first, vec!["Hi.".to_string()]);
        // Different session ⇒ own file ⇒ speaks again.
        let mut second = Vec::new();
        deliver_batch(&paths, "s2", &fin, false, true, false, |utt| {
            second.push(utt.text.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(second, vec!["Hi.".to_string()]);
    }

    #[test]
    fn rejected_delivery_keeps_offset_pending_and_retries_same_id_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let fin = cumulative("m", "> Retry me.\n\nBody.", true);
        let mut rejected_id = None;
        let mut rejected_detection = None;
        let mut rejected_key = None;
        let error = deliver_batch(&paths, "s", &fin, false, true, false, |utt| {
            rejected_id = Some(utt.id.clone());
            rejected_detection = Some(utt.detection_text.clone());
            rejected_key = Some(utt.message_key.clone());
            Err("queue full".to_string())
        })
        .unwrap_err();
        assert_eq!(error, "queue full");
        let state = read_state(&display_state_path(&paths, "s"));
        assert_eq!(state.offset, 0, "rejection must not commit delivery");
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].detection_text, "> Retry me.\n\nBody.");
        assert_eq!(state.pending[0].key, "m");

        let mut admitted = Vec::new();
        retry_pending(&paths, "s", |utt| {
            admitted.push((
                utt.id.clone(),
                utt.text.clone(),
                utt.detection_text.clone(),
                utt.message_key.clone(),
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            admitted,
            vec![(
                rejected_id.unwrap(),
                "Retry me.".to_string(),
                rejected_detection.unwrap(),
                rejected_key.unwrap(),
            )]
        );
        assert_eq!(admitted[0].2, "> Retry me.\n\nBody.");
        assert_eq!(admitted[0].3, "m");
        retry_pending(&paths, "s", |_| {
            panic!("committed retry must not be offered twice")
        })
        .unwrap();
        assert_eq!(read_state(&display_state_path(&paths, "s")).offset, 1);
    }

    #[test]
    fn pending_without_detection_text_deserializes_and_id_ignores_it() {
        // Old state files omit detection_text; serde default keeps retry ids stable.
        let json = r#"{
            "offset":0,
            "key":"m",
            "pending":[{
                "id":"deadbeefdeadbeefdeadbeefdeadbeef",
                "key":"m",
                "text":"Retry me.",
                "after":{"kind":"offset","value":1}
            }]
        }"#;
        let state: DisplayState = serde_json::from_str(json).unwrap();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].detection_text, "");
        assert_eq!(state.pending[0].text, "Retry me.");

        let with_det = pending_utterance(
            "s",
            "m",
            SelectedUtterance {
                text: "Retry me.".into(),
                detection_text: "full corpus that must not change id".into(),
            },
            DeliveryCheckpoint::Offset(1),
        );
        let without_det = pending_utterance(
            "s",
            "m",
            SelectedUtterance {
                text: "Retry me.".into(),
                detection_text: "different corpus".into(),
            },
            DeliveryCheckpoint::Offset(1),
        );
        assert_eq!(
            with_det.id, without_det.id,
            "pending id hash excludes detection_text"
        );
        assert_ne!(with_det.detection_text, without_det.detection_text);
    }

    #[test]
    fn shorts_only_consumes_blockquotes_without_creating_delivery_work() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let fin = cumulative("m", "> Digest disabled.\n\nBody.", true);
        deliver_batch(&paths, "s", &fin, false, false, true, |_| {
            panic!("shorts mode must not offer a reply that contains a blockquote")
        })
        .unwrap();
        let state = read_state(&display_state_path(&paths, "s"));
        assert_eq!(state.offset, 1);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn partial_multi_utterance_admission_commits_only_the_accepted_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let fin = cumulative("m", "> First.\n\nBody.\n\n> Second.", true);
        let mut attempts = 0;
        deliver_batch(&paths, "s", &fin, false, true, false, |_| {
            attempts += 1;
            if attempts == 1 {
                Ok(())
            } else {
                Err("queue full".to_string())
            }
        })
        .unwrap_err();
        let state = read_state(&display_state_path(&paths, "s"));
        assert_eq!(state.offset, 1);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].text, "Second.");

        let mut retried = Vec::new();
        retry_pending(&paths, "s", |utterance| {
            retried.push(utterance.text.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(retried, vec!["Second.".to_string()]);
        assert_eq!(read_state(&display_state_path(&paths, "s")).offset, 2);
    }

    #[test]
    fn witness_is_seeded_scoped_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        assert!(!witness_exists(&paths, "s1"));
        seed_witness(&paths, "s1");
        assert!(witness_exists(&paths, "s1"), "seed creates the witness");
        assert!(
            !witness_exists(&paths, "s2"),
            "a different session is never marked streamed"
        );
        assert!(stop_utterances(Some("> X.\n\nY."), true, true, false, true).is_empty());

        let path = display_state_path(&paths, "s1");
        let _ = std::fs::write(path.with_extension("lock"), "");
        let _ = std::fs::write(path.with_extension("tmp"), "");
        clear_session_state(&paths, "s1");
        assert!(!witness_exists(&paths, "s1"));
        assert!(!path.with_extension("lock").exists());
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn seed_witness_is_non_destructive() {
        // Re-fired seed must not clobber in-progress state.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let path = display_state_path(&paths, "s1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let sentinel = r#"{"offset":2,"key":"real-message-state"}"#;
        std::fs::write(&path, sentinel).unwrap();
        seed_witness(&paths, "s1");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sentinel,
            "seed must not clobber real in-progress message state"
        );
    }

    #[test]
    fn display_state_path_sanitizes_the_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let p = display_state_path(&paths, "a/b\\c:d e");
        let name = p.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, "narrate-display-a_b_c_d_e.json");
    }

    /// Shape streaming narrates — Stop would re-speak this if the guard regressed.
    const DIGEST_REPLY: &str = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.";

    #[test]
    fn stop_is_silent_when_already_streamed() {
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
}
