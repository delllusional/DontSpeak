//! Product brand strings — single source for name + version.
//! [`DISPLAY_NAME`] title-case; [`VERSION`] from workspace (`version.workspace = true`).

/// Human-facing product name (binary/server id is `dontspeak`).
pub const DISPLAY_NAME: &str = "DontSpeak";

/// Workspace version via `CARGO_PKG_VERSION` (single source).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `"DontSpeak 0.2.0"` for logs / UI.
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
        assert_eq!(s, format!("{DISPLAY_NAME} {VERSION}"));
        assert!(!VERSION.is_empty() && VERSION.contains('.'));
    }
}
