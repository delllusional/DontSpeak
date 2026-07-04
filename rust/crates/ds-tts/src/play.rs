//! PCM playback — 24 kHz mono f32.
//!
//! macOS: render the accumulated samples to a temp WAV and play via the system
//! `afplay`. `rodio`/`cpal`'s CoreAudio backend aborts on teardown on macOS 26
//! ("mutex lock failed: Invalid argument"), so for this short-lived one-shot
//! helper we avoid cpal entirely — `afplay` is blocking, reliable, and has no
//! teardown crash. (Trade-off: we synth all groups then play, rather than
//! streaming group N+1 while N plays. Fine for a fire-and-forget reply.)
//!
//! Other platforms: stream via `rodio` (enqueue is non-blocking; the queue plays
//! on rodio's own audio thread; `wait` drains it).
//!
//! NO-AUDIO DISCIPLINE: opening a device / spawning afplay is a real side effect,
//! so NOTHING here is exercised by unit tests. The ds-helper helper bin is
//! the only constructor; the pure pipeline is tested in vocab/voices/trim/batch.

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

        /// Append one group of 24 kHz mono f32 samples (played in order on `wait`).
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
    use std::num::NonZero;

    use rodio::buffer::SamplesBuffer;
    use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

    use crate::vocab::SAMPLE_RATE;

    /// Owns the open output device + a rodio `Player`. Dropping it closes the stream.
    pub struct AudioPlayer {
        // `player` must drop before `_device` — declare it first.
        player: Player,
        _device: MixerDeviceSink,
    }

    impl AudioPlayer {
        pub fn open() -> Result<Self, String> {
            let device = DeviceSinkBuilder::open_default_sink()
                .map_err(|e| format!("open audio output: {e}"))?;
            let player = Player::connect_new(device.mixer());
            Ok(Self {
                player,
                _device: device,
            })
        }

        pub fn enqueue(&self, samples: Vec<f32>) {
            if samples.is_empty() {
                return;
            }
            let channels = NonZero::new(1u16).expect("1 channel");
            let rate = NonZero::new(SAMPLE_RATE).expect("24000 sample rate");
            self.player
                .append(SamplesBuffer::new(channels, rate, samples));
        }

        pub fn wait(&self) {
            self.player.sleep_until_end();
        }
    }
}
