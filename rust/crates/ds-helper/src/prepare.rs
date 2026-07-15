//! Low-latency transactional PCM preparation shared by the one-shot and warm helper paths.
//!
//! The shared frontend already splits an utterance into model-bounded, sentence-aware phoneme
//! batches. Each batch is the transaction boundary here: synthesize and validate its complete
//! PCM before committing it to playback. Committing the first batch immediately lets playback
//! overlap synthesis of the remaining batches instead of making time-to-first-audio grow with
//! the whole utterance. If a later batch fails, its uncommitted PCM stays silent and the caller
//! stops any already-committed prefix before reporting the error.

use std::time::Instant;

/// One fully synthesized and validated phoneme batch, ready to commit to playback.
pub(crate) struct PreparedAudio {
    pub(crate) pcm: Vec<f32>,
    pub(crate) synth_nanos: u128,
}

pub(crate) enum PrepareOutcome {
    Cancelled,
    Finished,
}

/// Synthesize and commit each FULL phoneme batch before starting the next one. A synthesis
/// failure or empty PCM discards the current uncommitted batch and returns `Err` without
/// calling `commit` for it. Cancellation is checked before and after inference, between
/// batches, and after the final commit.
pub(crate) fn prepare_audio<T>(
    batches: &[T],
    cancelled: impl Fn() -> bool,
    mut synthesize: impl FnMut(&T) -> Result<Vec<f32>, String>,
    mut commit: impl FnMut(PreparedAudio) -> Result<(), String>,
) -> Result<PrepareOutcome, String> {
    for batch in batches {
        if cancelled() {
            return Ok(PrepareOutcome::Cancelled);
        }
        let started = Instant::now();
        let result = synthesize(batch);
        let elapsed = started.elapsed().as_nanos();
        // A barge that landed during inference owns the terminal outcome. Discard either PCM or
        // an incidental backend error so a cancelled request reports DONE, never a misleading ERR.
        if cancelled() {
            return Ok(PrepareOutcome::Cancelled);
        }
        let pcm = result?;
        if pcm.is_empty() {
            return Err("synthesis produced no audio for a phoneme chunk".to_string());
        }
        commit(PreparedAudio {
            pcm,
            synth_nanos: elapsed,
        })?;
    }

    Ok(if cancelled() {
        PrepareOutcome::Cancelled
    } else {
        PrepareOutcome::Finished
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::{PrepareOutcome, PreparedAudio, prepare_audio};

    /// A `commit` that records each committed batch.
    fn collect(groups: &RefCell<Vec<Vec<f32>>>) -> impl FnMut(PreparedAudio) -> Result<(), String> {
        |audio| {
            groups.borrow_mut().push(audio.pcm);
            Ok(())
        }
    }

    #[test]
    fn failure_before_first_commit_is_silent() {
        let groups = RefCell::new(Vec::new());
        let err = prepare_audio(
            &[1, 2],
            || false,
            |_| Err("first chunk failed".to_string()),
            collect(&groups),
        )
        .err()
        .expect("the transaction must fail");

        assert_eq!(err, "first chunk failed");
        assert!(groups.borrow().is_empty());
    }

    #[test]
    fn successful_preparation_commits_each_batch_in_order() {
        let groups = RefCell::new(Vec::new());
        let outcome = prepare_audio(
            &[1, 2],
            || false,
            |batch| Ok(vec![*batch as f32]),
            collect(&groups),
        )
        .expect("preparation succeeds");
        assert!(matches!(outcome, PrepareOutcome::Finished));

        assert_eq!(*groups.borrow(), vec![vec![1.0], vec![2.0]]);
    }

    /// Regression (#82): the first completed phoneme batch must become playable before
    /// inference for the rest of a long blockquote-less reply begins.
    #[test]
    fn first_batch_commits_before_later_synthesis() {
        let groups = RefCell::new(Vec::new());
        let outcome = prepare_audio(
            &[1, 2],
            || false,
            |batch| {
                if *batch == 2 {
                    assert_eq!(
                        groups.borrow().len(),
                        1,
                        "the first batch must be committed before the second is synthesized"
                    );
                }
                Ok(vec![*batch as f32])
            },
            collect(&groups),
        )
        .expect("preparation succeeds");

        assert!(matches!(outcome, PrepareOutcome::Finished));
        assert_eq!(groups.borrow().len(), 2);
    }

    #[test]
    fn empty_pcm_discards_only_the_failing_batch() {
        let groups = RefCell::new(Vec::new());
        let calls = Cell::new(0usize);
        let err = prepare_audio(
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
            collect(&groups),
        )
        .err()
        .expect("empty output must fail");
        assert_eq!(err, "synthesis produced no audio for a phoneme chunk");
        assert_eq!(calls.get(), 2);
        assert_eq!(groups.borrow().len(), 1);
    }

    #[test]
    fn cancellation_before_inference_is_silent() {
        let groups = RefCell::new(Vec::new());
        let calls = Cell::new(0usize);
        let outcome = prepare_audio(
            &[1, 2, 3],
            || true,
            |_| {
                calls.set(calls.get() + 1);
                Ok(vec![1.0])
            },
            collect(&groups),
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
        assert_eq!(calls.get(), 0);
        assert!(groups.borrow().is_empty());
    }

    #[test]
    fn cancellation_during_inference_wins_over_pcm_or_backend_error() {
        for result in [Ok(vec![1.0]), Err("incidental failure".to_string())] {
            let finished = Cell::new(false);
            let outcome = prepare_audio(
                &[1],
                || finished.get(),
                |_| {
                    finished.set(true);
                    result.clone()
                },
                |_| Ok(()),
            )
            .expect("cancellation is not a synthesis error");
            assert!(matches!(outcome, PrepareOutcome::Cancelled));
        }
    }

    #[test]
    fn later_failure_preserves_only_the_committed_prefix() {
        let groups = RefCell::new(Vec::new());
        let err = prepare_audio(
            &[1, 2, 3],
            || false,
            |batch| {
                if *batch == 3 {
                    Err("third chunk failed".to_string())
                } else {
                    Ok(vec![*batch as f32])
                }
            },
            collect(&groups),
        )
        .err()
        .expect("the failing batch must fail");

        assert_eq!(err, "third chunk failed");
        assert_eq!(groups.borrow().len(), 2);
    }

    #[test]
    fn commit_failure_aborts_preparation() {
        let calls = Cell::new(0usize);
        let err = prepare_audio(
            &[2usize, 2, 2],
            || false,
            |samples| {
                calls.set(calls.get() + 1);
                Ok(vec![0.0; *samples])
            },
            |_| Err("sink failed".to_string()),
        )
        .err()
        .expect("a commit failure is terminal");

        assert_eq!(err, "sink failed");
        assert_eq!(calls.get(), 1, "no further synthesis after a failed commit");
    }

    #[test]
    fn cancellation_between_batches_stops_before_the_next_inference() {
        let groups = RefCell::new(Vec::new());
        let calls = Cell::new(0usize);
        let outcome = prepare_audio(
            &[2usize, 2, 2],
            || !groups.borrow().is_empty(),
            |samples| {
                calls.set(calls.get() + 1);
                Ok(vec![0.0; *samples])
            },
            collect(&groups),
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
        assert_eq!(calls.get(), 1);
        assert_eq!(groups.borrow().len(), 1);
    }

    #[test]
    fn cancellation_after_final_commit_reports_cancelled() {
        let committed = Cell::new(false);
        let outcome = prepare_audio(
            &[1],
            || committed.get(),
            |_| Ok(vec![1.0]),
            |_| {
                committed.set(true);
                Ok(())
            },
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
    }
}
