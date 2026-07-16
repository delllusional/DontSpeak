//! Push-based config watch (`notify` crate: FSEvents / inotify / ReadDirectoryChangesW).
//! Flips `reload_requested` on change. Boot keeps a coarse `stat()` backstop
//! (`MTIME_CHECK_INTERVAL`) if the watcher fails or drops events.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Watch `config_path` parent dir; set `reload_requested` on create/modify/remove/rename.
/// Caller MUST keep the handle (drop stops the watch). `None` if watcher can't start.
///
/// Watch the PARENT, not the file: atomic temp+rename replaces the inode, so a file
/// watch goes deaf after the first save. Burst coalescing is `RELOAD_QUIET_WINDOW`.
pub(crate) fn spawn(
    config_path: &Path,
    reload_requested: Arc<AtomicBool>,
) -> Option<notify::RecommendedWatcher> {
    let dir = config_path.parent()?.to_path_buf();
    let file_name = config_path.file_name()?.to_os_string();
    // First run may predate the config dir; create it so the watch can attach.
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::debug!(target: "engine", "could not create config watch directory {dir:?}: {e}");
        return None;
    }

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if !is_relevant(&event.kind) {
            return;
        }
        // Match by file name — atomic-rename lists temp + target; ignore siblings.
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

/// Drop Access/Other; keep Create/Modify/Remove (incl. metadata — cheap extra reload).
fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Match watched file by name (survives atomic-rename; ignores sibling writes).
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
        let paths = vec![
            PathBuf::from("/home/user/.config/dontspeak/config.toml.tmp12345"),
            PathBuf::from("/home/user/.config/dontspeak/config.toml"),
        ];
        assert!(event_touches_config(&paths, OsStr::new("config.toml")));
    }

    #[test]
    fn sibling_file_write_does_not_match() {
        let paths = vec![PathBuf::from("/home/user/.config/dontspeak/other.json")];
        assert!(!event_touches_config(&paths, OsStr::new("config.toml")));
    }

    #[test]
    fn only_one_matching_path_among_several_still_matches() {
        let paths = vec![
            PathBuf::from("/home/user/.config/dontspeak/other.json"),
            PathBuf::from("/home/user/.config/dontspeak/config.toml"),
            PathBuf::from("/home/user/.config/dontspeak/another.json"),
        ];
        assert!(event_touches_config(&paths, OsStr::new("config.toml")));
    }

    #[test]
    fn empty_paths_does_not_match() {
        assert!(!event_touches_config(&[], OsStr::new("config.toml")));
    }
}
