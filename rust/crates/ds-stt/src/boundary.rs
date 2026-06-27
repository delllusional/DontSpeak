//! Live utterance segmentation for streaming dictation.
//!
//! [`VadBoundaryDetector`] watches a mono PCM stream (at the capture device's
//! native rate) and reports the sample offsets where a spoken segment ENDS — the
//! speech→silence transitions. It does NOT transcribe and it does NOT buffer the
//! audio: the caller keeps the full capture buffer and simply slices it at the
//! boundaries we hand back. That keeps this backend-agnostic (it works the same
//! whether the model behind it is the ONNX Parakeet, the macOS Core ML / ANE
//! Parakeet, or system STT) and loss-proof: because the caller still owns every
//! sample, a session where the detector never fires degrades to one whole-buffer
//! transcription — exactly the old behavior, never worse.
//!
//! Why this exists: dictation used to be transcribed as ONE buffer at stop (plus a
//! wasteful periodic re-pass of the WHOLE growing buffer for the live partial). On
//! a long utterance that final pass costs `rtf × duration` — the lag felt on the
//! second Caps tap. By cutting the utterance at natural pauses and transcribing
//! each segment WHILE the user keeps talking, only the short final segment is left
//! to transcribe at stop, so the submit feels instant. (transcribe-rs DOES expose
//! an incremental `Transcriber`/`VadChunked` API, but it borrows a `&mut dyn
//! SpeechModel` and so only fits the ONNX path; this detector reuses the same
//! [`transcribe_rs::vad`] smoothing for ALL backends and leaves inference to the
//! caller's existing per-segment pipeline — gain, resample, speaker-lock, trim.)
//!
//! The detector runs at the device rate (energy RMS is rate-independent; the frame
//! size is scaled to a fixed 30 ms), so the caller resamples each WHOLE segment to
//! 16 kHz once — no per-block resampling artifacts.

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
/// Hard cap on one segment (7 s). A pause-free monologue is force-split here so a
/// single transcription call (and the live-partial tail) stays bounded.
///
/// This MUST stay ≥ the live-partial tail re-pass budget in the dictation helper
/// (`tail_partial_max`), which is keyed off this value. The two used to diverge (8 s
/// preview cap vs 20 s split): a pause-free phrase between 8 s and 20 s grew a tail too
/// long to preview but too short to commit, so the overlay went BLANK until stop. Force-
/// splitting at 7 s keeps the open tail short enough to always preview, so committed text
/// lands every ~7 s during an unbroken monologue instead of nothing until you stop.
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
    /// to [`FRAME_MS`].
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
        let fps = 1000 / FRAME_MS;
        self.frame * fps * MAX_SEGMENT_SECS
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
                // Speech region just closed → boundary at the end of this frame.
                boundaries.push(self.pos);
                self.seg_start = self.pos;
            } else if now_speech && self.pos - self.seg_start >= max {
                // Pause-free monologue: force-split to bound transcription work.
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
        // Above SPEECH_RMS: a 0.2-amplitude tone-ish ramp.
        vec![0.2f32; frames * (RATE as usize * FRAME_MS / 1000)]
    }
    fn silence(frames: usize) -> Vec<f32> {
        vec![0.0f32; frames * (RATE as usize * FRAME_MS / 1000)]
    }

    #[test]
    fn no_boundary_until_speech_then_silence() {
        let mut d = VadBoundaryDetector::new(RATE);
        // Speech alone: onset fires but no closing boundary yet.
        assert!(d.feed(&speech(10)).is_empty());
        // Enough silence to exhaust the hangover → exactly one boundary.
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
        assert!(b[0] % frame == 0, "boundary is frame-aligned");
        assert!(b[0] <= fed, "boundary within the fed sample count");
    }

    #[test]
    fn long_monologue_force_splits() {
        let mut d = VadBoundaryDetector::new(RATE);
        // A pause-free run longer than MAX_SEGMENT_SECS must split.
        let fps = 1000 / FRAME_MS;
        let b = d.feed(&speech((MAX_SEGMENT_SECS + 3) * fps));
        assert!(!b.is_empty(), "force-split a pause-free monologue");
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
        let max = frame * fps * MAX_SEGMENT_SECS;
        assert_eq!(b[0], max, "first force-split must be exactly at MAX_SEGMENT_SECS");
        assert!(
            MAX_SEGMENT_SECS <= 8,
            "force-split must stay within the helper's live-partial tail budget (~8 s) \
             or the dictation overlay goes blank on long pause-free speech"
        );
    }

    #[test]
    fn sub_frame_blocks_reassemble_across_feeds() {
        let mut d = VadBoundaryDetector::new(RATE);
        let frame = RATE as usize * FRAME_MS / 1000;
        // Feed speech one odd-sized sliver at a time (smaller than a frame).
        let sp = speech(10);
        for chunk in sp.chunks(frame / 3 + 1) {
            d.feed(chunk);
        }
        // Then silence, likewise slivered, should still close exactly one segment.
        let si = silence(HANGOVER_FRAMES + 5);
        let mut total = 0;
        for chunk in si.chunks(frame / 3 + 1) {
            total += d.feed(chunk).len();
        }
        assert_eq!(total, 1, "boundary survives sub-frame fragmentation");
    }
}
