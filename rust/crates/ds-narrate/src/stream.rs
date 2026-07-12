//! The client-neutral streaming step + the file-backed per-session state (the streaming
//! WITNESS). Moved out of `dontspeak::hook_narrate` so the CLI hook adapters (Claude
//! Code / Qwen Code) and the engine's Codex app-server subscriber all drive the ONE
//! pipeline: [`StreamBatch`] in → [`step`] (pure) → [`narrate_batch`] (lock → read state
//! → step → atomic write) → utterances out. Because every adapter persists through the
//! same per-session file, the witness ([`witness_exists`]) that keeps `Stop` silent
//! comes for free for all three, and the on-disk `offset` high-water mark makes a
//! reconnect/restart dedup-safe (already-spoken runs never re-emit).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ds_config::Paths;
use serde::{Deserialize, Serialize};

use crate::accum::Accum;

/// One streamed text batch, client-neutral — the event every adapter's payload shape maps
/// onto (Claude Code's `MessageDisplay` delta, Qwen Code's cumulative snapshot, Codex's
/// `item/agentMessage/delta` / `item/completed`).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamBatch {
    /// Stable per-message key: the client's message/item id, or (older Claude Code with no
    /// id) the adapter's text fingerprint fallback (see [`crate::display_state_path`]'s
    /// sibling `message_key` in the adapters). A NEW key resets accumulation — one key is
    /// one assistant message.
    pub key: String,
    pub payload: BatchPayload,
    /// True on the message's last batch → the final blockquote run counts as complete even
    /// with no terminating blank line after it (and the "shorts" fallback may fire).
    pub is_final: bool,
}

/// How this batch carries its text.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchPayload {
    /// An incremental chunk. `index` is the content-block index within the message —
    /// NOT a key, but essential for ORDER when batches race (Claude Code spawns a process
    /// per batch); `None` (older clients / inherently ordered transports) appends after
    /// the highest index seen, preserving arrival order.
    Delta { index: Option<u64>, text: String },
    /// The whole message text so far (cumulative snapshot) — wins over any accumulated
    /// deltas, covering missed chunks (e.g. deltas sent before a Codex subscriber attached).
    Cumulative { text: String },
}

/// Per-session state for the streaming diff: how many blockquote utterances of the
/// current message we've already spoken (`offset` = spoken count), plus the message key
/// to detect when a NEW message starts (accumulation resets). Serialized as
/// `narrate-display-<session>.json` — the field names are the ON-DISK contract, shared
/// by every adapter process; don't rename them.
#[derive(Debug, Deserialize, Serialize, Default, Clone, PartialEq)]
pub struct DisplayState {
    /// Count of this message's top-level blockquotes already voiced. Each batch speaks any
    /// newly-completed run beyond this count and advances it; a new message resets it to 0.
    pub offset: usize,
    pub key: String,
    /// Delta mode: each batch's chunk keyed by its content-block `index`, so the cumulative
    /// text reconstructs in INDEX order regardless of the order racing batch-processes
    /// reach us. Empty in cumulative mode.
    #[serde(default)]
    pub parts: BTreeMap<u64, String>,
    /// Sticky "a batch with final=true has been seen" — the terminating flag must survive
    /// even when that batch is processed BEFORE the one carrying the blockquote (out of order).
    #[serde(default)]
    pub seen_final: bool,
    /// Sticky latch for the "shorts" fallback (a blockquote-less final reply voiced whole,
    /// once) — maps to `Accum::short_done`, so a late duplicate batch never re-speaks it.
    #[serde(default)]
    pub short_done: bool,
    /// The mic gate is decided ONCE per assistant message (keyed by the message key) and
    /// cached here, so a mid-stream mic flap can't strand the tail of a message we
    /// already started narrating — nor start one we decided to skip. `gate_msg` is the
    /// key the decision belongs to; `gate_on` is whether it narrates.
    #[serde(default)]
    pub gate_msg: String,
    #[serde(default)]
    pub gate_on: bool,
    /// Distinguishes an actual cached decision for an empty message key from the
    /// all-empty serde/default state.
    #[serde(default)]
    pub gate_set: bool,
}

/// One batch's effect, decided PURELY (no IO) so it is unit-testable — the seam the
/// streaming-accumulation regression tests drive. `write = None` means "leave the state
/// file untouched" (a no-op batch); `speak` holds the blockquote utterances that became
/// COMPLETE this batch (one per top-level `>` run), in order, each voiced once. Usually
/// empty or one item; a batch that completes several runs at once (out-of-order delivery,
/// or a body line that terminates the last run) yields several.
pub struct DisplayStep {
    pub write: Option<DisplayState>,
    pub speak: Vec<String>,
}

/// Decide what a single streamed batch does, given the previous per-session state and
/// whether the mic is live. Pure: same inputs → same outputs, no disk/socket/platform.
/// This is `dontspeak`'s old `step_display` with the hook struct swapped for the
/// client-neutral [`StreamBatch`]; the per-message mic gate is keyed by `batch.key`
/// (the same message id the old code gated on — the adapters put the message/item id, or
/// the fingerprint fallback, in `key`).
pub fn step(
    prev: &DisplayState,
    batch: &StreamBatch,
    mic_active: bool,
    digests_on: bool,
    shorts_on: bool,
) -> DisplayStep {
    // Per-MESSAGE mic gate. A message streams as many batches; checking the gate on EACH
    // batch lets a momentary mic blip strand the rest of a message we already began
    // narrating — observed as "only the first sentence spoke." So decide ONCE, when a
    // message first appears (by key), and cache it: every later batch of the same message
    // inherits that decision.
    //
    // FOCUS is NOT gated here: narration is forwarded TAGGED BY SESSION, and the engine
    // speaks only the ACTIVE terminal's items, holding the rest until they become active
    // (see docs/PER-TERMINAL-QUEUES.md). We still suppress narration sent WHILE the user is
    // recording — no reason to stream fresh chatter into a dictation.
    let gate_on = if prev.gate_set && prev.gate_msg == batch.key && !prev.seen_final {
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
                gate_msg: batch.key.clone(),
                gate_on: false,
                gate_set: true,
            }),
            speak: Vec::new(),
        };
    }

    // New-message key: the stable message/item id ALONE (never a per-batch index — keying
    // on the index once reset the accumulator each batch and silently dropped the leading
    // blockquote). The adapters fall back to a text fingerprint when the client sends no id.
    let same = prev.key == batch.key;

    // Drive the accumulator core ([`Accum`]) — the reconstruction + every-blockquote emit
    // logic, kept pure so it is exhaustively unit-testable and a fix lands in one place.
    // The per-session state FILE is this path's cross-process persistence, so we hydrate an
    // `Accum` from the prior state (for the same message) or fresh, step it, and write it
    // back. `offset` ⇆ `Accum::emitted` (runs already voiced); `parts`/`seen_final` map 1:1.
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
            // No `index` (older clients / ordered transports) → append after the highest
            // seen, preserving arrival order.
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
    };
    DisplayStep {
        write: Some(next),
        speak,
    }
}

/// The composed FILE-BACKED step — the whole cross-adapter contract in one function:
/// serialize on the per-session lock, hydrate the prior [`DisplayState`] from disk, run
/// the pure [`step`], persist its decision atomically, and return the utterances that
/// became ready THIS batch (the caller forwards them to the engine / TTS queue). Every
/// adapter persists through this same per-session file, so the streaming witness
/// ([`witness_exists`]) is written as a side effect and a reconnect/restart can never
/// double-speak (the `offset` high-water mark is on disk).
pub fn narrate_batch(
    paths: &Paths,
    session: &str,
    batch: &StreamBatch,
    mic_active: bool,
    digests_on: bool,
    shorts_on: bool,
) -> Vec<String> {
    let state_path = display_state_path(paths, session);
    // Make sure the state dir exists BEFORE taking the lock: `with_state_lock`'s
    // `create_new` can never succeed under a missing parent, which would spin out the
    // whole 800 ms ceiling on every batch. (The engine creates the dir at boot; the
    // per-batch hook processes shouldn't rely on that ordering.)
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    with_state_lock(&state_path, || {
        let prev: DisplayState = match std::fs::read_to_string(&state_path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(state) => state,
                Err(e) => {
                    eprintln!(
                        "dontspeak: ignoring corrupt narration state {}: {e}",
                        state_path.display()
                    );
                    DisplayState::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DisplayState::default(),
            Err(e) => {
                eprintln!(
                    "dontspeak: could not read narration state {}: {e}",
                    state_path.display()
                );
                DisplayState::default()
            }
        };
        let decided = step(&prev, batch, mic_active, digests_on, shorts_on);
        if let Some(next) = decided.write {
            atomic_write(
                &state_path,
                &serde_json::to_string(&next).unwrap_or_default(),
            );
        }
        decided.speak
    })
}

// ── The streaming witness + session lifecycle ────────────────────────────────────

/// Pre-create THIS session's streaming state file so the `Stop` hook's `streamed` guard
/// is reliably true before the first batch lands (closing the short-turn race where a
/// `Stop` could fire before the first flush). Idempotent + non-destructive: never
/// clobbers real in-progress state (a re-fired seed is a no-op), and the seeded default
/// reads exactly like "no file yet" (fresh [`Accum`]), so streaming is unaffected.
/// Callers: Claude Code's plain-`notify` SessionStart, and the Codex supervisor on a
/// successful `thread/resume`.
pub fn seed_witness(paths: &Paths, session: &str) {
    let path = display_state_path(paths, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // create_new is the atomic existence check: two concurrent seeders cannot both
    // observe a missing file and then overwrite a real batch written between those steps.
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

/// Witness that a streaming pass ran (or was seeded) for this session: its per-session
/// state file exists. The deterministic client-discriminator the `Stop` path needs —
/// present ⇒ the reply was (or will be) narrated mid-turn, so `Stop` must not re-speak
/// it; absent ⇒ `Stop` is this session's only narration path (plain-TUI Codex, Qwen Code).
pub fn witness_exists(paths: &Paths, session: &str) -> bool {
    display_state_path(paths, session).exists()
}

/// Reclaim a finished session's on-disk narration state: the state file and its lock/tmp
/// siblings. Without this they accumulate one `narrate-display-<session>.json` per
/// distinct session in the data dir forever. Called on `SessionEnd` (Claude/Qwen hooks)
/// and by the Codex supervisor's eviction (Codex wires no SessionEnd hook).
pub fn clear_session_state(paths: &Paths, session: &str) {
    let path = display_state_path(paths, session);
    let _ = std::fs::remove_file(path.with_extension("lock"));
    let _ = std::fs::remove_file(path.with_extension("tmp"));
    let _ = std::fs::remove_file(&path);
}

/// Decide what the `Stop` hook should voice, PURELY (no IO) so it is exhaustively
/// unit-testable — the seam the double-narration regression tests drive. Returns the
/// blockquote / short utterances to speak, in order, or EMPTY when `Stop` must stay silent:
///   • narration off (`!messages_on && !short_on`),
///   • mid-dictation (`mic_active` — don't talk over the user, mirrors the streaming gate),
///   • `streamed` — a streaming pass already narrated this turn; re-voicing here is the
///     double-narration bug, so we suppress it,
///   • no usable final text.
/// Otherwise the whole reply is fed through a fresh [`Accum`] as ONE final batch, yielding
/// the exact runs the streaming path would emit (every top-level blockquote in order; or,
/// under `short`, a brief blockquote-less reply whole) — so a non-streamed reply is voiced
/// just like a streamed one.
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
        return Vec::new(); // a streaming pass already narrated this session ⇒ never double-speak
    }
    let Some(text) = last_assistant_message
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    Accum::default().feed(0, text, None, true, messages_on, short_on)
}

// ── File plumbing (the per-session state file, its lock, and the atomic write) ────

/// The per-session streaming state file: `narrate-display-<session>.json`, a sibling of
/// `narrate.pid` in the data dir. Session ids are uuid-like; keep only filename-safe
/// chars defensively. Public so the engine's orphan sweep can enumerate/match the files
/// it owns.
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

/// Serialize the per-session state read-modify-write across independent processes (Claude
/// Code spawns one per streamed batch). Without it, overlapping batches race on the state
/// file and the accumulated blockquote is lost → the spoken line is silently dropped. A
/// lock file beside the state file is the mutex: `create_new` is atomic, so exactly one
/// process holds it and the rest spin briefly. Bounded so narration can never wedge (it
/// proceeds without the lock after the ceiling), and a stale lock from a crashed holder is
/// broken by age — batches are sub-second, so a 2 s floor never trips during normal
/// streaming. (The Codex subscriber is a single ordered in-process feeder, so for its
/// sessions the lock simply never contends.)
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
    if let Err(e) = ds_config::atomic_write_str(path, contents) {
        eprintln!(
            "dontspeak: could not write narration state {}: {e}",
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

    /// Feed a sequence of batches through the pure step, threading state as the
    /// file-backed path would. Returns the final state and every spoken line, in order.
    fn drive(batches: &[StreamBatch], mic_active: bool) -> (DisplayState, Vec<String>) {
        let mut state = DisplayState::default();
        let mut spoken = Vec::new();
        for b in batches {
            let decided = step(&state, b, mic_active, true, false);
            if let Some(next) = decided.write {
                state = next;
            }
            spoken.extend(decided.speak);
        }
        (state, spoken)
    }

    #[test]
    fn state_lock_serializes_concurrent_read_modify_write() {
        // Reproduces the batch-process race that silently dropped narration: many writers
        // doing read-modify-write on one state file. Under `with_state_lock` every increment
        // must land (final == N); without the lock the widened window loses updates.
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
        assert_eq!(
            final_v, N as u64,
            "lock must serialize all increments — no lost updates"
        );
    }

    // ── The "one core, three adapters" parity pin ─────────────────────────────────
    //
    // The SAME reply fed three ways — (a) Claude-Code-style per-batch deltas, (b)
    // Qwen-style cumulative snapshots, (c) Codex-style deltas + final-cumulative — must
    // speak IDENTICAL utterance sequences. This is the contract that lets three clients
    // share one core; if a payload shape ever needs core changes, this fails first.

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
        // Codex: ordered deltas per item, then `item/completed` carries the WHOLE final
        // text as one cumulative batch (flushing the last run exactly like CC's `final`
        // flag — and covering deltas missed before attach).
        let batches = [
            delta("item_1", 0, "> First point.\n\nDetail.", false),
            delta("item_1", 1, "\n\n> Second point.\n\nMore.", false),
            cumulative("item_1", REPLY, true),
        ];
        let (_, spoken) = drive(&batches, false);
        assert_eq!(spoken, EXPECT);
        // A late-attach subscriber that saw NO deltas still voices everything from the
        // final cumulative batch alone.
        let (_, late) = drive(&[cumulative("item_1", REPLY, true)], false);
        assert_eq!(late, EXPECT);
    }

    #[test]
    fn parity_duplicate_final_batch_is_a_noop() {
        // A replayed final batch (reconnect / duplicate hook process) emits nothing more,
        // in every payload shape.
        let mut state = DisplayState::default();
        let fin = cumulative("m", REPLY, true);
        let s1 = step(&state, &fin, false, true, false);
        state = s1.write.unwrap();
        assert_eq!(s1.speak, EXPECT);
        let s2 = step(&state, &fin, false, true, false);
        assert!(
            s2.speak.is_empty(),
            "duplicate final batch re-speaks nothing"
        );
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
        // If the mic was live when the message first appeared, the whole message stays
        // gated even after the blockquote completes (decided once, cached per key).
        let batches = [
            delta("m1", 0, "> Spoken.", false),
            delta("m1", 1, "\n\nBody.", true),
        ];
        let (_, spoken) = drive(&batches, true);
        assert!(spoken.is_empty(), "mic live at start ⇒ message gated");
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

    // ── narrate_batch: the file-backed composition ────────────────────────────────

    #[test]
    fn narrate_batch_persists_across_processes_and_never_double_speaks() {
        // Two independent `narrate_batch` calls (as two hook processes / a reconnected
        // subscriber would make) share state through the file: the second call sees the
        // first's high-water mark and re-emits nothing.
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let session = "sess-a";
        let fin = cumulative("item_1", REPLY, true);
        assert_eq!(
            narrate_batch(&paths, session, &fin, false, true, false),
            EXPECT
        );
        assert!(
            narrate_batch(&paths, session, &fin, false, true, false).is_empty(),
            "replayed batch after the state landed on disk must be silent"
        );
        // And the witness came for free.
        assert!(witness_exists(&paths, session));
    }

    #[test]
    fn narrate_batch_scopes_state_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        let fin = cumulative("m", "> Hi.\n\nBody.", true);
        assert_eq!(
            narrate_batch(&paths, "s1", &fin, false, true, false),
            vec!["Hi.".to_string()]
        );
        // A different session has its own file ⇒ speaks again.
        assert_eq!(
            narrate_batch(&paths, "s2", &fin, false, true, false),
            vec!["Hi.".to_string()]
        );
    }

    // ── Witness + lifecycle ───────────────────────────────────────────────────────

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
        // Seeded ⇒ Stop stays silent for that session.
        assert!(stop_utterances(Some("> X.\n\nY."), true, true, false, true).is_empty());

        // clear_session_state removes the whole trio.
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
        // Real in-progress state must survive a re-fired seed verbatim.
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

    // ── Stop decision (moved verbatim from dontspeak::hook_narrate) ──────────────────

    /// A reply whose digest is two blockquotes plus body — the exact shape the streaming
    /// path narrates and the shape Stop would re-speak if the guard regressed.
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
