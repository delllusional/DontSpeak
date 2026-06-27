//! Product brand strings — the ONE place the human-facing product name and version
//! live, so callers never hardcode either. [`DISPLAY_NAME`] is the spaced, title-case
//! name shown to people ("DontSpeak" — distinct from the lowercase `dontspeak` binary /
//! MCP server id). [`VERSION`] is the workspace version (`version.workspace = true`), so
//! it tracks the engine and installer with no second source to bump.

/// The human-facing product name, with the space — what the installer's DisplayName and the
/// tray show. NOT the binary/server id (`dontspeak`).
pub const DISPLAY_NAME: &str = "DontSpeak";

/// The crate (= workspace) version, e.g. `"0.2.0"`. Single source: `Cargo.toml`'s
/// `version.workspace = true`, surfaced here so a caller reuses it instead of re-reading
/// `CARGO_PKG_VERSION` (or worse, hardcoding a number that drifts).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `"DontSpeak 0.2.0"` — the name + version joined, the one-line product identity for a
/// log line or any user-visible surface.
pub fn name_version() -> String {
    format!("{DISPLAY_NAME} {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_version_is_display_name_space_version() {
        let s = name_version();
        assert!(s.starts_with(DISPLAY_NAME), "leads with the product name");
        assert!(s.ends_with(VERSION), "ends with the version");
        // Exactly "DontSpeak <version>" — one space joining the two halves.
        assert_eq!(s, format!("{DISPLAY_NAME} {VERSION}"));
        // VERSION is the real workspace version, never empty/placeholder.
        assert!(!VERSION.is_empty() && VERSION.contains('.'));
    }
}
