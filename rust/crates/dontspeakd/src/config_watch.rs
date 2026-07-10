//! Push-based settings.json watch. Replaces the per-tick `stat()` poll with a native
//! filesystem watcher — FSEvents on macOS, inotify on Linux, ReadDirectoryChangesW on
//! Windows (selected per-OS by the `notify` crate) — that flips `reload_requested` the
//! instant the config changes. The boot loop keeps a COARSE `stat()` backstop (see
//! `MTIME_CHECK_INTERVAL`) for the rare case the watcher can't start or a filesystem
//! drops an event — a native watch and a stat fallback COMPLEMENT each other rather
//! than one replacing the other.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Spawn a filesystem watcher that sets `reload_requested` whenever `config_path` is
/// created / modified / removed / renamed. Returns the live watcher handle — the caller
/// MUST keep it alive (dropping it stops the watch). `None` if the watcher can't start,
/// in which case the boot loop's `stat()` backstop is the sole reload trigger.
///
/// We watch the PARENT DIRECTORY, not the file itself: settings.json is written
/// atomically (temp + rename), which replaces the inode, so a watch bound to the original
/// file would go deaf after the first save. Watching the dir and filtering by file name
/// survives the rename. Coalescing of the burst an editor/atomic-save emits is left to the
/// boot loop's existing `RELOAD_QUIET_WINDOW` trailing-edge debounce.
pub(crate) fn spawn(
    config_path: &Path,
    reload_requested: Arc<AtomicBool>,
) -> Option<notify::RecommendedWatcher> {
    let dir = config_path.parent()?.to_path_buf();
    let file_name = config_path.file_name()?.to_os_string();
    // First run may predate the config dir; create it so the watch can attach.
    let _ = std::fs::create_dir_all(&dir);

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if !is_relevant(&event.kind) {
            return;
        }
        // Atomic-rename saves report the dir plus the temp/target paths; match by file
        // name so a sibling file's write in the same dir doesn't trigger a reload.
        let touches_config = event_touches_config(&event.paths, file_name.as_os_str());
        if touches_config {
            reload_requested.store(true, Ordering::Relaxed);
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            log::warn!(
                target: "engine",
                "config watcher init failed ({e}); using stat backstop"
            );
            return None;
        }
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        log::warn!(
            target: "engine",
            "config watch on {dir:?} failed ({e}); using stat backstop"
        );
        return None;
    }
    Some(watcher)
}

/// Gate out the events that can't reflect an edit: a pure `Access` (open/read) and the
/// catch-all `Other`. `Create`/`Modify`/`Remove` all pass — including `Modify(Metadata)`, a
/// mere attribute touch, which at worst triggers one extra idempotent reload (cheaper than
/// risking a missed save on a filesystem that reports a content change as metadata).
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Does any path in an event match the watched file by NAME (not full path)? Atomic-rename
/// saves report the dir plus the temp/target paths; matching by name (rather than requiring
/// an exact path) is what lets this survive the rename while still ignoring a sibling file's
/// unrelated write in the same watched directory.
fn event_touches_config(paths: &[PathBuf], file_name: &OsStr) -> bool {
    paths.iter().any(|p| p.file_name() == Some(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevant_kinds_trigger_reload_others_dont() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
        assert!(is_relevant(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_relevant(&EventKind::Remove(RemoveKind::File)));
        assert!(!is_relevant(&EventKind::Access(AccessKind::Any)));
        assert!(!is_relevant(&EventKind::Other));
    }

    #[test]
    fn atomic_rename_shape_matches_by_name() {
        // Atomic-save events report both the temp/staging path and the real target path;
        // matching should succeed on the real target name regardless of what else is listed.
        let paths = vec![
            PathBuf::from("/home/user/.config/dontspeak/settings.json.tmp12345"),
            PathBuf::from("/home/user/.config/dontspeak/settings.json"),
        ];
        assert!(event_touches_config(&paths, OsStr::new("settings.json")));
    }

    #[test]
    fn sibling_file_write_does_not_match() {
        let paths = vec![PathBuf::from("/home/user/.config/dontspeak/other.json")];
        assert!(!event_touches_config(&paths, OsStr::new("settings.json")));
    }

    #[test]
    fn only_one_matching_path_among_several_still_matches() {
        let paths = vec![
            PathBuf::from("/home/user/.config/dontspeak/other.json"),
            PathBuf::from("/home/user/.config/dontspeak/settings.json"),
            PathBuf::from("/home/user/.config/dontspeak/another.json"),
        ];
        assert!(event_touches_config(&paths, OsStr::new("settings.json")));
    }

    #[test]
    fn empty_paths_does_not_match() {
        assert!(!event_touches_config(&[], OsStr::new("settings.json")));
    }
}
