//! Startup update check against GitHub `releases/latest` (excludes drafts/prereleases).
//! Semver compare of `tag_name` (strip leading `v`) vs running version — not string order.
//! [`check_for_update_at`] takes the API base so tests point it at httpmock; production
//! callers pass the real base. Blocking `http_get_builder` — run off the UI thread.

use serde_json::Value;

const REPO_SLUG: &str = "delllusional/DontSpeak";

/// Shape for `ds_update_check_json` / host "update available" UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub update_available: bool,
    /// Normalized via `semver::Version` (leading `v` stripped).
    pub current_version: String,
    pub latest_version: String,
    /// Release page click-through; empty if response omitted `html_url`.
    pub html_url: String,
}

impl UpdateInfo {
    /// `{"update_available","current_version","latest_version","html_url"}` for the FFI.
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

/// GET + parse + compare; `api_base` is mockable. Same path as production.
pub fn check_for_update_at(api_base: &str, current_version: &str) -> std::io::Result<UpdateInfo> {
    let url = format!("{api_base}/repos/{REPO_SLUG}/releases/latest");
    let body = crate::download::http_get_builder(&url)
        // GitHub rejects empty User-Agent.
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
