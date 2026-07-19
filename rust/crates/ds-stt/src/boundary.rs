//! Live utterance segmentation for streaming dictation.
//!
//! [`VadBoundaryDetector`] reports speech→silence sample offsets on a mono PCM
//! stream at device rate. Caller owns the capture buffer and slices at those
//! offsets (backend-agnostic; no-fire degrades to whole-buffer). Segments at
//! pauses so only the short tail remains at stop (avoids `rtf × duration` lag on
//! Caps). Reuses [`transcribe_rs::vad`] for all backends; transcribe-rs's own
//! `VadChunked` is ONNX-only (`&mut dyn SpeechModel`). Frame = 30 ms at device rate;
//! caller resamples each whole segment to 16 kHz once.

use transcribe_rs::vad::{EnergyVad, SmoothedVad, Vad};

/// VAD frame duration. 30 ms is the Silero/`EnergyVad` convention and a good
/// granularity for endpointing speech.
const FRAME_MS: usize = 30;
/// RMS energy above which a frame counts as speech. Matches the dictation loop's
/// raw noise floor; below this is room hum / silence. The `SmoothedVad` onset +
/// hangover wrapping makes the exact value forgiving.
const SPEECH_RMS: f32 = 0.01;
/// Consecutive speech frames required to ENTER speech (90 ms) — rejects clicks.
const ONSET_FRAMES: usize = 3;
/// Non-speech frames tolerated before a segment CLOSES (~750 ms) — a natural
/// sentence pause, long enough not to split mid-sentence on a brief breath.
const HANGOVER_FRAMES: usize = 25;
/// Force-split monologue at 7 s so one call + live-partial tail stay bounded.
///
/// MUST stay ≥ helper `tail_partial_max` (keyed off this). Diverged 8 s preview vs
/// 20 s split left mid-length tails unpreviewable and blank until stop.
pub const MAX_SEGMENT_SECS: usize = 7;

/// Detects spoken-segment end boundaries in a live mono PCM stream at `rate` Hz.
///
/// Feed it the same samples, in the same order, that you append to your capture
/// buffer; each [`feed`](Self::feed) returns the absolute sample offsets (into that
/// fed stream) at which a segment closed. Slice `buffer[prev_boundary..boundary]`
/// to get the audio to transcribe.
pub struct VadBoundaryDetector {
    vad: SmoothedVad,
    frame: usize,
    /// Sub-frame remainder carried between `feed()` calls (samples that didn't fill
    /// a whole frame yet). Always shorter than `frame`.
    rem: Vec<f32>,
    /// Total samples consumed into COMPLETE, VAD-classified frames so far — the
    /// timeline the returned boundaries are expressed in. Equals the caller's
    /// buffer length minus the held `rem`.
    pos: usize,
    /// Whether the previous frame was inside a (smoothed) speech region.
    in_speech: bool,
    /// `pos` at the start of the current un-boundaried region (last boundary, or 0).
    /// Used only for the max-length force split.
    seg_start: usize,
}

impl VadBoundaryDetector {
    /// Build a detector for a `rate` Hz mono stream. The VAD frame is `rate`-scaled
    /// to `FRAME_MS`.
    pub fn new(rate: u32) -> Self {
        let frame = ((rate as usize * FRAME_MS) / 1000).max(1);
        let inner = EnergyVad::new(frame, SPEECH_RMS);
        // prefill_frames = 0: the caller keeps the full buffer and slices it, so we
        // never need the VAD to hand back pre-onset audio.
        let vad = SmoothedVad::new(Box::new(inner), 0, HANGOVER_FRAMES, ONSET_FRAMES);
        Self {
            vad,
            frame,
            rem: Vec::new(),
            pos: 0,
            in_speech: false,
            seg_start: 0,
        }
    }

    fn max_segment_samples(&self) -> usize {
        // frame · frames-per-second · seconds == rate · seconds, frame-aligned.
        self.frame * MAX_SEGMENT_SECS * 1000 / FRAME_MS
    }

    /// Classify the newly captured `samples` and return the absolute sample offsets
    /// (in the fed-stream timeline) where a spoken segment ended. A boundary is
    /// emitted on each speech→silence transition (hangover expired) and whenever an
    /// unbroken speech run exceeds [`MAX_SEGMENT_SECS`].
    pub fn feed(&mut self, samples: &[f32]) -> Vec<usize> {
        let mut boundaries = Vec::new();
        self.rem.extend_from_slice(samples);
        let fs = self.frame;
        let max = self.max_segment_samples();

        let mut i = 0;
        while i + fs <= self.rem.len() {
            let frame = &self.rem[i..i + fs];
            // SmoothedVad updates its onset/hangover state here; `in_speech()`
            // queries the resulting region state.
            let _ = self.vad.is_speech(frame);
            let now_speech = self.vad.in_speech();
            self.pos += fs;
            i += fs;

            if self.in_speech && !now_speech {
                boundaries.push(self.pos);
                self.seg_start = self.pos;
            } else if now_speech && self.pos - self.seg_start >= max {
                // Force-split pause-free monologue (see MAX_SEGMENT_SECS).
                boundaries.push(self.pos);
                self.seg_start = self.pos;
            }
            self.in_speech = now_speech;
        }
        self.rem.drain(..i);
        boundaries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn speech(frames: usize) -> Vec<f32> {
        vec![0.2f32; frames * (RATE as usize * FRAME_MS / 1000)] // above SPEECH_RMS
    }
    fn silence(frames: usize) -> Vec<f32> {
        vec![0.0f32; frames * (RATE as usize * FRAME_MS / 1000)]
    }

    #[test]
    fn no_boundary_until_speech_then_silence() {
        let mut d = VadBoundaryDetector::new(RATE);
        assert!(d.feed(&speech(10)).is_empty());
        let b = d.feed(&silence(HANGOVER_FRAMES + 5));
        assert_eq!(b.len(), 1, "one speech→silence boundary");
    }

    #[test]
    fn pure_silence_never_boundaries() {
        let mut d = VadBoundaryDetector::new(RATE);
        assert!(d.feed(&silence(100)).is_empty());
    }

    #[test]
    fn boundary_offset_is_frame_aligned_and_within_fed_length() {
        let mut d = VadBoundaryDetector::new(RATE);
        let mut fed = 0usize;
        let a = speech(10);
        fed += a.len();
        d.feed(&a);
        let s = silence(HANGOVER_FRAMES + 5);
        fed += s.len();
        let b = d.feed(&s);
        let frame = RATE as usize * FRAME_MS / 1000;
        assert_eq!(b.len(), 1);
        assert!(b[0].is_multiple_of(frame), "boundary is frame-aligned");
        assert!(b[0] <= fed, "boundary within the fed sample count");
    }

    #[test]
    fn force_split_lands_exactly_at_max_segment() {
        // Regression guard for the "overlay goes blank mid-monologue" bug: the live-
        // partial tail in the dictation helper is previewable only while it fits the
        // re-pass budget, which is keyed off MAX_SEGMENT_SECS. If the force-split lands
        // LATER than MAX_SEGMENT_SECS — or the constant is bumped large again — a pause-
        // free phrase grows a tail too long to preview but too short to commit, and the
        // overlay shows nothing until stop. Pin both: the split fires AT the bound, and
        // the bound stays small enough to keep the preview cost (and lag) reasonable.
        let mut d = VadBoundaryDetector::new(RATE);
        let fps = 1000 / FRAME_MS;
        let frame = RATE as usize * FRAME_MS / 1000;
        let b = d.feed(&speech((MAX_SEGMENT_SECS + 2) * fps));
        let exact = RATE as usize * MAX_SEGMENT_SECS;
        let max = exact.div_ceil(frame) * frame;
        assert_eq!(
            b[0], max,
            "first force-split must be the first frame at or after MAX_SEGMENT_SECS"
        );
        // Compile-time invariant (not a runtime check on a constant): force-split must stay
        // within the helper's live-partial tail budget (~8 s) or the dictation overlay goes
        // blank on long pause-free speech.
        const _: () = assert!(MAX_SEGMENT_SECS <= 8);
    }

    #[test]
    fn sub_frame_blocks_reassemble_across_feeds() {
        let mut d = VadBoundaryDetector::new(RATE);
        let frame = RATE as usize * FRAME_MS / 1000;
        // Sub-frame slivers must reassemble across feeds.
        let sp = speech(10);
        for chunk in sp.chunks(frame / 3 + 1) {
            d.feed(chunk);
        }
        let si = silence(HANGOVER_FRAMES + 5);
        let mut total = 0;
        for chunk in si.chunks(frame / 3 + 1) {
            total += d.feed(chunk).len();
        }
        assert_eq!(total, 1, "boundary survives sub-frame fragmentation");
    }
}
