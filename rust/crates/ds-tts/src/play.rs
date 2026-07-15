//! PCM playback — 24 kHz mono f32.
//!
//! macOS: render the accumulated samples to a temp WAV and play via the system
//! `afplay`. `rodio`/`cpal`'s CoreAudio backend aborts on teardown on macOS 26
//! ("mutex lock failed: Invalid argument"), so for this short-lived one-shot
//! helper we avoid cpal entirely — `afplay` is blocking, reliable, and has no
//! teardown crash. The one-shot player accumulates committed batches before `wait`;
//! the default warm helper uses its persistent output path for incremental playback.
//!
//! Other platforms: play the prepared batches through the shared incremental
//! [`crate::sink::IncrementalSink`] (enqueue is non-blocking; rodio's audio thread
//! plays them continuously and `wait` drains them). The sink prepends its short
//! leading silence on a drained start, so the one-shot rodio path shares the warm
//! serve loop's onset-clip fix.
//!
//! NO-AUDIO DISCIPLINE: opening a device / spawning afplay is a real side effect,
//! so NOTHING here is exercised by unit tests. The ds-helper helper bin is
//! the only constructor; the pure pipeline is tested in vocab/voices/trim/batch,
//! and the sink's clock/accounting is tested device-free in `crate::sink`.

pub use imp::AudioPlayer;

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use crate::vocab::SAMPLE_RATE;

    /// Accumulates synthesized PCM, then plays it once via `afplay` on `wait()`.
    pub struct AudioPlayer {
        samples: RefCell<Vec<f32>>,
    }

    impl AudioPlayer {
        /// No device is opened up front (afplay owns playback) — never fails.
        pub fn open() -> Result<Self, String> {
            Ok(Self {
                samples: RefCell::new(Vec::new()),
            })
        }

        /// Append one batch of 24 kHz mono f32 samples (played in order on `wait`).
        pub fn enqueue(&self, mut samples: Vec<f32>) {
            if !samples.is_empty() {
                self.samples.borrow_mut().append(&mut samples);
            }
        }

        /// Render the accumulated samples to a temp WAV and block on `afplay`
        /// until playback finishes. Fail-quiet (degrade to silence on any error).
        pub fn wait(&self) {
            let samples = self.samples.borrow();
            if samples.is_empty() {
                return;
            }
            let path = std::env::temp_dir().join(format!("ds-{}.wav", std::process::id()));
            if crate::wav::write_wav16(&path, &samples, SAMPLE_RATE).is_err() {
                return;
            }
            // afplay blocks until the file finishes playing.
            let _ = std::process::Command::new("afplay").arg(&path).status();
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::cell::RefCell;

    use crate::sink::IncrementalSink;

    /// Owns the open output device through the shared [`IncrementalSink`]. Dropping it
    /// closes the stream. `RefCell` keeps the historical `enqueue(&self)` signature
    /// while the sink's append accounting needs `&mut` (single-threaded one-shot use).
    pub struct AudioPlayer {
        sink: RefCell<IncrementalSink>,
    }

    impl AudioPlayer {
        pub fn open() -> Result<Self, String> {
            Ok(Self {
                sink: RefCell::new(IncrementalSink::open_default()?),
            })
        }

        pub fn enqueue(&self, samples: Vec<f32>) {
            self.sink.borrow_mut().append(samples);
        }

        pub fn wait(&self) {
            self.sink.borrow().wait();
        }
    }
}
