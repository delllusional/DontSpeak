//! Live TTS engine stats for the app's "Engine stats" view (ARCHITECTURE: the
//! engine measures, the app renders). The warm Kokoro child emits a `STATS …`
//! line per utterance (synth time, audio produced, time-to-first-audio); the
//! `TtsManager` parses it and records here. Surfaced via `model_status`'s `stats`
//! key (polled), so there is no extra IPC/FFI.
//!
//! The headline number is the **realtime factor** (synth_ms / audio_ms): < 1.0
//! means faster than real time. We keep min/avg/max realtime + time-to-first-audio,
//! lifetime totals, and a failure count.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct TtsStats {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    utterances: u64,
    total_audio_ms: f64,
    total_synth_ms: f64,
    total_first_ms: f64,
    failures: u64,
    // Min/max over the session (seeded on the first utterance).
    rtf_min: f64,
    rtf_max: f64,
    first_min: f64,
    first_max: f64,
}

impl TtsStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all counters (called when the execution provider changes, so the
    /// range bars reflect only the new provider).
    pub fn reset(&self) {
        *self.inner.lock().unwrap() = Inner::default();
    }

    /// Record one completed utterance's timing (ignored if it produced no audio).
    pub fn record(&self, synth_ms: f64, audio_ms: f64, first_ms: f64) {
        if audio_ms <= 0.0 {
            return;
        }
        let mut s = self.inner.lock().unwrap();
        let rtf = synth_ms / audio_ms;
        s.utterances += 1;
        s.total_audio_ms += audio_ms;
        s.total_synth_ms += synth_ms;
        s.total_first_ms += first_ms;
        // Seed min/max on the first utterance (defaults are 0.0, which would
        // otherwise pin the minimum at zero forever).
        if s.utterances == 1 {
            s.rtf_min = rtf;
            s.rtf_max = rtf;
            s.first_min = first_ms;
            s.first_max = first_ms;
        } else {
            s.rtf_min = s.rtf_min.min(rtf);
            s.rtf_max = s.rtf_max.max(rtf);
            s.first_min = s.first_min.min(first_ms);
            s.first_max = s.first_max.max(first_ms);
        }
    }

    /// Record a failed warm-speak (the engine then falls back to the cold path).
    pub fn record_failure(&self) {
        self.inner.lock().unwrap().failures += 1;
    }

    /// Parse a `STATS synth_ms=.. audio_ms=.. first_ms=..` line from the child and
    /// record it. Unknown/garbled lines are ignored. Returns the recorded audio
    /// duration in SECONDS (for the lifetime total), or `None` if nothing recorded.
    pub fn record_stats_line(&self, rest: &str) -> Option<f64> {
        let (mut synth, mut audio, mut first) = (0.0, 0.0, 0.0);
        for kv in rest.split_whitespace() {
            if let Some(v) = kv.strip_prefix("synth_ms=") {
                synth = v.parse().unwrap_or(0.0);
            } else if let Some(v) = kv.strip_prefix("audio_ms=") {
                audio = v.parse().unwrap_or(0.0);
            } else if let Some(v) = kv.strip_prefix("first_ms=") {
                first = v.parse().unwrap_or(0.0);
            }
        }
        if audio > 0.0 {
            self.record(synth, audio, first);
            Some(audio / 1000.0)
        } else {
            None
        }
    }

    /// Typed snapshot for `model_status`'s `stats.tts` key (the shared schema type the
    /// FFI hands the apps — one definition, no hand-written DTOs).
    pub fn snapshot(&self) -> ds_status::TtsSnapshot {
        let s = self.inner.lock().unwrap();
        let avg_rtf = if s.total_audio_ms > 0.0 {
            s.total_synth_ms / s.total_audio_ms
        } else {
            0.0
        };
        let first_avg = if s.utterances > 0 {
            s.total_first_ms / s.utterances as f64
        } else {
            0.0
        };
        ds_status::TtsSnapshot {
            rtf_avg: avg_rtf,
            rtf_min: s.rtf_min,
            rtf_max: s.rtf_max,
            first_avg_ms: first_avg,
            first_min_ms: s.first_min,
            first_max_ms: s.first_max,
            utterances: s.utterances,
            audio_secs: s.total_audio_ms / 1000.0,
            failures: s.failures,
        }
    }

    /// JSON form of [`snapshot`](Self::snapshot) for the unit tests (the producer path
    /// uses the typed `snapshot`).
    #[cfg(test)]
    pub fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self.snapshot()).unwrap()
    }
}

/// Live Parakeet STT stats — the speech-IN counterpart of [`TtsStats`], fed by
/// the helper's per-utterance `STTSTATS` line. Realtime factor = transcribe time
/// / audio duration (lower = faster); min/avg/max + counts, mirroring TTS.
#[derive(Default)]
pub struct SttStats {
    inner: Mutex<SttInner>,
}

#[derive(Default)]
struct SttInner {
    rtf_min: f64,
    rtf_max: f64,
    total_transcribe_ms: f64,
    total_audio_ms: f64,
    count: u64,
    failures: u64,
}

impl SttStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset all counters — called when the warm child restarts (e.g. a provider
    /// change), so the range bars reflect only the post-restart window. Mirrors
    /// [`TtsStats::reset`]; the child hosts both engines, so they reset together.
    pub fn reset(&self) {
        *self.inner.lock().unwrap() = SttInner::default();
    }

    pub fn record(&self, transcribe_ms: f64, audio_ms: f64) {
        if audio_ms <= 0.0 {
            return;
        }
        let mut s = self.inner.lock().unwrap();
        let rtf = transcribe_ms / audio_ms;
        s.count += 1;
        s.total_transcribe_ms += transcribe_ms;
        s.total_audio_ms += audio_ms;
        if s.count == 1 {
            s.rtf_min = rtf;
            s.rtf_max = rtf;
        } else {
            s.rtf_min = s.rtf_min.min(rtf);
            s.rtf_max = s.rtf_max.max(rtf);
        }
    }

    pub fn record_failure(&self) {
        self.inner.lock().unwrap().failures += 1;
    }

    /// Parse a `transcribe_ms=.. audio_ms=..` STTSTATS line and record it. Returns
    /// the recorded audio duration in SECONDS (for the lifetime total), or `None`.
    pub fn record_stt_line(&self, rest: &str) -> Option<f64> {
        let (mut tr, mut audio) = (0.0, 0.0);
        for kv in rest.split_whitespace() {
            if let Some(v) = kv.strip_prefix("transcribe_ms=") {
                tr = v.parse().unwrap_or(0.0);
            } else if let Some(v) = kv.strip_prefix("audio_ms=") {
                audio = v.parse().unwrap_or(0.0);
            }
        }
        if audio > 0.0 {
            self.record(tr, audio);
            Some(audio / 1000.0)
        } else {
            None
        }
    }

    /// Typed snapshot for `model_status`'s `stats.stt` key (the shared schema type).
    pub fn snapshot(&self) -> ds_status::SttSnapshot {
        let s = self.inner.lock().unwrap();
        let rtf_avg = if s.total_audio_ms > 0.0 {
            s.total_transcribe_ms / s.total_audio_ms
        } else {
            0.0
        };
        ds_status::SttSnapshot {
            rtf_avg,
            rtf_min: s.rtf_min,
            rtf_max: s.rtf_max,
            transcriptions: s.count,
            audio_secs: s.total_audio_ms / 1000.0,
            failures: s.failures,
        }
    }

    /// JSON form of [`snapshot`](Self::snapshot) for the unit tests.
    #[cfg(test)]
    pub fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self.snapshot()).unwrap()
    }
}

/// How often, at most, the lifetime totals are flushed to disk. CORR-2: the
/// in-memory add (a counter bump) happens on EVERY utterance on the warm-child
/// stdout-demux reader thread — the same thread that carries DONE/FINAL — but the
/// fs::write+rename is throttled to this interval so a busy speak/listen session
/// can't stall that latency-sensitive thread on disk IO per line. A crash inside
/// the window loses at most this much of the lifetime tally (whole-second totals
/// for an About-screen counter — acceptable); `flush()` on shutdown persists the
/// remainder on a clean stop.
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(5);

/// Persisted lifetime usage totals: seconds of audio spoken (TTS) and heard (STT),
/// summed across EVERY session. Unlike the live stats above, this is never reset —
/// it is loaded from a tiny TOML file (`stats.toml`) at engine start and
/// kept in memory, with the disk write DEBOUNCED (see [`PERSIST_DEBOUNCE`]) off the
/// reader thread plus a final [`flush`](Self::flush) on shutdown. Surfaced in
/// `model_status` for the About screen's lifetime counters.
pub struct LifetimeSeconds {
    inner: Mutex<LifetimeInner>,
    path: PathBuf,
    /// Last time the in-memory totals were written to disk. Guards the debounce so
    /// the per-line add only triggers an actual fs write every PERSIST_DEBOUNCE.
    last_persist: Mutex<Instant>,
    /// Set when an add changed the in-memory totals but the debounce skipped the
    /// disk write — so `flush()` knows there is unpersisted data to write out.
    dirty: AtomicBool,
}

/// On-disk shape of `stats.toml`. WHOLE seconds — the sub-second part of a lifetime
/// total is noise, so these are integers. (De)serialized directly with `toml`;
/// `#[serde(default)]` makes a missing key fail-open to 0 (a partial file still loads).
#[derive(Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct LifetimeInner {
    #[serde(default)]
    tts_secs: u64,
    #[serde(default)]
    stt_secs: u64,
}

impl LifetimeSeconds {
    /// Load the totals from `path`; zeros if the file is missing, unreadable, or
    /// garbled (a corrupt file just restarts the count rather than failing).
    pub fn load(path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str::<LifetimeInner>(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Mutex::new(inner),
            path,
            // Seed one debounce window in the past so the FIRST add of a session
            // persists immediately (it then throttles subsequent adds).
            last_persist: Mutex::new(
                Instant::now()
                    .checked_sub(PERSIST_DEBOUNCE)
                    .unwrap_or_else(Instant::now),
            ),
            dirty: AtomicBool::new(false),
        }
    }

    /// Add seconds of spoken audio (TTS). Cheap in-memory bump; the disk write is
    /// DEBOUNCED off the (reader) caller — see [`PERSIST_DEBOUNCE`].
    pub fn add_tts(&self, secs: f64) {
        if secs <= 0.0 {
            return;
        }
        let snap = {
            let mut s = self.inner.lock().unwrap();
            s.tts_secs += secs.round() as u64; // whole seconds; sub-second is noise
            *s
        };
        self.maybe_persist(snap);
    }

    /// Add seconds of heard audio (STT). Cheap in-memory bump; the disk write is
    /// DEBOUNCED off the (reader) caller — see [`PERSIST_DEBOUNCE`].
    pub fn add_stt(&self, secs: f64) {
        if secs <= 0.0 {
            return;
        }
        let snap = {
            let mut s = self.inner.lock().unwrap();
            s.stt_secs += secs.round() as u64; // whole seconds; sub-second is noise
            *s
        };
        self.maybe_persist(snap);
    }

    /// Typed snapshot for `model_status`'s `stats.lifetime` key. Reads the live
    /// in-memory totals, so it reflects un-persisted adds too.
    pub fn snapshot(&self) -> ds_status::LifetimeSnapshot {
        let s = *self.inner.lock().unwrap();
        ds_status::LifetimeSnapshot {
            tts_secs: s.tts_secs,
            stt_secs: s.stt_secs,
        }
    }

    /// JSON form of [`snapshot`](Self::snapshot) for the unit tests.
    #[cfg(test)]
    pub fn snapshot_json(&self) -> serde_json::Value {
        serde_json::to_value(self.snapshot()).unwrap()
    }

    /// Persist NOW iff at least [`PERSIST_DEBOUNCE`] has elapsed since the last
    /// write; otherwise just mark the totals dirty so a later add or [`flush`] picks
    /// them up. This is what keeps the per-line add off the disk on the hot reader
    /// thread.
    ///
    /// [`flush`]: Self::flush
    fn maybe_persist(&self, snap: LifetimeInner) {
        let mut last = self.last_persist.lock().unwrap();
        if last.elapsed() >= PERSIST_DEBOUNCE {
            *last = Instant::now();
            // Cleared BEFORE the write: a concurrent add that re-dirties while we
            // write is safe (it just re-flushes next time).
            self.dirty.store(false, Ordering::Relaxed);
            drop(last); // don't hold the debounce lock across the disk write
            self.persist(snap);
        } else {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Force a final persist of the latest in-memory totals if anything is unwritten
    /// (the debounce may have skipped the last add). Called on engine shutdown so a
    /// clean stop never drops the tail of the session's tally. No-op when nothing is
    /// pending.
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let snap = *self.inner.lock().unwrap();
        *self.last_persist.lock().unwrap() = Instant::now();
        self.persist(snap);
    }

    /// Atomic write (temp file + rename) so a crash mid-write can't corrupt the file.
    fn persist(&self, snap: LifetimeInner) {
        // Serialize the typed struct straight to TOML (u64 seconds — no NaN/inf to worry
        // about).
        let Ok(text) = toml::to_string(&snap) else {
            return;
        };
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf_and_totals_accumulate() {
        let st = TtsStats::new();
        st.record(550.0, 1000.0, 200.0); // rtf 0.55
        st.record(600.0, 1000.0, 180.0); // rtf 0.60
        let j = st.snapshot_json();
        assert_eq!(j["utterances"], 2);
        let avg = j["rtf_avg"].as_f64().unwrap();
        assert!(
            (avg - 0.575).abs() < 1e-6,
            "avg rtf = total_synth/total_audio"
        );
        assert_eq!(j["audio_secs"], 2.0);
        // min/max over the two utterances (rtf 0.55, 0.60; first 200, 180).
        assert!((j["rtf_min"].as_f64().unwrap() - 0.55).abs() < 1e-9);
        assert!((j["rtf_max"].as_f64().unwrap() - 0.60).abs() < 1e-9);
        assert_eq!(j["first_min_ms"], 180.0);
        assert_eq!(j["first_max_ms"], 200.0);
        assert_eq!(j["first_avg_ms"], 190.0);
        st.reset();
        assert_eq!(st.snapshot_json()["utterances"], 0);
    }

    #[test]
    fn parses_a_stats_line() {
        let st = TtsStats::new();
        // Returns the recorded audio seconds (20 ms → 0.02 s), None for garbage.
        assert_eq!(
            st.record_stats_line("synth_ms=11.0 audio_ms=20.0 first_ms=2.0"),
            Some(0.02)
        );
        assert_eq!(st.record_stats_line("garbage"), None); // no audio → not recorded
        assert_eq!(st.snapshot_json()["utterances"], 1);
    }

    #[test]
    fn lifetime_accumulates_and_survives_reload() {
        let mut path = std::env::temp_dir();
        path.push(format!("ds-stats-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let lt = LifetimeSeconds::load(path.clone());
        lt.add_tts(1.4); // rounds to 1
        lt.add_stt(2.0);
        lt.add_tts(0.6); // rounds to 1 → 2 s TTS total (whole seconds)
        assert_eq!(lt.snapshot_json()["tts_secs"], 2);
        assert_eq!(lt.snapshot_json()["stt_secs"], 2);

        // CORR-2: the disk write is now debounced off the (reader) caller, so the
        // adds after the first are still only in memory. flush() (called on engine
        // shutdown) persists the tail so a clean stop never drops it.
        lt.flush();

        // A fresh load reads the persisted totals back from disk.
        let reloaded = LifetimeSeconds::load(path.clone());
        assert_eq!(reloaded.snapshot_json()["tts_secs"], 2);
        assert_eq!(reloaded.snapshot_json()["stt_secs"], 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lifetime_debounces_disk_writes_but_flush_persists() {
        // The per-line add must NOT hit the disk every time (CORR-2): only the
        // first add of a session persists immediately; subsequent adds inside the
        // debounce window stay in memory until flush() (or a later, debounced add).
        let mut path = std::env::temp_dir();
        path.push(format!("ds-stats-debounce-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let lt = LifetimeSeconds::load(path.clone());
        lt.add_tts(3.0); // first add → persists immediately (seeded one window back)
        assert_eq!(
            LifetimeSeconds::load(path.clone()).snapshot_json()["tts_secs"],
            3,
            "first add persists right away"
        );

        lt.add_tts(4.0); // within the debounce window → in memory only, not on disk
        assert_eq!(
            LifetimeSeconds::load(path.clone()).snapshot_json()["tts_secs"],
            3,
            "debounced add must NOT touch disk yet"
        );
        assert_eq!(
            lt.snapshot_json()["tts_secs"],
            7,
            "but the in-memory total reflects it"
        );

        lt.flush(); // shutdown flush writes the pending tail
        assert_eq!(
            LifetimeSeconds::load(path.clone()).snapshot_json()["tts_secs"],
            7,
            "flush persists the debounced remainder"
        );

        // flush() with nothing pending is a no-op (does not error).
        lt.flush();
        assert_eq!(
            LifetimeSeconds::load(path.clone()).snapshot_json()["tts_secs"],
            7
        );

        let _ = std::fs::remove_file(&path);
    }
}
