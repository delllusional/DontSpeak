//! Incremental rodio playback sink — 24 kHz mono f32, one per speak request.
//!
//! Owns the three behaviours the warm serve loop (`ds-helper --serve`) and the
//! non-macOS one-shot player ([`crate::play`]) share:
//!
//!   * **Onset lead silence.** A short silent buffer is prepended whenever the sink
//!     starts (or restarts) from drained, so the output-stream RESUME latency (rodio
//!     pauses the CoreAudio output when idle) is absorbed by silence instead of
//!     clipping the speech onset — the "first speak, purple icon, no sound" fix.
//!   * **Wall-clock drain detection.** rodio's `empty()` lies on WASAPI (it reports
//!     true before the mixer consumed freshly appended buffers), so drained-ness is
//!     computed deterministically from wall time vs. appended audio (`AppendClock`).
//!   * **Played-batch accounting.** Each appended PCM batch records the cumulative
//!     queued duration at its end; [`played_batches`](crate::sink::IncrementalSink::played_batches)
//!     estimates how many batches have fully sounded by a given instant — the basis
//!     for batch-granular resume after a barge (lead silence is never counted as a
//!     batch).
//!
//! NO-AUDIO DISCIPLINE: unit tests construct via
//! [`IncrementalSink::connect_to`](crate::sink::IncrementalSink::connect_to) on a
//! detached `rodio::mixer::Mixer` (no output device) and drive the clock with injected
//! instants; [`IncrementalSink::open_default`](crate::sink::IncrementalSink::open_default)
//! opens a real device and is exercised
//! only by the ds-helper binary.

use std::num::NonZero;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

use crate::vocab::SAMPLE_RATE;

/// Leading silence prepended whenever the sink starts from drained (first append, or
/// synthesis fell behind real time mid-utterance), so the output-stream resume never
/// clips an onset. Pure + unit-tested so it can't silently regress to 0 samples
/// (which would re-break the onset).
const LEAD_SILENCE_MS: u32 = 80;

// Compile-time invariant: too little lead won't cover the rodio output-stream
// resume latency.
const _: () = assert!(LEAD_SILENCE_MS >= 40);

/// `LEAD_SILENCE_MS` of mono silence at `srate_hz`. See [`LEAD_SILENCE_MS`].
fn leading_silence_pcm(srate_hz: u32) -> Vec<f32> {
    vec![0.0f32; srate_hz as usize * LEAD_SILENCE_MS as usize / 1000]
}

/// Wall-clock drain detector for the incremental rodio path. `empty()` lies on WASAPI
/// (see the module doc), so drained-ness is deterministic: once more wall time has
/// elapsed in this playback run than audio was appended, the sink idled and the next
/// append needs a fresh leading silence to absorb the output-stream resume (the
/// "purple icon, no sound" clip, reachable mid-utterance because batches stream while
/// later inference runs).
#[derive(Default)]
struct AppendClock {
    started: Option<Instant>,
    queued: Duration,
}

impl AppendClock {
    fn drained(&self, now: Instant) -> bool {
        self.started
            .is_none_or(|t| now.saturating_duration_since(t) >= self.queued)
    }

    fn begin_run(&mut self, now: Instant) {
        self.started = Some(now);
        self.queued = Duration::ZERO;
    }

    fn append(&mut self, samples: usize, srate_hz: u32) {
        self.queued += Duration::from_secs_f64(samples as f64 / f64::from(srate_hz));
    }
}

/// A per-request incremental playback sink: append validated PCM batches as they are
/// committed, then [`wait`](Self::wait) for the tail (or [`stop`](Self::stop) on barge).
pub struct IncrementalSink {
    // `player` must drop before `_device` — declare it first.
    player: Arc<Player>,
    /// `Some` only for [`open_default`](Self::open_default) (the sink owns its output
    /// device); [`connect_to`](Self::connect_to) callers own a persistent device and
    /// hand in its mixer.
    _device: Option<MixerDeviceSink>,
    clock: AppendClock,
    /// Batches fully played in earlier, already-drained runs (see `boundaries`).
    completed: usize,
    /// Cumulative queued duration at each REAL-PCM batch end within the current run.
    /// Lead-silence appends extend the durations but never add a boundary, so they are
    /// never counted as batches.
    boundaries: Vec<Duration>,
}

impl IncrementalSink {
    /// Open the default output device and connect a fresh player to it — the one-shot
    /// path, where no persistent device exists.
    pub fn open_default() -> Result<Self, String> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("open audio output: {e}"))?;
        let player = Arc::new(Player::connect_new(device.mixer()));
        Ok(Self {
            player,
            _device: Some(device),
            clock: AppendClock::default(),
            completed: 0,
            boundaries: Vec::new(),
        })
    }

    /// Connect a fresh player to a caller-owned mixer (the warm serve loop's persistent
    /// output device — or, in tests, a detached mixer with no device behind it).
    pub fn connect_to(mixer: &rodio::mixer::Mixer) -> Self {
        Self {
            player: Arc::new(Player::connect_new(mixer)),
            _device: None,
            clock: AppendClock::default(),
            completed: 0,
            boundaries: Vec::new(),
        }
    }

    /// The shared player handle — for out-of-band barge (`stop()` is a non-blocking
    /// flag) and mute volume, both owned by the caller's policy, not this sink.
    pub fn player(&self) -> Arc<Player> {
        self.player.clone()
    }

    /// Append one committed batch of 24 kHz mono f32 PCM. Re-prepends the leading
    /// silence whenever the sink drained first — deliberately conservative: a
    /// borderline call gets an extra 80 ms of inaudible silence versus a clipped onset.
    pub fn append(&mut self, pcm: Vec<f32>) {
        self.append_at(Instant::now(), pcm);
    }

    /// Clock-injectable body of [`append`](Self::append) (tests fabricate `now`).
    fn append_at(&mut self, now: Instant, pcm: Vec<f32>) {
        if pcm.is_empty() {
            return;
        }
        let channels = NonZero::new(1u16).expect("1 channel");
        let rate = NonZero::new(SAMPLE_RATE).expect("24000 sample rate");
        if self.clock.drained(now) {
            // The previous run drained fully, so every batch it queued has sounded:
            // roll its boundaries into `completed` before starting the new run.
            self.completed += self.boundaries.len();
            self.boundaries.clear();
            self.clock.begin_run(now);
            let lead = leading_silence_pcm(SAMPLE_RATE);
            self.clock.append(lead.len(), SAMPLE_RATE);
            self.player.append(SamplesBuffer::new(channels, rate, lead));
        }
        self.clock.append(pcm.len(), SAMPLE_RATE);
        self.boundaries.push(self.clock.queued);
        self.player.append(SamplesBuffer::new(channels, rate, pcm));
    }

    /// How many appended batches have fully PLAYED by `now` — a wall-clock estimate
    /// (committed audio races ahead of the playhead, so commit counts would over-skip).
    /// Callers on a cancelled path must cap `now` at the audible-stop instant, or wall
    /// time keeps "playing" boundaries nobody heard.
    ///
    /// Known tolerances (accepted; resume misses at most a few words, exactly once):
    /// a machine suspend or output-device stall mid-utterance inflates elapsed wall
    /// time, so a barge right after resume can over-count batches the user never
    /// heard; and a batch whose final tens of milliseconds were still in the DAC at
    /// the stop instant counts as played — the symmetric twin of the ~60 ms fade-tail
    /// under-skip the cancel-instant cap accepts.
    pub fn played_batches(&self, now: Instant) -> usize {
        let Some(started) = self.clock.started else {
            return self.completed;
        };
        let elapsed = now.saturating_duration_since(started);
        self.completed + self.boundaries.iter().filter(|b| **b <= elapsed).count()
    }

    /// Block until everything appended has played (returns early on `stop`).
    pub fn wait(&self) {
        self.player.sleep_until_end();
    }

    /// Stop playback and drop anything still queued (non-blocking flag).
    pub fn stop(&self) {
        self.player.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{AppendClock, IncrementalSink, LEAD_SILENCE_MS, leading_silence_pcm};

    /// A sink over a detached mixer: NO output device is opened (see the module doc's
    /// NO-AUDIO DISCIPLINE) — appends queue in the player and never sound.
    fn detached_sink() -> IncrementalSink {
        let (mixer, _source) = rodio::mixer::mixer(
            std::num::NonZero::new(1u16).unwrap(),
            std::num::NonZero::new(24_000u32).unwrap(),
        );
        IncrementalSink::connect_to(&mixer)
    }

    /// Regression guard for the "first speak, no sound" fix: every utterance must be preceded by
    /// a NON-EMPTY, fully-SILENT leading buffer so the rodio output-stream resume is absorbed
    /// instead of clipping the speech onset. If someone drops the prepend or zeroes its duration,
    /// this fails.
    #[test]
    fn leading_silence_is_nonempty_and_pure_silence() {
        let pcm = leading_silence_pcm(24_000);
        // ~80 ms @ 24 kHz mono = 1920 samples — and NEVER empty (empty re-breaks the onset).
        assert_eq!(pcm.len(), 24_000 * LEAD_SILENCE_MS as usize / 1000);
        assert_eq!(pcm.len(), 1_920);
        assert!(
            !pcm.is_empty(),
            "leading silence must not regress to 0 samples"
        );
        // Pure silence — a non-zero lead would be an audible click before every reply.
        assert!(pcm.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn leading_silence_scales_with_sample_rate() {
        // Duration is fixed; sample count tracks the rate.
        assert_eq!(
            leading_silence_pcm(48_000).len(),
            48_000 * LEAD_SILENCE_MS as usize / 1000
        );
    }

    /// Pins the mid-utterance onset clip fix: the FIRST commit of a request must read as
    /// drained so it gets the leading silence (the old first-commit-only prepend).
    #[test]
    fn append_clock_reads_drained_before_any_run() {
        let clock = AppendClock::default();
        assert!(clock.drained(Instant::now()));
    }

    /// While appended audio outpaces wall time the sink is still busy — re-prepending the
    /// silence there would inject an audible gap mid-utterance.
    #[test]
    fn append_clock_is_not_drained_while_queued_audio_outpaces_wall_time() {
        let t0 = Instant::now();
        let mut clock = AppendClock::default();
        clock.begin_run(t0);
        clock.append(24_000, 24_000); // 1 s of audio
        assert!(!clock.drained(t0 + Duration::from_millis(500)));
    }

    /// Wall time catching up with the appended total means the sink idled (we distrust
    /// WASAPI's `empty()` — see the module doc) → the next append needs a fresh
    /// leading silence.
    #[test]
    fn append_clock_drains_once_wall_time_reaches_the_queued_total() {
        let t0 = Instant::now();
        let mut clock = AppendClock::default();
        clock.begin_run(t0);
        clock.append(24_000, 24_000); // 1 s of audio
        assert!(clock.drained(t0 + Duration::from_secs(1)));
        assert!(clock.drained(t0 + Duration::from_secs(2)));
    }

    /// `begin_run` must reset the accounting: a stale `queued` total from the previous run
    /// would postpone the next drain detection past the real sink idle.
    #[test]
    fn append_clock_begin_run_resets_the_accounting() {
        let t0 = Instant::now();
        let mut clock = AppendClock::default();
        clock.begin_run(t0);
        clock.append(24_000 * 10, 24_000); // 10 s of audio in run 1
        let t1 = t0 + Duration::from_secs(30);
        clock.begin_run(t1);
        clock.append(2_400, 24_000); // 100 ms in run 2
        assert!(!clock.drained(t1 + Duration::from_millis(50)));
        assert!(clock.drained(t1 + Duration::from_millis(100)));
    }

    /// The resume-mark contract: a batch counts as played only once wall time reaches its
    /// cumulative end (lead silence included in the durations), and a capped `now` earlier
    /// than real time never counts unheard boundaries (the cancel-instant cap).
    #[test]
    fn played_batches_counts_only_boundaries_wall_time_has_passed() {
        let mut s = detached_sink();
        let t0 = Instant::now();
        s.append_at(t0, vec![0.1; 24_000]); // lead 80 ms + 1 s → boundary at 1.08 s
        s.append_at(t0 + Duration::from_millis(10), vec![0.1; 12_000]); // +0.5 s → 1.58 s
        assert_eq!(s.played_batches(t0), 0);
        // Past the lead silence but inside batch 1: the lead is never itself a batch.
        assert_eq!(s.played_batches(t0 + Duration::from_millis(90)), 0);
        assert_eq!(s.played_batches(t0 + Duration::from_millis(1_079)), 0);
        assert_eq!(s.played_batches(t0 + Duration::from_millis(1_080)), 1);
        assert_eq!(s.played_batches(t0 + Duration::from_millis(1_580)), 2);
        // A capped `now` (cancel landed earlier) must not count later boundaries.
        assert_eq!(s.played_batches(t0 + Duration::from_millis(500)), 0);
    }

    /// A drained run rolls its batches into `completed` and the re-lead starts a fresh
    /// run — counts accumulate across runs instead of resetting.
    #[test]
    fn played_batches_carries_completed_runs_across_a_drain() {
        let mut s = detached_sink();
        let t0 = Instant::now();
        s.append_at(t0, vec![0.1; 2_400]); // run 1: lead 80 ms + 100 ms → boundary 0.18 s
        // Wall time passes the whole run → drained; the next append begins run 2.
        let t1 = t0 + Duration::from_secs(5);
        s.append_at(t1, vec![0.1; 2_400]);
        assert_eq!(s.played_batches(t1), 1, "run 1's batch is completed");
        assert_eq!(s.played_batches(t1 + Duration::from_millis(179)), 1);
        assert_eq!(s.played_batches(t1 + Duration::from_millis(180)), 2);
    }

    /// Before anything is appended there is nothing to count — and an empty PCM append
    /// is a no-op (no lead, no boundary), matching the old one-shot enqueue guard.
    #[test]
    fn played_batches_ignores_empty_appends() {
        let mut s = detached_sink();
        let t0 = Instant::now();
        assert_eq!(s.played_batches(t0 + Duration::from_secs(10)), 0);
        s.append_at(t0, Vec::new());
        assert_eq!(s.played_batches(t0 + Duration::from_secs(10)), 0);
    }
}
