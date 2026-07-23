//! The persistent stdout reader for [`super::TtsManager`] — owns the warm child's
//! stdout and demuxes each line into the speak/listen/diarize/enroll slots, so a
//! `speak` and a `listen` can be served concurrently. Split from the manager into its
//! own module (matching `codex_stream/`) so the slot types stay private to `tts/`.

use std::io::BufRead;
use std::sync::{Arc, Condvar, Mutex};

use ds_helper_proto as proto;

use super::{realized_stt_token, store_realized_stt};
use crate::child_slot::ChildSlot;
use crate::model_slot::{ModelSlot, ModelState};
use crate::status::StatusGate;

/// Render a `try_wait`'d exit status for a log line ("exit status: 0" / "signal: 9
/// (SIGKILL)" via `ExitStatus`'s own `Display`, or a fixed fallback when the status
/// couldn't be obtained). Shared ONLY for this one formatting detail — each caller
/// (`mark_dead_locked`'s reap, the reader thread's live unexpected-EOF detection)
/// keeps its own distinct surrounding message, so which one fired stays traceable
/// in the log — that distinction is itself diagnostic (crash reaped lazily on the
/// next speak vs. caught live the instant the pipe closed).
pub(super) fn describe_exit(status: Option<std::process::ExitStatus>) -> String {
    status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "exit status unavailable".to_string())
}

/// What a `speak` waits for: the persistent reader thread sets `done` on the
/// child's `DONE` (or `ERR`/EOF, with `err`). `fatal` distinguishes a child that
/// DIED (EOF/read error ⇒ reap + restart) from a soft `ERR` line (child alive).
#[derive(Default)]
pub(super) struct SpeakSlot {
    pub(super) done: bool,
    pub(super) err: Option<String>,
    pub(super) fatal: bool,
    /// The helper's ABSOLUTE played-batch high-water mark for THIS request
    /// (`PROGRESS` lines — see `ds_helper_proto::PROGRESS_PREFIX`). 0 = no mark seen
    /// (older helper, duplex path, or nothing played) ⇒ resume falls back to the top.
    /// Monotone within a request; reset with the whole slot by `play()`.
    pub(super) progress: usize,
}

/// What an ordered earcon waits for: the reader sets `done` on `CUEDONE`; `dead` wakes the
/// queue if the helper exits mid-cue.
#[derive(Default)]
pub(super) struct CueSlot {
    pub(super) done: bool,
    pub(super) dead: bool,
}

/// One demuxed line of a `listen` session (the reader routes the child's
/// LISTENING/PARTIAL/FINAL/STTERR/LDONE lines here).
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) enum ListenEvt {
    Partial(String),
    Final(String),
    Err(String),
    Done,
}

/// What a `listen` drains: the reader pushes [`ListenEvt`]s; `dead` marks the
/// child gone so a waiting listen unblocks.
#[derive(Default)]
pub(super) struct ListenSlot {
    pub(super) events: std::collections::VecDeque<ListenEvt>,
    pub(super) dead: bool,
}

/// What a one-shot `diarize` waits for: the reader fills `result` from the child's
/// `DIAR <json>` (Ok) or `DIARERR <msg>` (Err), then sets `done` on `DDONE`. `dead`
/// marks the child gone mid-diarize so the waiter unblocks. Simpler than a listen —
/// diarize is record-then-return, not streamed.
#[derive(Default)]
pub(super) struct DiarizeSlot {
    pub(super) result: Option<Result<String, String>>,
    pub(super) done: bool,
    pub(super) dead: bool,
}

/// What a one-shot `enroll` waits for: the reader fills `result` from the child's
/// `EMB <json-floats>` (Ok) or `ENROLLERR <msg>` (Err), then sets `done` on `EDONE`.
/// Same shape as [`DiarizeSlot`].
#[derive(Default)]
pub(super) struct EnrollSlot {
    pub(super) result: Option<Result<String, String>>,
    pub(super) done: bool,
    pub(super) dead: bool,
}

/// The five demux slots [`reader_loop`] routes the child's lines into. Bundled because every
/// caller always supplies the whole set together rather than passing a fixed-order run of arcs.
pub(super) struct ReaderSlots {
    pub(super) speak: Arc<(Mutex<SpeakSlot>, Condvar)>,
    pub(super) cue: Arc<(Mutex<CueSlot>, Condvar)>,
    pub(super) listen: Arc<(Mutex<ListenSlot>, Condvar)>,
    pub(super) diarize: Arc<(Mutex<DiarizeSlot>, Condvar)>,
    pub(super) enroll: Arc<(Mutex<EnrollSlot>, Condvar)>,
}

/// The three stats/lifetime sinks [`reader_loop`] feeds from the
/// child's `STATS`/`STTSTATS` lines. Bundled alongside [`ReaderSlots`] for the
/// same reason — always passed together, never partially.
pub(super) struct ReaderStats {
    pub(super) tts: Arc<crate::stats::TtsStats>,
    pub(super) stt: Arc<crate::stats::SttStats>,
    pub(super) lifetime: Arc<crate::stats::LifetimeSeconds>,
}

/// The model-residency state [`reader_loop`] flips on
/// `TTSLOADED`/`STTLOADED`/unexpected EOF, plus the shared [`ChildSlot`] it asks
/// whether an EOF was deliberate and peeks (never kills) for the real exit
/// status. Bundled so the reader doesn't thread a fixed-order run of positional
/// args that share a type — an easy mis-ordering footgun.
pub(super) struct ReaderModelState {
    pub(super) tts_model: Arc<ModelSlot>,
    pub(super) stt_model: Arc<ModelSlot>,
    pub(super) stt_realized: Arc<Mutex<Option<String>>>,
    pub(super) gate: Option<Arc<StatusGate>>,
    pub(super) child: Arc<ChildSlot>,
}

/// The persistent stdout reader: owns the warm child's stdout and demuxes each
/// line into the operation slots, so independent operations can be served concurrently.
/// Returns on EOF / read error (child gone), signalling every slot so all waiters unblock.
pub(super) fn reader_loop(
    // `impl BufRead` (not `BufReader<ChildStdout>`) so the EOF handling is unit-testable
    // with a canned byte slice — production passes the child's buffered stdout.
    mut stdout: impl BufRead,
    slots: ReaderSlots,
    stats: ReaderStats,
    model: ReaderModelState,
) {
    let ReaderSlots {
        speak: speak_slot,
        cue: cue_slot,
        listen: listen_slot,
        diarize: diarize_slot,
        enroll: enroll_slot,
    } = slots;
    let ReaderStats {
        tts: stats,
        stt: stt_stats,
        lifetime,
    } = stats;
    let ReaderModelState {
        tts_model,
        stt_model,
        stt_realized,
        gate,
        child,
    } = model;
    let push_listen = |evt: ListenEvt| {
        let (m, cv) = &*listen_slot;
        m.lock().unwrap().events.push_back(evt);
        cv.notify_all();
    };
    let mut line = String::new();
    const MAX_HELPER_LINE_BYTES: usize = 1024 * 1024;
    loop {
        line.clear();
        let read = {
            let mut limited = std::io::Read::take(&mut stdout, (MAX_HELPER_LINE_BYTES + 1) as u64);
            limited.read_line(&mut line)
        };
        if read.as_ref().is_ok_and(|&n| n > MAX_HELPER_LINE_BYTES) {
            // Consume the remainder so the next iteration starts at a protocol boundary.
            if !line.ends_with('\n') {
                loop {
                    let (used, done) = match stdout.fill_buf() {
                        Ok([]) | Err(_) => break,
                        Ok(buf) => match buf.iter().position(|&b| b == b'\n') {
                            Some(pos) => (pos + 1, true),
                            None => (buf.len(), false),
                        },
                    };
                    stdout.consume(used);
                    if done {
                        break;
                    }
                }
            }
            log::warn!(target: "engine", "helper emitted an oversized protocol line; discarded");
            continue;
        }
        match read {
            Ok(0) | Err(_) => {
                // Child gone: unblock a waiting speak (fatal) and a waiting listen.
                let (m, cv) = &*speak_slot;
                let mut s = m.lock().unwrap();
                s.done = true;
                s.fatal = true;
                if s.err.is_none() {
                    s.err = Some("TTS child closed".into());
                }
                cv.notify_all();
                drop(s);
                let (cm, ccv) = &*cue_slot;
                cm.lock().unwrap().dead = true;
                ccv.notify_all();
                let (lm, lcv) = &*listen_slot;
                lm.lock().unwrap().dead = true;
                lcv.notify_all();
                let (dm, dcv) = &*diarize_slot;
                dm.lock().unwrap().dead = true;
                dcv.notify_all();
                let (em, ecv) = &*enroll_slot;
                em.lock().unwrap().dead = true;
                ecv.notify_all();
                // An EOF nobody marked expected = the child DIED post-READY (AV
                // false-positive on freshly written dylibs, OOM, GPU driver). Such
                // deaths used to be invisible — no log line, and the stale "loaded"
                // flags stayed green until some later write failed. Unload both models
                // NOW (the status dots go amber immediately) and say so; the worker's
                // `restart_if_crashed` revives the child on the next speak. Deliberate
                // teardowns (`stop_child`/`mark_dead`) own their flags and logging.
                if !child.eof_was_expected() {
                    // `ModelSlot::transition` to `Idle` clears any per-model "failed to
                    // load" state too (mirrors every other teardown path —
                    // `start_locked`'s fresh-install, `clear_loaded_flags`,
                    // `unload_engine`): a crashed child's stale error must not keep
                    // showing after the process is gone, or it lingers until the next
                    // successful `start_locked`. Each call is independently change-gated,
                    // so this replaces the old unconditional gate bump below it too — a
                    // real transition on either model already wakes a blocked waiter.
                    //
                    // The dead child's realized backend is no longer a measurement — and the
                    // two transitions below BUMP, so a waiter woken by them must not still
                    // read "CUDA" for a process that no longer exists. Same ordering trap as
                    // the set direction.
                    store_realized_stt(&stt_realized, None, gate.as_deref());
                    tts_model.transition(ModelState::Idle, gate.as_deref());
                    stt_model.transition(ModelState::Idle, gate.as_deref());
                    // Debug aid: try_wait() the real exit status/signal at the MOMENT
                    // of detection — peek only (never kill/take), so the later
                    // mark_dead/restart_if_crashed still owns the actual reap. Without
                    // this the cause (SIGKILL/SIGSEGV/OOM/clean exit) was only ever
                    // learned lazily, whenever the next speak/listen happened to
                    // trigger restart_if_crashed — which may be minutes later, or
                    // never before the app itself restarts.
                    let status = child.peek_exit_status();
                    log::warn!(
                        target: "engine",
                        "TTS warm child exited unexpectedly ({}) — models \
                         unloaded; the next speak restarts it",
                        describe_exit(status)
                    );
                }
                return;
            }
            Ok(_) => {
                let l = line.trim();
                // ── speak terminals ──────────────────────────────────────────
                if l == proto::DONE {
                    let (m, cv) = &*speak_slot;
                    m.lock().unwrap().done = true;
                    cv.notify_all();
                } else if l == proto::CUEDONE {
                    let (m, cv) = &*cue_slot;
                    m.lock().unwrap().done = true;
                    cv.notify_all();
                } else if let Some(rest) = l.strip_prefix(proto::STATS_PREFIX) {
                    // Persist the per-utterance playback timing to the activity log (it
                    // otherwise only fed the in-app stats view, so a clipped/short reply left
                    // no trace — the gap that made the tail-clip bug hard to diagnose). DEBUG
                    // level: off by default, one concise line per speak when DONTSPEAK_DEBUG
                    // is on, size-rotated like the rest.
                    log::debug!(target: "engine", "TTS speak {rest}");
                    if let Some(secs) = stats.record_stats_line(rest) {
                        lifetime.add_tts(secs);
                        // End-of-utterance: refresh stats UI even if speaking flag doesn't edge.
                        if let Some(g) = gate.as_ref() {
                            g.bump();
                        }
                    }
                } else if let Some(rest) = l.strip_prefix(proto::PROGRESS_PREFIX) {
                    // Batch-granular resume mark: intermediate, never terminal (no
                    // done/condvar). Malformed values are ignored — protocol chatter
                    // must never fail a speak — and the max() keeps the mark monotone
                    // even if lines somehow arrive out of order.
                    if let Ok(v) = rest.trim().parse::<usize>() {
                        let mut s = speak_slot.0.lock().unwrap();
                        s.progress = s.progress.max(v);
                    }
                } else if let Some(msg) = l.strip_prefix(proto::ERR) {
                    let (m, cv) = &*speak_slot;
                    let mut s = m.lock().unwrap();
                    s.err = Some(format!("TTS child error:{msg}"));
                    s.done = true; // soft error: child stays alive
                    cv.notify_all();
                // ── listen events ────────────────────────────────────────────
                } else if l == proto::LDONE {
                    push_listen(ListenEvt::Done);
                } else if let Some(rest) = l.strip_prefix(proto::PARTIAL_PREFIX) {
                    push_listen(ListenEvt::Partial(rest.to_string()));
                } else if l == proto::FINAL {
                    push_listen(ListenEvt::Final(String::new()));
                } else if let Some(rest) = l.strip_prefix(proto::FINAL_PREFIX) {
                    push_listen(ListenEvt::Final(rest.to_string()));
                } else if let Some(rest) = l.strip_prefix(proto::STTSTATS_PREFIX) {
                    // Per-listen transcription timing → the activity log, the speech-IN
                    // mirror of the `TTS speak` line above (so a slow dictation leaves a
                    // trace, not just an in-app stats bump). DEBUG: off by default, one
                    // concise line per listen when DONTSPEAK_DEBUG is on.
                    log::debug!(target: "engine", "STT listen {rest}");
                    if let Some(secs) = stt_stats.record_stt_line(rest) {
                        lifetime.add_stt(secs);
                        if let Some(g) = gate.as_ref() {
                            g.bump();
                        }
                    }
                } else if let Some(rest) = l.strip_prefix(proto::STTERR_PREFIX) {
                    push_listen(ListenEvt::Err(rest.to_string()));
                // STT lifecycle — the SAME `ModelSlot::transition` `start()`'s wait loop
                // uses, so the pre-/post-READY paths can't drift (STT preloads in parallel →
                // its terminal lands on either side of READY).
                } else if l == proto::TTSLOADED {
                    // The Kokoro analogue of STTLOADED: the helper confirms the model is
                    // resident after a `load tts`, so the dot greens only now — not on the
                    // optimistic request. (The COMMON path for a mid-session TTS (re)select.)
                    // One write does what used to be two kept in lockstep (mark loaded +
                    // clear any stale load error) — see `ModelSlot::transition`.
                    tts_model.transition(ModelState::Loaded, gate.as_deref());
                } else if l == proto::STTLOADED {
                    stt_model.transition(ModelState::Loaded, gate.as_deref());
                } else if let Some(msg) = l.strip_prefix(proto::STTLOADERR_PREFIX) {
                    // A mid-session `load stt`/preload failure (e.g. a transient AV-scan
                    // file-not-found on an already-downloaded model) — surfaced per-model so
                    // `model_status`'s `parakeet` row can show it without touching `kokoro`.
                    // Change-gated: the exact same failure can repeat identically several
                    // times in a row and must not spam `StatusGate` each time.
                    stt_model
                        .transition(ModelState::Failed(msg.trim().to_string()), gate.as_deref());
                } else if let Some(msg) = l.strip_prefix(proto::TTSLOADERR_PREFIX) {
                    tts_model
                        .transition(ModelState::Failed(msg.trim().to_string()), gate.as_deref());
                } else if let Some(p) = l.strip_prefix(proto::STT_PROVIDER_PREFIX) {
                    // The REALIZED STT EP (mirrors the pre-READY parse in start()). Post-READY is
                    // the COMMON path — the parallel preload usually reports after READY — so
                    // this is what keeps the STT status row honest on a GPU box. The write is
                    // change-gated and BUMPS: `STTLOADED` alone would otherwise publish a row
                    // read between the child's paired lines, with no later bump to correct it.
                    store_realized_stt(&stt_realized, realized_stt_token(p), gate.as_deref());
                // ── diarize events ───────────────────────────────────────────
                } else if let Some(rest) = l.strip_prefix(proto::DIAR_PREFIX) {
                    diarize_slot.0.lock().unwrap().result = Some(Ok(rest.to_string()));
                } else if let Some(rest) = l.strip_prefix(proto::DIARERR_PREFIX) {
                    diarize_slot.0.lock().unwrap().result = Some(Err(rest.to_string()));
                } else if l == proto::DDONE {
                    let (m, cv) = &*diarize_slot;
                    m.lock().unwrap().done = true;
                    cv.notify_all();
                // ── enroll events ────────────────────────────────────────────
                } else if let Some(rest) = l.strip_prefix(proto::EMB_PREFIX) {
                    enroll_slot.0.lock().unwrap().result = Some(Ok(rest.to_string()));
                } else if let Some(rest) = l.strip_prefix(proto::ENROLLERR_PREFIX) {
                    enroll_slot.0.lock().unwrap().result = Some(Err(rest.to_string()));
                } else if l == proto::EDONE {
                    let (m, cv) = &*enroll_slot;
                    m.lock().unwrap().done = true;
                    cv.notify_all();
                }
                // else: LISTENING / PROVIDER / other chatter — ignore
            }
        }
    }
}

#[cfg(test)]
mod dl_lifecycle_tests {
    use super::describe_exit;

    #[test]
    fn describe_exit_falls_back_when_no_status_was_obtained() {
        assert_eq!(describe_exit(None), "exit status unavailable");
    }
}

#[cfg(test)]
mod reader_eof_tests {
    use super::*;
    use std::io::BufReader;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A fresh [`ModelSlot`] already transitioned to `Loaded` — the post-READY state in
    /// which a crash used to leave the old raw flags stale. No gate: these tests assert
    /// on `is_loaded()`/`error()` outcomes, not bump counts.
    fn loaded_slot() -> Arc<ModelSlot> {
        let slot = Arc::new(ModelSlot::new());
        slot.transition(ModelState::Loaded, None);
        slot
    }

    /// A canned [`ChildSlot`]: EMPTY (no real process behind it) with the
    /// deliberate-stop marker optionally pre-set — reproduces the exact
    /// `{child: None, expected_eof}` pairs the raw fields used to take, so the
    /// crash case (empty slot, `expected_eof = false`) stays representable
    /// WITHOUT spawning a real process in every reader test.
    fn canned_slot(expected_eof: bool) -> Arc<ChildSlot> {
        let slot = Arc::new(ChildSlot::new());
        if expected_eof {
            slot.begin_deliberate_stop();
        }
        slot
    }

    /// Drive `reader_loop` over a canned child stdout (ending in EOF, like a real death)
    /// and return `(tts_loaded, stt_loaded, speak_fatal)` afterwards. Both models start
    /// `Loaded` — see [`loaded_slot`].
    fn run_reader(stdout: &[u8], expected_eof: bool) -> (bool, bool, bool) {
        let dir = tempfile::tempdir().unwrap();
        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        let tts_model = loaded_slot();
        let stt_model = loaded_slot();
        reader_loop(
            stdout,
            ReaderSlots {
                speak: speak_slot.clone(),
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-eof-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: None,
                child: canned_slot(expected_eof),
            },
        );
        let fatal = speak_slot.0.lock().unwrap().fatal;
        (tts_model.is_loaded(), stt_model.is_loaded(), fatal)
    }

    #[test]
    fn unexpected_eof_unloads_both_models() {
        // A post-READY child DEATH (no teardown marked the EOF expected): the reader must
        // drop BOTH loaded flags so the status dots go amber immediately. Previously the
        // stale green survived until some later write failed — and with the flags then
        // cleared by `mark_dead`, the worker's not-ready guard dropped every speak, so the
        // crash wedged TTS+STT in "Starting" until an app restart.
        let (tts, stt, fatal) = run_reader(b"", false);
        assert!(!tts && !stt, "an unexpected EOF must unload both models");
        assert!(fatal, "a waiting speak must be unblocked as fatal");
    }

    /// Drive `reader_loop` over canned stdout WITH a real gate and return how far `seq`
    /// advanced — the only observable effect of the end-of-utterance bumps.
    fn reader_gate_bumps(stdout: &[u8]) -> u64 {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::status::StatusGate::new();
        let before = gate.seq();
        reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-gate-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: loaded_slot(),
                stt_model: loaded_slot(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: Some(gate.clone()),
                child: canned_slot(true),
            },
        );
        gate.seq().wrapping_sub(before)
    }

    #[test]
    fn end_of_utterance_stats_lines_bump_the_status_gate() {
        // The bump is the whole point of these two arms: hosts long-polling WaitModelStatus
        // repaint per-utterance stats even though `speaking` never edges.
        assert_eq!(
            reader_gate_bumps(b"STATS synth_ms=11.0 audio_ms=20.0 first_ms=2.0\n"),
            1,
            "a TTS stats line must bump once"
        );
        assert_eq!(
            reader_gate_bumps(b"STTSTATS transcribe_ms=120.0 audio_ms=500.0\n"),
            1,
            "an STT stats line must bump once"
        );
        // Unrecorded lines (audio_ms=0 / garbage / unrelated chatter) must not wake waiters.
        assert_eq!(
            reader_gate_bumps(b"STATS synth_ms=11.0 audio_ms=0.0\nSTTSTATS audio_ms=0.0\nSTATS junk\nPROGRESS 3\n"),
            0,
            "lines that record no audio must not bump"
        );
    }

    #[test]
    fn stt_provider_line_bumps_the_status_gate() {
        // The helper prints STTLOADED then STT_PROVIDER. Only the provider line carries the
        // realized backend, so it must bump on its own — otherwise a client woken by
        // STTLOADED reads the row BETWEEN the two lines and latches `provider: null`.
        assert_eq!(
            reader_gate_bumps(b"STTLOADED\n"),
            0,
            "an already-Loaded slot is change-gated to a no-op"
        );
        assert_eq!(
            reader_gate_bumps(b"STTLOADED\nSTT_PROVIDER MLX\n"),
            1,
            "the single bump is the provider line"
        );
        assert_eq!(
            reader_gate_bumps(b"STT_PROVIDER MLX\nSTT_PROVIDER MLX\n"),
            1,
            "an identical repeat must not bump"
        );
    }

    /// Drive `reader_loop` with a REAL gate and both model slots left `Idle`, and
    /// return `(realized_stt_token, seq_delta)`. Idle slots are deliberate: the
    /// unexpected-EOF branch's two `transition(Idle)` calls are then change-gated
    /// no-ops, so every observed bump is attributable to the realized-STT write or
    /// clear — which is the property under test.
    fn reader_realized_stt(stdout: &[u8], expected_eof: bool) -> (Option<String>, u64) {
        let dir = tempfile::tempdir().unwrap();
        let gate = crate::status::StatusGate::new();
        let before = gate.seq();
        let stt_realized = Arc::new(Mutex::new(None));
        reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-realized-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: Arc::new(ModelSlot::new()),
                stt_model: Arc::new(ModelSlot::new()),
                stt_realized: stt_realized.clone(),
                gate: Some(gate.clone()),
                child: canned_slot(expected_eof),
            },
        );
        let realized = stt_realized.lock().unwrap().clone();
        (realized, gate.seq().wrapping_sub(before))
    }

    #[test]
    fn unexpected_eof_drops_the_realized_stt_token() {
        // Without the clear the reader idles both models and bumps while `stt.provider`
        // still reads the dead child's "cuda" — which macOS's Runtime row would show
        // indefinitely, since `mark_dead` may not run for minutes (or ever).
        assert_eq!(
            reader_realized_stt(b"STT_PROVIDER MLX\n", false),
            (None, 2),
            "one bump for the realize, one for the crash clear — the clear must be \
             observable to a WaitModelStatus waiter"
        );
    }

    #[test]
    fn deliberate_stop_eof_leaves_the_realized_stt_token_to_the_stopper() {
        // A `stop_child`/`mark_dead` EOF is owned by `clear_loaded_flags`, mirroring the
        // same split for the loaded flags.
        assert_eq!(
            reader_realized_stt(b"STT_PROVIDER MLX\n", true),
            (Some("MLX".to_string()), 1),
        );
    }

    #[test]
    #[cfg(unix)]
    fn unexpected_eof_reads_the_real_exit_status_through_the_shared_child_handle() {
        // End-to-end through the ACTUAL wiring `start()` uses (not just a canned byte slice
        // with no child behind it): a real spawned process, sharing the same
        // `Arc<ChildSlot>` production hands the reader thread. Proves the slot's peek
        // sees a genuine ExitStatus — this is what lets a log line say WHY the child
        // died instead of "reason unknown".
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn `true`");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let _ = child.wait(); // let it actually exit before the reader sees stdout EOF
        let child_slot = Arc::new(ChildSlot::new());
        child_slot.install(child);
        // `install` reset the deliberate-stop marker — so the reader takes the
        // UNEXPECTED-EOF (crash-detection) path below, same as production.

        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        reader_loop(
            stdout,
            ReaderSlots {
                speak: speak_slot,
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-real-child-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: loaded_slot(),
                stt_model: loaded_slot(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: None,
                child: child_slot.clone(),
            },
        );

        // The peek must not have consumed/broken the handle — a real teardown (mark_dead)
        // still needs to try_wait()/kill() it afterwards without erroring.
        assert_eq!(
            child_slot.probe(),
            (true, true),
            "exit status still readable after the reader's peek"
        );
    }

    #[test]
    fn deliberate_stop_eof_leaves_the_flags_to_the_stopper() {
        // `stop_child`/`mark_dead` set `expected_eof` before killing: they own the flag
        // clearing and the logging, so the reader must NOT double-report their EOF as a
        // crash (a restart would otherwise log a spurious "exited unexpectedly" WARN).
        let (tts, stt, fatal) = run_reader(b"", true);
        assert!(
            tts && stt,
            "a deliberate stop's EOF must not touch the flags"
        );
        assert!(fatal, "waiters still unblock on any EOF");
    }

    #[test]
    fn post_ready_lines_still_route_before_eof() {
        // The genericized reader (`impl BufRead`) must keep demuxing real lines: an
        // STTLOADED before the crash re-greens STT, then the unexpected EOF clears both.
        let (tts, stt, _) = run_reader(b"STTLOADED\nDONE\n", false);
        assert!(
            !tts && !stt,
            "EOF handling runs after the lines are demuxed"
        );
    }

    /// Like `run_reader` but with EXPLICIT initial loaded states — for asserting a line
    /// FLIPS a model to `Loaded` (a load terminal greening a not-yet-resident model), not
    /// just that a crash clears one. `expected_eof=true` so the trailing EOF leaves the
    /// state exactly as the demuxed line set it (the deliberate-stop path).
    fn run_reader_init(tts0: bool, stt0: bool, stdout: &[u8]) -> (bool, bool) {
        let dir = tempfile::tempdir().unwrap();
        let mk = |loaded: bool| {
            let slot = Arc::new(ModelSlot::new());
            if loaded {
                slot.transition(ModelState::Loaded, None);
            }
            slot
        };
        let tts_model = mk(tts0);
        let stt_model = mk(stt0);
        reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-load-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: None,
                child: canned_slot(true),
            },
        );
        (tts_model.is_loaded(), stt_model.is_loaded())
    }

    #[test]
    fn ttsloaded_greens_tts_only() {
        // The Kokoro analogue of STTLOADED: a mid-session `load tts` confirms residency, so
        // the reader greens `tts_loaded` ONLY on this line — never on the optimistic request
        // (the old premature-green). Start both flags FALSE (fresh (re)load) and confirm the
        // TTS terminal flips TTS and leaves STT alone.
        let (tts, stt) = run_reader_init(false, false, b"TTSLOADED\n");
        assert!(tts, "TTSLOADED must mark the TTS model resident");
        assert!(!stt, "TTSLOADED must not touch the STT flag");
    }

    #[test]
    fn sttloaded_greens_stt_only() {
        // Symmetric guard for the STT terminal, so the two load paths can't silently swap
        // (a regression that would green dictation while narration is still warming).
        let (tts, stt) = run_reader_init(false, false, b"STTLOADED\n");
        assert!(stt, "STTLOADED must mark the STT model resident");
        assert!(!tts, "STTLOADED must not touch the TTS flag");
    }

    /// Like `run_reader_init`, but starting both models `Idle` and additionally returning
    /// their drained `error()` — for the `STTLOADERR`/`TTSLOADERR` coverage below.
    fn run_reader_init_errs(stdout: &[u8]) -> (bool, bool, Option<String>, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let tts_model = Arc::new(ModelSlot::new());
        let stt_model = Arc::new(ModelSlot::new());
        reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-loaderr-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: None,
                child: canned_slot(true),
            },
        );
        (
            tts_model.is_loaded(),
            stt_model.is_loaded(),
            stt_model.error(),
            tts_model.error(),
        )
    }

    #[test]
    fn sttloaderr_sets_the_error_without_touching_stt_loaded() {
        let (_, stt_loaded, stt_err, tts_err) = run_reader_init_errs(b"STTLOADERR boom\n");
        assert_eq!(stt_err.as_deref(), Some("boom"));
        assert_eq!(tts_err, None, "STTLOADERR must not touch tts_load_error");
        assert!(
            !stt_loaded,
            "a load FAILURE must not mark the model resident"
        );
    }

    #[test]
    fn ttsloaderr_sets_the_error_without_touching_tts_loaded() {
        let (tts_loaded, _, stt_err, tts_err) = run_reader_init_errs(b"TTSLOADERR boom\n");
        assert_eq!(tts_err.as_deref(), Some("boom"));
        assert_eq!(stt_err, None, "TTSLOADERR must not touch stt_load_error");
        assert!(
            !tts_loaded,
            "a load FAILURE must not mark the model resident"
        );
    }

    #[test]
    fn sttloaded_after_sttloaderr_clears_the_error() {
        // The AV-scan-retry scenario this whole channel exists for: a transient failure
        // followed by a successful (re)load must clear the stale error, not leave it stuck
        // showing "failed" forever alongside a now-healthy green dot.
        let (_, stt_loaded, stt_err, _) =
            run_reader_init_errs(b"STTLOADERR transient boom\nSTTLOADED\n");
        assert!(
            stt_loaded,
            "STTLOADED after the retry must mark it resident"
        );
        assert_eq!(
            stt_err, None,
            "a subsequent STTLOADED must clear the earlier STTLOADERR"
        );
    }

    /// A `Read` that yields `data` once, then BLOCKS (never reports EOF) until `close` is
    /// flipped — used to test a reader_loop line WITHOUT the trailing implicit EOF that a
    /// finite canned byte slice (as `run_reader` uses) always ends in. That EOF unconditionally
    /// sets `speak_slot.fatal = true`, which would clobber the very distinction the soft-ERR
    /// test below exists to observe (`ERR` sets `err`+`done` WITHOUT `fatal`). Wrapped in a
    /// `BufReader` (like production's real `ChildStdout`) to satisfy `reader_loop`'s bound.
    struct BlockThenClose {
        data: Vec<u8>,
        pos: usize,
        close: Arc<AtomicBool>,
    }

    impl std::io::Read for BlockThenClose {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos < self.data.len() {
                let n = buf.len().min(self.data.len() - self.pos);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            // Exhausted: block until told to close, THEN report EOF — never spontaneously.
            while !self.close.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(0)
        }
    }

    #[test]
    fn soft_err_line_sets_err_without_marking_fatal() {
        // The soft-ERR arm (child stays alive) sets `err`+`done` but leaves `fatal` false —
        // distinct from the EOF/read-error arm, which sets BOTH. Never previously exercised:
        // see `BlockThenClose` for why a plain finite byte slice can't observe this directly.
        let dir = tempfile::tempdir().unwrap();
        let close = Arc::new(AtomicBool::new(false));
        let stdout = BufReader::new(BlockThenClose {
            data: b"ERR bad phoneme\n".to_vec(),
            pos: 0,
            close: close.clone(),
        });
        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        let tts_model = loaded_slot();
        let stt_model = loaded_slot();
        let reader_speak = speak_slot.clone();
        let reader_tts = tts_model.clone();
        let reader_stt = stt_model.clone();
        let handle = std::thread::spawn(move || {
            reader_loop(
                stdout,
                ReaderSlots {
                    speak: reader_speak,
                    cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                    listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                    diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                    enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
                },
                ReaderStats {
                    tts: Arc::new(crate::stats::TtsStats::new()),
                    stt: Arc::new(crate::stats::SttStats::new()),
                    lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                        dir.path().join("ds-stats-reader-soft-err-test.json"),
                    )),
                },
                ReaderModelState {
                    tts_model: reader_tts,
                    stt_model: reader_stt,
                    stt_realized: Arc::new(Mutex::new(None)),
                    gate: None,
                    child: canned_slot(true),
                },
            );
        });

        // Wait for the soft-ERR line to land (the reader sets `done` on it, same as DONE) —
        // at this point the reader is still blocked inside `BlockThenClose::read`, so the
        // trailing EOF (and its `fatal = true`) has NOT happened yet.
        let (m, cv) = &*speak_slot;
        let mut s = m.lock().unwrap();
        while !s.done {
            s = cv.wait(s).unwrap();
        }
        let err = s.err.clone();
        let fatal = s.fatal;
        drop(s);
        let loaded_ok = tts_model.is_loaded() && stt_model.is_loaded();

        // Let the reader exit cleanly (EOF) and join it BEFORE asserting, so a future
        // regression here (which is exactly what these assertions exist to catch) can't
        // also leak the blocked reader thread spinning in `BlockThenClose::read` for the
        // rest of the test process's life.
        close.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(err.as_deref(), Some("TTS child error: bad phoneme"));
        assert!(
            !fatal,
            "a soft ERR (child stays alive) must not mark the speak fatal"
        );
        assert!(loaded_ok, "a soft ERR must not touch the loaded flags");
    }

    /// PROGRESS is an INTERMEDIATE resume mark, not a terminal: the slot's high-water
    /// mark must accumulate monotonically (a backwards value can't lower it) with
    /// malformed values ignored, none of which may set `err` or `fatal` — only the
    /// later `DONE` terminates the request. Skew guard: an older helper never emits
    /// the line, so the mark simply stays 0. Uses [`BlockThenClose`] so the trailing
    /// EOF's unconditional `err`/`fatal` can't mask what the PROGRESS lines set.
    #[test]
    fn progress_lines_accumulate_monotonically_without_erring() {
        let dir = tempfile::tempdir().unwrap();
        let close = Arc::new(AtomicBool::new(false));
        let stdout = BufReader::new(BlockThenClose {
            data: b"PROGRESS 3\nPROGRESS 2\nPROGRESS x\nDONE\n".to_vec(),
            pos: 0,
            close: close.clone(),
        });
        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        let reader_speak = speak_slot.clone();
        let handle = std::thread::spawn(move || {
            reader_loop(
                stdout,
                ReaderSlots {
                    speak: reader_speak,
                    cue: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
                    listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                    diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                    enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
                },
                ReaderStats {
                    tts: Arc::new(crate::stats::TtsStats::new()),
                    stt: Arc::new(crate::stats::SttStats::new()),
                    lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                        dir.path().join("ds-stats-reader-progress-test.json"),
                    )),
                },
                ReaderModelState {
                    tts_model: loaded_slot(),
                    stt_model: loaded_slot(),
                    stt_realized: Arc::new(Mutex::new(None)),
                    gate: None,
                    child: canned_slot(true),
                },
            );
        });

        // Wait for DONE; the reader is then still blocked pre-EOF (see BlockThenClose).
        let (m, cv) = &*speak_slot;
        let mut s = m.lock().unwrap();
        while !s.done {
            s = cv.wait(s).unwrap();
        }
        let progress = s.progress;
        let err = s.err.clone();
        let fatal = s.fatal;
        drop(s);

        close.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(progress, 3, "monotone max; backwards/malformed ignored");
        assert_eq!(err, None, "an intermediate mark must never set err");
        assert!(!fatal, "an intermediate mark must never be fatal");
    }

    /// Drained results from a `run_reader_slots` call: the `listen_slot` events (in order),
    /// then the terminal `diarize_slot` (result, done) and `enroll_slot` (result, done).
    type ReaderSlotsResult = (
        Vec<ListenEvt>,
        Option<Result<String, String>>,
        bool,
        Option<Result<String, String>>,
        bool,
        bool,
    );

    /// Drive `reader_loop` over a canned child stdout and return the drained `listen_slot`
    /// events (in order) plus the terminal `diarize_slot`/`enroll_slot` results — the
    /// Listen/Diarize/Enroll demux arms, previously exercised by no test (only
    /// `tts_loaded`/`stt_loaded`/`speak_slot.fatal` were ever asserted on in this module).
    fn run_reader_slots(stdout: &[u8]) -> ReaderSlotsResult {
        let dir = tempfile::tempdir().unwrap();
        let listen_slot = Arc::new((Mutex::new(ListenSlot::default()), Condvar::new()));
        let diarize_slot = Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new()));
        let enroll_slot = Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new()));
        let cue_slot = Arc::new((Mutex::new(CueSlot::default()), Condvar::new()));
        reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                cue: cue_slot.clone(),
                listen: listen_slot.clone(),
                diarize: diarize_slot.clone(),
                enroll: enroll_slot.clone(),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-slots-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: loaded_slot(),
                stt_model: loaded_slot(),
                stt_realized: Arc::new(Mutex::new(None)),
                gate: None,
                child: canned_slot(true),
            },
        );
        let events: Vec<ListenEvt> = listen_slot.0.lock().unwrap().events.drain(..).collect();
        let diarize = diarize_slot.0.lock().unwrap();
        let enroll = enroll_slot.0.lock().unwrap();
        (
            events,
            diarize.result.clone(),
            diarize.done,
            enroll.result.clone(),
            enroll.done,
            cue_slot.0.lock().unwrap().done,
        )
    }

    #[test]
    fn cuedone_routes_to_the_cue_slot_only() {
        let (events, diarize, diarize_done, enroll, enroll_done, cue_done) =
            run_reader_slots(b"CUEDONE\n");
        assert!(cue_done);
        assert!(events.is_empty());
        assert_eq!(diarize, None);
        assert!(!diarize_done);
        assert_eq!(enroll, None);
        assert!(!enroll_done);
    }

    #[test]
    fn listen_demux_orders_partial_final_done() {
        let (events, ..) = run_reader_slots(b"PARTIAL hi\nFINAL done\nLDONE\n");
        assert_eq!(
            events,
            vec![
                ListenEvt::Partial("hi".to_string()),
                ListenEvt::Final("done".to_string()),
                ListenEvt::Done,
            ]
        );
    }

    #[test]
    fn listen_demux_routes_sttterr() {
        let (events, ..) = run_reader_slots(b"STTERR mic denied\nLDONE\n");
        assert_eq!(
            events,
            vec![ListenEvt::Err("mic denied".to_string()), ListenEvt::Done]
        );
    }

    #[test]
    fn diarize_demux_routes_ok_result_and_done() {
        let (_, result, done, ..) = run_reader_slots(b"DIAR {\"segments\":[]}\nDDONE\n");
        assert_eq!(result, Some(Ok("{\"segments\":[]}".to_string())));
        assert!(done);
    }

    #[test]
    fn diarize_demux_routes_err_result_and_done() {
        let (_, result, done, ..) = run_reader_slots(b"DIARERR boom\nDDONE\n");
        assert_eq!(result, Some(Err("boom".to_string())));
        assert!(done);
    }

    #[test]
    fn enroll_demux_routes_ok_result_and_done() {
        let (_, _, _, result, done, _) = run_reader_slots(b"EMB [0.1,0.2]\nEDONE\n");
        assert_eq!(result, Some(Ok("[0.1,0.2]".to_string())));
        assert!(done);
    }

    #[test]
    fn enroll_demux_routes_err_result_and_done() {
        let (_, _, _, result, done, _) = run_reader_slots(b"ENROLLERR boom\nEDONE\n");
        assert_eq!(result, Some(Err("boom".to_string())));
        assert!(done);
    }
}
