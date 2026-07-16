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

/// Once at start of `--serve` / one-shot in `main()` — whole process priority. Windows only.
pub(crate) fn elevate_process() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
        };
        // SAFETY: GetCurrentProcess is a pseudo-handle (no close); SetPriorityClass is self-only.
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
        }
    }
}

/// Start of each latency-critical thread (serve/one-shot + full-duplex listen).
/// Windows: MMCSS "Audio" (no revert — process ends via `_exit`, so leak is intentional).
/// macOS: QoS bump. Linux: no-op (see module doc).
pub(crate) fn elevate_current_thread() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
        let mut task_index: u32 = 0;
        // SAFETY: live stack out-param + static wide task name; MMCSS handle intentionally
        // leaked (process ends via `_exit`; revert would be dead code).
        unsafe {
            let _ = AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task_index);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: self-only QoS; QOS_CLASS_USER_INTERACTIVE + relative 0 is valid; no caller memory.
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
    // Linux only (CI): on win/mac these do unreverted real priority mutation.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn elevate_calls_are_a_noop_here() {
        super::elevate_process();
        super::elevate_current_thread();
    }
}
