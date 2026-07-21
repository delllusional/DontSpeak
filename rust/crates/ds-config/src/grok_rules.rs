//! Managed Grok narrate section in `~/.grok/AGENTS.md`.
//!
//! Grok ignores UserPromptSubmit stdout (#95), so digests go here at session start.
//! Marker-bounded section for [`crate::DEFAULT_NARRATION_SPEC`]; user rules kept.

use std::path::Path;

use crate::narration::DEFAULT_NARRATION_SPEC;
use crate::wire::settings::atomic_write_str;

/// Start marker for the managed narrate section in `~/.grok/AGENTS.md`.
pub const GROK_NARRATE_BEGIN: &str = "<!-- dontspeak-narrate:begin -->";
/// End marker for the managed narrate section in `~/.grok/AGENTS.md`.
pub const GROK_NARRATE_END: &str = "<!-- dontspeak-narrate:end -->";

/// PURE: insert, replace, or remove the managed narrate section.
///
/// - `body = Some(spec)` → exactly one managed section with trimmed `spec`.
/// - `body = None` → strip managed section; leave the rest.
///
/// Idempotent.
pub fn apply_grok_narrate_section(existing: &str, body: Option<&str>) -> String {
    let stripped = strip_managed_section(existing);
    match body {
        None => normalize_trailing(stripped),
        Some(spec) => {
            let section = format!(
                "{GROK_NARRATE_BEGIN}\n{}\n{GROK_NARRATE_END}\n",
                spec.trim_end()
            );
            let base = stripped.trim_end();
            if base.is_empty() {
                section
            } else {
                format!("{base}\n\n{section}")
            }
        }
    }
}

/// Write or clear the managed section in `agents_md`.
/// `digests_on` injects [`DEFAULT_NARRATION_SPEC`]; false strips. Deletes empty file.
/// Returns whether the filesystem changed.
pub fn sync_grok_narrate_agents_md(agents_md: &Path, digests_on: bool) -> std::io::Result<bool> {
    let existing = match std::fs::read_to_string(agents_md) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let body = if digests_on {
        Some(DEFAULT_NARRATION_SPEC)
    } else {
        None
    };
    let next = apply_grok_narrate_section(&existing, body);
    if next == existing {
        return Ok(false);
    }
    if next.trim().is_empty() {
        if agents_md.exists() {
            std::fs::remove_file(agents_md)?;
        }
        return Ok(true);
    }
    atomic_write_str(agents_md, &next)?;
    Ok(true)
}

/// Best-effort sync from live voice config.
pub fn sync_grok_narrate_from_config(paths: &crate::Paths) -> std::io::Result<bool> {
    let digests_on = crate::VoiceConfig::load(paths).narrates(crate::NarrateKind::Digests);
    sync_grok_narrate_agents_md(&paths.grok_agents_md, digests_on)
}

fn strip_managed_section(existing: &str) -> String {
    let Some(start) = existing.find(GROK_NARRATE_BEGIN) else {
        return existing.to_string();
    };
    let after_begin = start + GROK_NARRATE_BEGIN.len();
    let end = existing[after_begin..]
        .find(GROK_NARRATE_END)
        .map(|rel| after_begin + rel + GROK_NARRATE_END.len())
        .unwrap_or(existing.len());
    // Drop one surrounding blank so repeated inject/strip doesn't grow whitespace.
    let mut before = &existing[..start];
    let mut after = &existing[end..];
    if before.ends_with("\n\n") {
        before = &before[..before.len() - 1];
    } else if before.ends_with('\n') && after.starts_with('\n') {
        after = &after[1..];
    }
    if after.starts_with('\n') {
        after = &after[1..];
    }
    format!("{before}{after}")
}

fn normalize_trailing(s: String) -> String {
    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn inject_into_empty_file() {
        let out = apply_grok_narrate_section("", Some(DEFAULT_NARRATION_SPEC));
        assert!(out.contains(GROK_NARRATE_BEGIN));
        assert!(out.contains(GROK_NARRATE_END));
        assert!(out.contains("Start every reply"));
        assert_eq!(
            apply_grok_narrate_section(&out, Some(DEFAULT_NARRATION_SPEC)),
            out,
            "idempotent"
        );
    }

    #[test]
    fn preserves_user_content_around_managed_section() {
        let existing = "# My rules\n\nUse TypeScript.\n";
        let out = apply_grok_narrate_section(existing, Some("SPEC\n"));
        assert!(out.starts_with("# My rules"));
        assert!(out.contains("Use TypeScript."));
        assert!(out.contains("SPEC"));
        let stripped = apply_grok_narrate_section(&out, None);
        assert_eq!(stripped, existing);
    }

    #[test]
    fn replace_updates_spec_without_duplicating() {
        let once = apply_grok_narrate_section("keep\n", Some("first\n"));
        let twice = apply_grok_narrate_section(&once, Some("second\n"));
        assert_eq!(twice.matches(GROK_NARRATE_BEGIN).count(), 1);
        assert!(twice.contains("second"));
        assert!(!twice.contains("first"));
        assert!(twice.contains("keep"));
    }

    #[test]
    fn strip_missing_section_is_identity_normalized() {
        assert_eq!(apply_grok_narrate_section("hello\n", None), "hello\n");
        assert_eq!(apply_grok_narrate_section("", None), "");
    }

    #[test]
    fn strip_unclosed_section_drops_to_eof() {
        let messy = format!("user\n{GROK_NARRATE_BEGIN}\norphan");
        let out = apply_grok_narrate_section(&messy, None);
        assert_eq!(out, "user\n");
    }

    #[test]
    fn sync_writes_and_clears_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        assert!(sync_grok_narrate_agents_md(&path, true).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(GROK_NARRATE_BEGIN));
        assert!(text.contains("Start every reply"));
        assert!(!sync_grok_narrate_agents_md(&path, true).unwrap());
        assert!(sync_grok_narrate_agents_md(&path, false).unwrap());
        assert!(!path.exists(), "empty managed-only file is removed");
    }

    #[test]
    fn sync_clear_preserves_user_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# User\n\nkeep me\n").unwrap();
        assert!(sync_grok_narrate_agents_md(&path, true).unwrap());
        assert!(sync_grok_narrate_agents_md(&path, false).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# User\n\nkeep me\n");
    }
}
