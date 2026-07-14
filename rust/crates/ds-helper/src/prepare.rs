//! Transactional PCM preparation shared by the one-shot and warm helper paths.
//!
//! Synthesis may span several model-bounded phoneme chunks. Playback must not begin until every
//! chunk succeeds: once samples reach an audio device they cannot be retracted if a later chunk
//! fails. This module stages the complete utterance behind one bounded, testable commit point.

use std::time::Instant;

use ds_tts::SAMPLE_RATE;

/// Bound transactional staging to the macOS VPIO render ring's 90-second capacity. Applying the
/// same cap on every backend prevents platform-dependent truncation and bounds staged mono f32
/// PCM to about 8.2 MiB before the 24→48 kHz VPIO resample.
const MAX_PREPARED_SAMPLES: usize = SAMPLE_RATE as usize * 90;

pub(crate) struct PreparedAudio {
    pub(crate) pieces: Vec<Vec<f32>>,
    pub(crate) synth_nanos: u128,
    pub(crate) total_samples: usize,
}

pub(crate) enum PrepareOutcome {
    Cancelled,
    Ready(PreparedAudio),
}

pub(crate) fn prepare_audio<T>(
    batches: &[T],
    cancelled: impl Fn() -> bool,
    synthesize: impl FnMut(&T) -> Result<Vec<f32>, String>,
) -> Result<PrepareOutcome, String> {
    prepare_audio_with_limit(batches, cancelled, synthesize, MAX_PREPARED_SAMPLES)
}

fn prepare_audio_with_limit<T>(
    batches: &[T],
    cancelled: impl Fn() -> bool,
    mut synthesize: impl FnMut(&T) -> Result<Vec<f32>, String>,
    max_samples: usize,
) -> Result<PrepareOutcome, String> {
    let mut pieces = Vec::with_capacity(batches.len());
    let mut synth_nanos = 0u128;
    let mut total_samples = 0usize;

    for batch in batches {
        if cancelled() {
            return Ok(PrepareOutcome::Cancelled);
        }
        let started = Instant::now();
        let result = synthesize(batch);
        synth_nanos = synth_nanos.saturating_add(started.elapsed().as_nanos());
        // A barge that landed during inference owns the terminal outcome. Discard either PCM or
        // an incidental backend error so a cancelled request reports DONE, never a misleading ERR.
        if cancelled() {
            return Ok(PrepareOutcome::Cancelled);
        }
        let pcm = result?;
        if pcm.is_empty() {
            return Err("synthesis produced no audio for a phoneme chunk".to_string());
        }
        total_samples = total_samples
            .checked_add(pcm.len())
            .filter(|total| *total <= max_samples)
            .ok_or_else(|| {
                format!(
                    "synthesis output exceeds the {}-sample transactional limit",
                    max_samples
                )
            })?;
        pieces.push(pcm);
    }

    if cancelled() {
        return Ok(PrepareOutcome::Cancelled);
    }
    Ok(PrepareOutcome::Ready(PreparedAudio {
        pieces,
        synth_nanos,
        total_samples,
    }))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{PrepareOutcome, prepare_audio_with_limit};

    #[test]
    fn later_failure_returns_no_committable_partial_audio() {
        let err = prepare_audio_with_limit(
            &[1, 2, 3],
            || false,
            |batch| {
                if *batch == 2 {
                    Err("second chunk failed".to_string())
                } else {
                    Ok(vec![*batch as f32])
                }
            },
            10,
        )
        .err()
        .expect("the transaction must fail");

        assert_eq!(err, "second chunk failed");
    }

    #[test]
    fn successful_preparation_preserves_piece_order() {
        let outcome =
            prepare_audio_with_limit(&[1, 2], || false, |batch| Ok(vec![*batch as f32]), 10)
                .expect("preparation succeeds");
        let PrepareOutcome::Ready(audio) = outcome else {
            panic!("preparation was unexpectedly cancelled");
        };

        assert_eq!(audio.pieces, vec![vec![1.0], vec![2.0]]);
        assert_eq!(audio.total_samples, 2);
    }

    #[test]
    fn empty_pcm_from_any_chunk_is_a_transaction_failure() {
        let calls = Cell::new(0usize);
        let err = prepare_audio_with_limit(
            &[1, 2, 3],
            || false,
            |_| {
                calls.set(calls.get() + 1);
                Ok(if calls.get() == 2 {
                    Vec::new()
                } else {
                    vec![1.0]
                })
            },
            10,
        )
        .err()
        .expect("empty output must fail");
        assert_eq!(err, "synthesis produced no audio for a phoneme chunk");
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn cancellation_discards_staged_pieces_before_commit() {
        let calls = Cell::new(0usize);
        let outcome = prepare_audio_with_limit(
            &[1, 2, 3],
            || calls.get() == 1,
            |_| {
                calls.set(calls.get() + 1);
                Ok(vec![1.0])
            },
            10,
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn cancellation_during_inference_wins_over_pcm_or_backend_error() {
        for result in [Ok(vec![1.0]), Err("incidental failure".to_string())] {
            let finished = Cell::new(false);
            let outcome = prepare_audio_with_limit(
                &[1],
                || finished.get(),
                |_| {
                    finished.set(true);
                    result.clone()
                },
                10,
            )
            .expect("cancellation is not a synthesis error");
            assert!(matches!(outcome, PrepareOutcome::Cancelled));
        }
    }

    #[test]
    fn transactional_buffer_has_a_hard_sample_limit() {
        let exact = prepare_audio_with_limit(&[1, 2], || false, |_| Ok(vec![1.0, 2.0]), 4)
            .expect("the exact sample cap is valid");
        let PrepareOutcome::Ready(exact) = exact else {
            panic!("preparation was unexpectedly cancelled");
        };
        assert_eq!(exact.total_samples, 4);

        let err = prepare_audio_with_limit(&[1, 2], || false, |_| Ok(vec![1.0, 2.0]), 3)
            .err()
            .expect("the sample cap must reject the second piece");
        assert!(err.contains("3-sample transactional limit"));
    }
}
