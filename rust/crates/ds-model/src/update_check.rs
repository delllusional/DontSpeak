//! Startup update check: is a newer DontSpeak release out?
//!
//! Hits GitHub's REST `releases/latest` endpoint for this repo (which already excludes
//! drafts/prereleases — no client-side filtering needed), parses the release's `tag_name` as
//! semver (a leading `v` is stripped, matching this repo's tag convention — see `make-release`),
//! and compares it against the RUNNING version via a real semver ordering (not a string
//! compare, which would misorder e.g. "0.9.0" vs "0.10.0"). [`crate::update_check::check_for_update_at`] takes the
//! API base as a parameter so tests can point it at a local httpmock server instead of the
//! real `api.github.com`; [`check_for_update`] is the thin wrapper the ds-core FFI boundary
//! calls, pointed at the real GitHub API.
//!
//! No async runtime (matches the rest of ds-model): one blocking `attohttpc` GET using the
//! crate's shared timeout/TLS-root builder (`crate::download::http_get_builder`). Network —
//! callers (the ds-core FFI, then each host's UI) must run this off the main/UI thread, the
//! same way `ds_model_status_wait` is documented to be.

use serde_json::Value;

/// GitHub REST API base — parameterized so tests can swap in a local mock server.
const GITHUB_API_BASE: &str = "https://api.github.com";

/// `owner/repo` slug for the release-check endpoint (matches the workspace `repository` field
/// in `rust/Cargo.toml`).
const REPO_SLUG: &str = "delllusional/DontSpeak";

/// Outcome of a startup update check — the shape [`UpdateInfo::to_json`] hands to the ds-core
/// FFI (`ds_update_check_json`), which every host's UI reads to decide whether to show the
/// "update available" pill next to the version number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// `true` when the latest GitHub release is a semver-newer version than `current_version`.
    pub update_available: bool,
    /// The running version, normalized through `semver::Version` (so e.g. "0.1.0" round-trips
    /// unchanged, but any leading `v` from the caller is stripped for consistency with
    /// `latest_version`).
    pub current_version: String,
    /// The latest release's version, normalized the same way.
    pub latest_version: String,
    /// The latest release's GitHub page — the "update available" pill's click-through target.
    /// Empty if the release response carried no `html_url`.
    pub html_url: String,
}

impl UpdateInfo {
    /// Serialize to the JSON object the ds-core FFI hands the UI:
    /// `{"update_available":bool,"current_version":str,"latest_version":str,"html_url":str}`.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "update_available": self.update_available,
            "current_version": self.current_version,
            "latest_version": self.latest_version,
            "html_url": self.html_url,
        })
        .to_string()
    }
}

/// Check the real GitHub API for a release newer than `current_version` (pass the running
/// `ds-core::VERSION`). See [`check_for_update_at`] for the exact request/parse/compare and its
/// failure modes (network error, non-2xx, malformed JSON, a `tag_name`/`current_version` that
/// isn't valid semver).
pub fn check_for_update(current_version: &str) -> std::io::Result<UpdateInfo> {
    check_for_update_at(GITHUB_API_BASE, current_version)
}

/// The GET + parse + compare half of [`check_for_update`], taking the API base as a parameter
/// so tests can point it at a local mock server — everything below this is the exact
/// production code path (same `http_get_builder`, same JSON shape, same semver compare).
pub fn check_for_update_at(api_base: &str, current_version: &str) -> std::io::Result<UpdateInfo> {
    let url = format!("{api_base}/repos/{REPO_SLUG}/releases/latest");
    let body = crate::download::http_get_builder(&url)
        // GitHub's REST API rejects requests with no User-Agent header; identify ourselves
        // rather than relying on attohttpc's generic "attohttpc/<ver>" default.
        .header("User-Agent", "DontSpeak-update-check")
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| std::io::Error::other(format!("update check request failed: {e}")))?;

    let json: Value = serde_json::from_str(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tag_name = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "release response has no tag_name",
            )
        })?;
    let html_url = json
        .get("html_url")
        .and_then(|u| u.as_str())
        .filter(|u| u.starts_with("https://github.com/"))
        .unwrap_or_default()
        .to_string();

    let latest = semver::Version::parse(tag_name.trim_start_matches('v')).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("release tag {tag_name:?} isn't semver: {e}"),
        )
    })?;
    let current = semver::Version::parse(current_version.trim_start_matches('v')).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("current version {current_version:?} isn't semver: {e}"),
        )
    })?;

    Ok(UpdateInfo {
        update_available: latest > current,
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        html_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_update_available_when_latest_tag_is_newer() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200).json_body(serde_json::json!({
                "tag_name": "v0.2.0",
                "html_url": "https://github.com/delllusional/DontSpeak/releases/tag/v0.2.0",
            }));
        });

        let info = check_for_update_at(&server.base_url(), "0.1.0").expect("update check parses");
        mock.assert();

        assert!(info.update_available);
        assert_eq!(info.current_version, "0.1.0");
        assert_eq!(info.latest_version, "0.2.0");
        assert_eq!(
            info.html_url,
            "https://github.com/delllusional/DontSpeak/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn reports_no_update_when_latest_tag_matches_current() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200)
                .json_body(serde_json::json!({ "tag_name": "v0.1.0", "html_url": "" }));
        });

        let info = check_for_update_at(&server.base_url(), "0.1.0").expect("update check parses");
        mock.assert();
        assert!(!info.update_available);
    }

    #[test]
    fn reports_no_update_when_latest_tag_is_older() {
        // Also exercises the real semver ORDERING (not a string compare): "0.9.0" > "0.10.0"
        // lexically, but "0.10.0" is semver-newer — the running version here must NOT be
        // reported as ahead of a genuinely older tag, and vice versa isn't confused either.
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200)
                .json_body(serde_json::json!({ "tag_name": "v0.9.0", "html_url": "" }));
        });

        let info = check_for_update_at(&server.base_url(), "0.10.0").expect("update check parses");
        mock.assert();
        assert!(!info.update_available);
    }

    #[test]
    fn errors_on_non_semver_tag_name() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200)
                .json_body(serde_json::json!({ "tag_name": "not-a-version" }));
        });

        let err = check_for_update_at(&server.base_url(), "0.1.0").unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("isn't semver"), "{err}");
    }

    #[test]
    fn errors_on_missing_tag_name() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200).json_body(serde_json::json!({}));
        });

        let err = check_for_update_at(&server.base_url(), "0.1.0").unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("no tag_name"), "{err}");
    }

    #[test]
    fn errors_on_http_error_status() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(404).body("Not Found");
        });

        let err = check_for_update_at(&server.base_url(), "0.1.0").unwrap_err();
        mock.assert();
        assert!(
            err.to_string().contains("update check request failed"),
            "{err}"
        );
    }

    #[test]
    fn to_json_serializes_the_expected_shape() {
        let info = UpdateInfo {
            update_available: true,
            current_version: "0.1.0".into(),
            latest_version: "0.2.0".into(),
            html_url: "https://example.com".into(),
        };
        let json: Value = serde_json::from_str(&info.to_json()).unwrap();
        assert_eq!(json["update_available"], true);
        assert_eq!(json["current_version"], "0.1.0");
        assert_eq!(json["latest_version"], "0.2.0");
        assert_eq!(json["html_url"], "https://example.com");
    }
}
