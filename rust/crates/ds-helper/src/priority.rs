//! Best-effort OS scheduling boost for ds-helper's own latency-critical threads
//! (Kokoro synth/decode, Parakeet capture+inference) — NOT the cpal-owned audio
//! render/capture callback thread, which this doesn't reach (cpal's own
//! `realtime`/`realtime-dbus` features cover that thread, but published rodio
//! (0.22.x) still pins cpal 0.17, which predates them — revisit once rodio ships
//! a cpal>=0.18-based release).
//!
//! Self-only, no cross-process reach, no privilege/entitlement needed on any OS.
//! Deliberately conservative: ABOVE_NORMAL (not HIGH/REALTIME) on Windows, the
//! "Audio" MMCSS category (not "Pro Audio"), QOS_CLASS_USER_INTERACTIVE (not a
//! hard real-time thread policy) on macOS. Linux is intentionally left at default
//! niceness/scheduling class — raising it needs CAP_SYS_NICE/RLIMIT_NICE this
//! unprivileged desktop app doesn't have, and the correct no-root path (rtkit) is
//! designed for a genuine periodic audio callback, not this variable-duration
//! synth/inference loop, so there is nothing safe to add here yet.

/// Call ONCE, as the first thing in the `--serve` / one-shot synth paths of
/// `main()` — raises the WHOLE process's priority class. Windows only; no-op
/// elsewhere.
pub(crate) fn elevate_process() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
        };
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
        }
    }
}

/// Call at the START of every latency-critical thread (the serve-loop/one-shot
/// thread via `main()`, and the full-duplex concurrent-listen thread). Windows:
/// registers this thread with MMCSS under the "Audio" task category — the same
/// mechanism the Windows audio stack itself uses, no revert (the OS reclaims the
/// registration when the PROCESS terminates — normally via `_exit()`, matching
/// this crate's existing teardown convention; see main.rs's top comment — so
/// leaving it unreverted per-thread is safe even on the one auxiliary thread that
/// exits by a plain `return` rather than `_exit()` itself). macOS: bumps this
/// thread's QoS class. Linux: no-op (see module doc).
pub(crate) fn elevate_current_thread() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
        let mut task_index: u32 = 0;
        unsafe {
            // Leaked HANDLE is intentional: this thread lives for the process's
            // lifetime, which ends via `_exit()` (skips destructors) — reverting
            // would be dead code, not a fix.
            let _ = AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task_index);
        }
    }
    #[cfg(target_os = "macos")]
    {
        unsafe {
            let _ = libc::pthread_set_qos_class_self_np(
                libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    // Only exercised where these are genuine no-ops (per-commit CI is Linux-only).
    // NOT run on windows/macos: elevate_process/elevate_current_thread do REAL,
    // unreverted OS priority/QoS mutation there (by design — see module doc), so
    // running them against the live `cargo test` process/thread would leave that
    // process permanently boosted with no seam to undo it.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn elevate_calls_are_a_noop_here() {
        super::elevate_process();
        super::elevate_current_thread();
    }
}
