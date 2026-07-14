//! Transactional PCM preparation shared by the one-shot and warm helper paths.
//!
//! Synthesis may span several model-bounded phoneme chunks. Playback of staged audio must not
//! begin until every chunk behind it succeeds: once samples reach an audio device they cannot
//! be retracted if a later chunk fails. Staging the WHOLE utterance behind one commit point was
//! the first shape here, but it turned any utterance longer than the staging cap into total
//! silence + ERR — queue admission accepts ~10 KiB of text, several minutes of speech, while
//! the cap allows 90 seconds. Utterances now stage into consecutive bounded groups: each group
//! is all-or-nothing, an utterance at or under the cap keeps the original single-transaction
//! guarantee, and a later failure discards only the staged-but-uncommitted group — it can never
//! leave a half-played group.

use std::time::Instant;

use ds_tts::SAMPLE_RATE;

/// Bound each staged group to the macOS VPIO render ring's 90-second capacity. Applying the
/// same cap on every backend prevents platform-dependent truncation and bounds staged mono f32
/// PCM to about 8.2 MiB per group before the 24→48 kHz VPIO resample. A single chunk whose PCM
/// alone exceeds the cap forms its own group rather than failing the utterance.
const MAX_PREPARED_SAMPLES: usize = SAMPLE_RATE as usize * 90;

/// One fully staged, committable group of PCM pieces.
pub(crate) struct PreparedAudio {
    pub(crate) pieces: Vec<Vec<f32>>,
    pub(crate) synth_nanos: u128,
    pub(crate) total_samples: usize,
}

pub(crate) enum PrepareOutcome {
    Cancelled,
    Finished,
}

/// Synthesize `batches` into bounded groups, handing each FULLY staged group to `commit`
/// (which plays or enqueues it). A synthesis failure or empty PCM discards the current
/// uncommitted group and returns `Err` without calling `commit` for it; a cancellation
/// (checked before, after, and between chunks) discards it and returns `Cancelled`.
pub(crate) fn prepare_audio<T>(
    batches: &[T],
    cancelled: impl Fn() -> bool,
    synthesize: impl FnMut(&T) -> Result<Vec<f32>, String>,
    commit: impl FnMut(PreparedAudio) -> Result<(), String>,
) -> Result<PrepareOutcome, String> {
    prepare_audio_with_limit(batches, cancelled, synthesize, commit, MAX_PREPARED_SAMPLES)
}

fn prepare_audio_with_limit<T>(
    batches: &[T],
    cancelled: impl Fn() -> bool,
    mut synthesize: impl FnMut(&T) -> Result<Vec<f32>, String>,
    mut commit: impl FnMut(PreparedAudio) -> Result<(), String>,
    max_samples: usize,
) -> Result<PrepareOutcome, String> {
    let mut pieces: Vec<Vec<f32>> = Vec::new();
    let mut synth_nanos = 0u128;
    let mut total_samples = 0usize;

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
        // Group boundary: this piece would overflow the staging cap, so commit the group staged
        // so far and start the next one with it.
        if total_samples > 0
            && !matches!(total_samples.checked_add(pcm.len()), Some(t) if t <= max_samples)
        {
            commit(PreparedAudio {
                pieces: std::mem::take(&mut pieces),
                synth_nanos,
                total_samples,
            })?;
            synth_nanos = 0;
            total_samples = 0;
        }
        synth_nanos = synth_nanos.saturating_add(elapsed);
        total_samples = total_samples.saturating_add(pcm.len());
        pieces.push(pcm);
    }

    if cancelled() {
        return Ok(PrepareOutcome::Cancelled);
    }
    if !pieces.is_empty() {
        commit(PreparedAudio {
            pieces,
            synth_nanos,
            total_samples,
        })?;
    }
    Ok(PrepareOutcome::Finished)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::{PrepareOutcome, PreparedAudio, prepare_audio_with_limit};

    /// A `commit` that records each committed group's piece layout.
    fn collect(
        groups: &RefCell<Vec<Vec<Vec<f32>>>>,
    ) -> impl FnMut(PreparedAudio) -> Result<(), String> {
        |audio| {
            groups.borrow_mut().push(audio.pieces);
            Ok(())
        }
    }

    #[test]
    fn later_failure_commits_no_partial_group() {
        let groups = RefCell::new(Vec::new());
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
            collect(&groups),
            10,
        )
        .err()
        .expect("the transaction must fail");

        assert_eq!(err, "second chunk failed");
        assert!(
            groups.borrow().is_empty(),
            "a failed group must never reach commit"
        );
    }

    #[test]
    fn successful_preparation_preserves_piece_order() {
        let groups = RefCell::new(Vec::new());
        let outcome = prepare_audio_with_limit(
            &[1, 2],
            || false,
            |batch| Ok(vec![*batch as f32]),
            collect(&groups),
            10,
        )
        .expect("preparation succeeds");
        assert!(matches!(outcome, PrepareOutcome::Finished));

        assert_eq!(*groups.borrow(), vec![vec![vec![1.0], vec![2.0]]]);
    }

    #[test]
    fn empty_pcm_from_any_chunk_is_a_transaction_failure() {
        let groups = RefCell::new(Vec::new());
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
            collect(&groups),
            10,
        )
        .err()
        .expect("empty output must fail");
        assert_eq!(err, "synthesis produced no audio for a phoneme chunk");
        assert_eq!(calls.get(), 2);
        assert!(groups.borrow().is_empty());
    }

    #[test]
    fn cancellation_discards_staged_pieces_before_commit() {
        let groups = RefCell::new(Vec::new());
        let calls = Cell::new(0usize);
        let outcome = prepare_audio_with_limit(
            &[1, 2, 3],
            || calls.get() == 1,
            |_| {
                calls.set(calls.get() + 1);
                Ok(vec![1.0])
            },
            collect(&groups),
            10,
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
        assert_eq!(calls.get(), 1);
        assert!(groups.borrow().is_empty());
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
                |_| Ok(()),
                10,
            )
            .expect("cancellation is not a synthesis error");
            assert!(matches!(outcome, PrepareOutcome::Cancelled));
        }
    }

    /// Regression (audit F02): an utterance longer than the staging cap used to fail outright —
    /// fully synthesized, then discarded as one over-limit transaction, so a long accepted
    /// queue item produced total silence plus ERR. It must instead split into bounded groups
    /// that each commit whole.
    #[test]
    fn over_cap_utterance_splits_into_bounded_groups_instead_of_failing() {
        let groups = RefCell::new(Vec::new());
        let outcome = prepare_audio_with_limit(
            &[1, 2, 3],
            || false,
            |_| Ok(vec![1.0, 2.0]),
            collect(&groups),
            3,
        )
        .expect("an over-cap utterance still plays");
        assert!(matches!(outcome, PrepareOutcome::Finished));

        let committed = groups.borrow();
        assert_eq!(
            committed.len(),
            3,
            "each 2-sample piece fills a 3-sample group"
        );
        assert!(committed.iter().all(|group| group.len() == 1));
    }

    #[test]
    fn oversized_single_piece_forms_its_own_group() {
        let groups = RefCell::new(Vec::new());
        prepare_audio_with_limit(
            &[5usize, 1],
            || false,
            |samples| Ok(vec![0.0; *samples]),
            collect(&groups),
            3,
        )
        .expect("a single over-cap piece cannot be split, so it commits alone");

        let committed = groups.borrow();
        assert_eq!(committed.len(), 2);
        assert_eq!(committed[0][0].len(), 5);
        assert_eq!(committed[1][0].len(), 1);
    }

    /// The grouped-transaction guarantee: a failure discards only the group staged since the
    /// last commit — already-committed groups have played, uncommitted pieces never do.
    #[test]
    fn later_group_failure_loses_only_the_uncommitted_group() {
        let groups = RefCell::new(Vec::new());
        let calls = Cell::new(0usize);
        let err = prepare_audio_with_limit(
            &[2usize, 2, 2],
            || false,
            |samples| {
                calls.set(calls.get() + 1);
                if calls.get() == 3 {
                    Err("third chunk failed".to_string())
                } else {
                    Ok(vec![0.0; *samples])
                }
            },
            collect(&groups),
            2,
        )
        .err()
        .expect("the failing group must fail");

        assert_eq!(err, "third chunk failed");
        assert_eq!(
            groups.borrow().len(),
            1,
            "only the first, complete group was committed before the failure"
        );
    }

    #[test]
    fn commit_failure_aborts_preparation() {
        let calls = Cell::new(0usize);
        let err = prepare_audio_with_limit(
            &[2usize, 2, 2],
            || false,
            |samples| {
                calls.set(calls.get() + 1);
                Ok(vec![0.0; *samples])
            },
            |_| Err("sink failed".to_string()),
            2,
        )
        .err()
        .expect("a commit failure is terminal");

        assert_eq!(err, "sink failed");
        assert_eq!(calls.get(), 2, "no further synthesis after a failed commit");
    }

    #[test]
    fn cancellation_between_groups_stops_before_the_next_chunk() {
        let groups = RefCell::new(Vec::new());
        let outcome = prepare_audio_with_limit(
            &[2usize, 2, 2],
            || !groups.borrow().is_empty(),
            |samples| Ok(vec![0.0; *samples]),
            collect(&groups),
            2,
        )
        .expect("cancellation is not a synthesis error");

        assert!(matches!(outcome, PrepareOutcome::Cancelled));
        assert_eq!(
            groups.borrow().len(),
            1,
            "the committed group stands; nothing further stages"
        );
    }
}
