//! A single non-blocking "wait N ms, then fire" primitive, shared by the Caps-Lock
//! and hands-free submit paths' deferred post-paste Enter. Both live on the
//! engine's single poll thread (`Engine::tick` / `Listener::tick`), so the delay
//! must be polled once per tick — like the engine's existing `pending_tap_at` /
//! long-press timers — rather than blocking that thread with `std::thread::sleep`.

use std::time::{Duration, Instant};

/// True once `delay_ms` has elapsed since `pending` was armed, clearing it so the
/// caller fires its deferred action exactly once. `false` (with `pending` left
/// untouched) while still waiting or if nothing is armed.
pub(crate) fn deferred_ready(pending: &mut Option<Instant>, delay_ms: u64) -> bool {
    match *pending {
        Some(since) if since.elapsed() >= Duration::from_millis(delay_ms) => {
            *pending = None;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_ready_before_the_delay_elapses() {
        let mut pending = Some(Instant::now());
        assert!(!deferred_ready(&mut pending, 10_000));
        assert!(pending.is_some());
    }

    #[test]
    fn ready_once_the_delay_has_elapsed_and_clears_itself() {
        let mut pending = Some(Instant::now() - Duration::from_millis(50));
        assert!(deferred_ready(&mut pending, 10));
        assert!(pending.is_none());
        // A second poll finds nothing armed.
        assert!(!deferred_ready(&mut pending, 10));
    }

    #[test]
    fn none_is_never_ready() {
        let mut pending = None;
        assert!(!deferred_ready(&mut pending, 0));
    }
}
