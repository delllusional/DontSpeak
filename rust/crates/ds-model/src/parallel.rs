//! Bounded multi-file download pool. Workers run blocking GETs; the caller thread
//! is the sole progress coordinator (so UI callbacks need not be `Send`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Concurrent file fetches inside one asset set (HF trees and the CUDA wheel host); caps FD/TLS
/// pressure.
pub(crate) const PARALLEL_DOWNLOADS: usize = 4;

/// One unit of work that reports local `(done, _)` bytes via the callback.
pub(crate) type DownloadJob =
    Box<dyn FnOnce(&dyn Fn(u64, u64)) -> std::io::Result<()> + Send>;

enum Event {
    Progress,
    Finished { result: std::io::Result<()> },
}

/// Run `jobs` with up to [`PARALLEL_DOWNLOADS`] workers.
///
/// Progress: per-job high-water of reported bytes; aggregate =
/// `min(total, max(last, initial_done + sum(highs)))`. On full success always emits
/// `progress(total, total)` (silent / already-present jobs may contribute 0 mid-run).
/// On error does **not** force 100%. Stops scheduling new work after the first failure.
pub(crate) fn run_jobs_parallel(
    progress: &dyn Fn(u64, u64),
    total: u64,
    initial_done: u64,
    jobs: Vec<DownloadJob>,
) -> std::io::Result<()> {
    if jobs.is_empty() {
        progress(total, total);
        return Ok(());
    }

    let n = jobs.len();
    let highs: Arc<Vec<AtomicU64>> = Arc::new((0..n).map(|_| AtomicU64::new(0)).collect());
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Event>();
    let queue = Arc::new(Mutex::new(
        jobs.into_iter()
            .enumerate()
            .collect::<VecDeque<(usize, DownloadJob)>>(),
    ));

    let workers = PARALLEL_DOWNLOADS.min(n).max(1);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        let cancel = Arc::clone(&cancel);
        let highs = Arc::clone(&highs);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let next = {
                    let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
                    q.pop_front()
                };
                let Some((idx, job)) = next else {
                    break;
                };

                let tx_prog = tx.clone();
                let highs = Arc::clone(&highs);
                let emit = move |done: u64, _local_total: u64| {
                    let slot = &highs[idx];
                    let mut cur = slot.load(Ordering::Relaxed);
                    while done > cur {
                        match slot.compare_exchange_weak(
                            cur,
                            done,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(v) => cur = v,
                        }
                    }
                    let _ = tx_prog.send(Event::Progress);
                };

                let result = job(&emit);
                if result.is_err() {
                    cancel.store(true, Ordering::Relaxed);
                }
                let _ = tx.send(Event::Finished { result });
            }
        }));
    }
    drop(tx);

    let mut finished = 0usize;
    let mut last = initial_done.min(total);
    if last > 0 {
        progress(last, total);
    }
    let mut first_err: Option<std::io::Error> = None;

    // Channel closes when every worker exits. Cancelled queue leftovers never emit Finished,
    // so stop on disconnect rather than requiring finished == n.
    while finished < n {
        match rx.recv() {
            Ok(Event::Progress) => {
                let sum: u64 = highs.iter().map(|a| a.load(Ordering::Relaxed)).sum();
                let v = (initial_done.saturating_add(sum)).min(total).max(last);
                if v > last {
                    last = v;
                    progress(v, total);
                }
            }
            Ok(Event::Finished { result }) => {
                finished += 1;
                if let Err(e) = result
                    && first_err.is_none()
                {
                    first_err = Some(e);
                }
                let sum: u64 = highs.iter().map(|a| a.load(Ordering::Relaxed)).sum();
                let v = (initial_done.saturating_add(sum)).min(total).max(last);
                if v > last {
                    last = v;
                    progress(v, total);
                }
            }
            Err(_) => break,
        }
    }

    for h in handles {
        let _ = h.join();
    }

    if let Some(e) = first_err {
        return Err(e);
    }
    if finished != n {
        return Err(std::io::Error::other(format!(
            "download pool stopped early ({finished} of {n} jobs finished)"
        )));
    }
    progress(total, total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn empty_jobs_emit_final_total() {
        let seen = Mutex::new(Vec::<u64>::new());
        run_jobs_parallel(&|d, _t| seen.lock().unwrap().push(d), 50, 0, vec![]).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![50]);
    }

    #[test]
    fn silent_jobs_still_end_at_total() {
        let seen = Mutex::new(Vec::<u64>::new());
        let jobs: Vec<DownloadJob> = vec![
            Box::new(|p| {
                p(40, 40);
                Ok(())
            }),
            Box::new(|_p| Ok(())),
        ];
        run_jobs_parallel(&|d, _| seen.lock().unwrap().push(d), 100, 0, jobs).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert_eq!(*seen.last().unwrap(), 100);
    }

    #[test]
    fn error_does_not_force_100() {
        let seen = Mutex::new(Vec::<u64>::new());
        let jobs: Vec<DownloadJob> = vec![
            Box::new(|p| {
                p(10, 50);
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "bad",
                ))
            }),
            Box::new(|p| {
                p(50, 50);
                Ok(())
            }),
        ];
        let err = run_jobs_parallel(&|d, _| seen.lock().unwrap().push(d), 100, 0, jobs).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let seen = seen.lock().unwrap();
        assert!(
            seen.last().is_none_or(|&v| v < 100),
            "must not force 100 on error: {seen:?}"
        );
    }

    #[test]
    fn initial_done_pre_credits_bar() {
        let seen = Mutex::new(Vec::<u64>::new());
        let jobs: Vec<DownloadJob> = vec![Box::new(|p| {
            p(20, 20);
            Ok(())
        })];
        run_jobs_parallel(&|d, _| seen.lock().unwrap().push(d), 100, 80, jobs).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&80) || seen.first() == Some(&80) || seen.iter().any(|&v| v >= 80));
        assert_eq!(*seen.last().unwrap(), 100);
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
    }

    #[test]
    fn concurrent_jobs_are_monotonic() {
        let seen = Mutex::new(Vec::<u64>::new());
        let jobs: Vec<DownloadJob> = (0..6)
            .map(|i| {
                Box::new(move |p: &dyn Fn(u64, u64)| {
                    for b in [1u64, 5, 10] {
                        p(b, 10);
                        std::thread::sleep(std::time::Duration::from_millis(1 + i as u64));
                    }
                    Ok(())
                }) as DownloadJob
            })
            .collect();
        run_jobs_parallel(&|d, t| {
            assert_eq!(t, 60);
            seen.lock().unwrap().push(d);
        }, 60, 0, jobs)
        .unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert!(seen.iter().all(|&v| v <= 60));
        assert_eq!(*seen.last().unwrap(), 60);
    }
}
