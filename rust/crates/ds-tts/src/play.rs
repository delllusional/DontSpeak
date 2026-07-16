//! PCM playback — 24 kHz mono f32.
//!
//! macOS: temp WAV + `afplay`. rodio/cpal CoreAudio aborts on teardown on macOS 26
//! ("mutex lock failed"), so one-shot avoids cpal. Warm helper uses incremental path.
//!
//! Elsewhere: [`crate::sink::IncrementalSink`] (non-blocking enqueue; rodio drains).
//! Leading silence on drained start = onset-clip fix shared with warm serve.
//!
//! NO-AUDIO: device open / afplay are side effects — not unit-tested. Pure pipeline
//! tested in vocab/voices/trim/batch; sink clock/accounting device-free in `sink`.

pub use imp::AudioPlayer;

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use crate::vocab::SAMPLE_RATE;

    /// Accumulate PCM; play once via `afplay` on `wait()`.
    pub struct AudioPlayer {
        samples: RefCell<Vec<f32>>,
    }

    impl AudioPlayer {
        /// afplay owns playback — never fails.
        pub fn open() -> Result<Self, String> {
            Ok(Self {
                samples: RefCell::new(Vec::new()),
            })
        }

        pub fn enqueue(&self, mut samples: Vec<f32>) {
            if !samples.is_empty() {
                self.samples.borrow_mut().append(&mut samples);
            }
        }

        /// Temp WAV + block on afplay. Fail-quiet.
        pub fn wait(&self) {
            let samples = self.samples.borrow();
            if samples.is_empty() {
                return;
            }
            let path = std::env::temp_dir().join(format!("ds-{}.wav", std::process::id()));
            if crate::wav::write_wav16(&path, &samples, SAMPLE_RATE).is_err() {
                return;
            }
            let _ = std::process::Command::new("afplay").arg(&path).status();
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::cell::RefCell;

    use crate::sink::IncrementalSink;

    /// Shared [`IncrementalSink`]. Drop closes stream. `RefCell` keeps `enqueue(&self)`
    /// while sink needs `&mut` (single-threaded one-shot).
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
