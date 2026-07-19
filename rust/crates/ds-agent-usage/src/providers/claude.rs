use serde_json::Value;

#[cfg(target_os = "macos")]
use super::MAX_CREDENTIAL_BYTES;
use super::{read_json_file, request, rfc3339_timestamp, send_json, string_at};
use crate::{Period, UsageRow};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let credentials = read_credentials(paths)?;
    let token = string_at(&credentials, &["claudeAiOauth", "accessToken"])
        .or_else(|| string_at(&credentials, &["claude_ai_oauth", "access_token"]))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Claude OAuth token unavailable",
            )
        })?;
    let client = ds_config::client_spec(ds_config::ClientSource::ClaudeCode).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Claude client registry entry unavailable",
        )
    })?;
    let user_agent = format!("claude-code/{}", client.verified_client_version);
    let json = send_json(
        request(ds_http::Method::GET, USAGE_URL)?
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("Accept", "application/json")
            // Same client identity as Claude Code (read-only token consumer).
            .header("User-Agent", user_agent),
    )?;
    Ok(parse(&json))
}

/// Local email from `~/.claude.json`. No network.
pub(crate) fn account(paths: &ds_config::Paths) -> Option<String> {
    let config = read_json_file(&paths.claude_code_config).ok()?;
    string_at(&config, &["oauthAccount", "emailAddress"])
        .or_else(|| string_at(&config, &["oauthAccount", "email"]))
        .map(str::to_owned)
}

#[cfg(not(target_os = "macos"))]
fn read_credentials(paths: &ds_config::Paths) -> std::io::Result<Value> {
    read_json_file(&paths.claude_dir.join(".credentials.json"))
}

#[cfg(target_os = "macos")]
fn read_credentials(paths: &ds_config::Paths) -> std::io::Result<Value> {
    let file_error = match read_json_file(&paths.claude_dir.join(".credentials.json")) {
        Ok(credentials) => return Ok(credentials),
        Err(error) => error,
    };
    // macOS Keychain: skip protected items (never raise auth UI).
    match read_keychain_credentials() {
        Ok(credentials) => Ok(credentials),
        Err(_) => Err(file_error),
    }
}

#[cfg(target_os = "macos")]
fn read_keychain_credentials() -> std::io::Result<Value> {
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    let results = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .load_data(true)
        .skip_authenticated_items(true)
        .search()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Claude Keychain credentials unavailable",
            )
        })?;
    let bytes = results
        .into_iter()
        .find_map(|result| match result {
            SearchResult::Data(bytes) => Some(bytes),
            _ => None,
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Claude Keychain credentials unavailable",
            )
        })?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential payload exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// `five_hour` + `seven_day` only (no model-scoped / $ extra).
fn parse(json: &Value) -> Vec<UsageRow> {
    [
        parse_window(json.get("five_hour"), Period::Session),
        parse_window(json.get("seven_day"), Period::Week),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn parse_window(value: Option<&Value>, period: Period) -> Option<UsageRow> {
    let window = value?.as_object()?;
    // Already 0..100 (1.0 = 1%, not full).
    let used = window.get("utilization").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|n| n as f64))
            .or_else(|| v.as_str()?.trim().parse().ok())
    })?;
    let reset = window
        .get("resets_at")?
        .as_str()
        .and_then(rfc3339_timestamp)?;
    UsageRow::checked(period, used, reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_five_hour_and_generic_seven_day() {
        let windows = parse(&serde_json::json!({
            "five_hour": { "utilization": 80, "resets_at": "2026-07-17T18:00:00Z" },
            "seven_day": { "utilization": 22.5, "resets_at": "2026-07-24T18:00:00Z" },
            "seven_day_opus": { "utilization": 90, "resets_at": "2026-07-24T18:00:00Z" }
        }));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].period, Period::Session);
        assert_eq!(windows[0].used_percent, 80.0);
        assert!(windows[0].resets_at_unix > 0);
        assert_eq!(windows[1].period, Period::Week);
        assert_eq!(windows[1].used_percent, 22.5);
        assert!(windows[1].resets_at_unix > 0);
    }

    #[test]
    fn weekly_only_when_session_window_absent() {
        let windows = parse(&serde_json::json!({
            "seven_day": { "utilization": 22.5, "resets_at": "2026-07-24T18:00:00Z" }
        }));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].period, Period::Week);
    }

    #[test]
    fn missing_reset_is_not_a_quota_row() {
        assert!(parse(&serde_json::json!({ "seven_day": { "utilization": 20 } })).is_empty());
    }

    #[test]
    fn accepts_fractional_reset_timestamps() {
        let windows = parse(&serde_json::json!({
            "five_hour": {
                "utilization": 0.42,
                "resets_at": "2025-12-12T20:59:59.707736+00:00"
            },
            "seven_day": {
                "utilization": 27.0,
                "resets_at": "2025-12-16T03:59:59.707754+00:00"
            }
        }));
        assert_eq!(windows.len(), 2);
        // Sub-percent utilization stays sub-percent (not ×100 as a fraction).
        assert!((windows[0].used_percent - 0.42).abs() < 0.01);
        assert_eq!(windows[0].period, Period::Session);
        assert!(windows[0].resets_at_unix > 0);
        assert_eq!(windows[1].used_percent, 27.0);
    }

    #[test]
    fn utilization_one_is_one_percent_not_full() {
        // Live API: five_hour.utilization == 1.0 with limits[].percent == 1.
        let windows = parse(&serde_json::json!({
            "five_hour": {
                "utilization": 1.0,
                "resets_at": "2026-07-17T18:00:00Z"
            }
        }));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 1.0);
    }

    #[test]
    fn account_reads_oauth_email_from_claude_json() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        std::fs::write(
            &paths.claude_code_config,
            r#"{"oauthAccount":{"emailAddress":"me@anthropic.test","displayName":"Me"}}"#,
        )
        .unwrap();
        assert_eq!(account(&paths).as_deref(), Some("me@anthropic.test"));
    }
}
