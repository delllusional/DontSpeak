use serde_json::Value;

use super::{
    integer_at, number_at, read_json_file, request, rfc3339_timestamp, send_json, string_at,
};
use crate::{Period, UsageRow};

/// Base for `/usages`; env override trimmed of trailing `/`.
const DEFAULT_BASE: &str = "https://api.kimi.com/coding/v1";

pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let credentials = read_json_file(&paths.kimi_credentials_json)?;
    let token = access_token(&credentials).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Kimi Code OAuth token unavailable",
        )
    })?;
    let base = std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .map(|value| value.trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_owned());
    let client = ds_config::client_spec(ds_config::WiredAgent::KimiCode);
    let user_agent = format!("kimi-code/{}", client.verified_client_version);
    let json = send_json(
        request(ds_http::Method::GET, &format!("{base}/usages"))?
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            // Same client identity as Kimi Code (read-only token consumer).
            .header("User-Agent", user_agent),
    )?;
    Ok(parse(&json))
}

/// OAuth file; camel `accessToken` alias.
fn access_token(credentials: &Value) -> Option<&str> {
    string_at(credentials, &["access_token"]).or_else(|| string_at(credentials, &["accessToken"]))
}

/// `usage` = week; `limits[]` 5h = session. `boosterWallet` ignored.
fn parse(json: &Value) -> Vec<UsageRow> {
    let mut rows: Vec<UsageRow> = parse_window(json.get("usage"), Period::Week)
        .into_iter()
        .collect();
    if let Some(limits) = json.get("limits").and_then(Value::as_array) {
        for item in limits {
            let detail = detail_of(item);
            if is_session_window(item, detail) {
                rows.extend(parse_window(Some(detail), Period::Session));
            }
        }
    }
    rows
}

fn detail_of(item: &Value) -> &Value {
    item.get("detail")
        .filter(|detail| detail.is_object())
        .unwrap_or(item)
}

/// 5h window: duration 300 minutes / 5 hours, or a `5h` name label. `window` may sit on the
/// item (live payloads: `{"window": {...}, "detail": {...}}`) or inside the detail itself.
fn is_session_window(item: &Value, detail: &Value) -> bool {
    if string_at(detail, &["name"])
        .or_else(|| string_at(item, &["window", "name"]))
        .is_some_and(|name| name.to_ascii_lowercase().contains("5h"))
    {
        return true;
    }
    let window = item
        .get("window")
        .filter(|window| window.is_object())
        .or_else(|| detail.get("window").filter(|window| window.is_object()))
        .unwrap_or(detail);
    let Some(duration) = number_at(window, "duration") else {
        return false;
    };
    let unit = string_at(window, &["timeUnit"])
        .or_else(|| string_at(window, &["time_unit"]))
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Live payloads spell units as TIME_UNIT_MINUTE / TIME_UNIT_HOUR (and "minutes"/"hours").
    (duration == 300.0 && unit.contains("min")) || (duration == 5.0 && unit.contains("hour"))
}

fn parse_window(value: Option<&Value>, period: Period) -> Option<UsageRow> {
    let detail = value.filter(|detail| detail.is_object())?;
    let limit = number_at(detail, "limit")?;
    if limit <= 0.0 {
        return None;
    }
    let used = number_at(detail, "used")
        .or_else(|| number_at(detail, "remaining").map(|remaining| limit - remaining))?;
    let reset = reset_unix(detail)?;
    UsageRow::checked(period, used / limit * 100.0, reset)
}

/// Absolute RFC3339 (`resetAt`/`resetTime`, fractional seconds tolerated) or relative
/// `reset_in` seconds. Live payloads use `resetTime` with nano fractions.
fn reset_unix(detail: &Value) -> Option<i64> {
    if let Some(raw) = string_at(detail, &["resetAt"])
        .or_else(|| string_at(detail, &["reset_at"]))
        .or_else(|| string_at(detail, &["resetTime"]))
        .or_else(|| string_at(detail, &["reset_time"]))
    {
        return rfc3339_timestamp(raw);
    }
    let seconds = integer_at(detail, "reset_in").or_else(|| integer_at(detail, "resetIn"))?;
    Some(now_unix().saturating_add(seconds))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_top_level_usage_to_the_week_row() {
        let rows = parse(&serde_json::json!({
            "usage": { "used": 40, "limit": 100, "resetAt": "2026-07-24T18:00:00Z" }
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].period, Period::Week);
        assert_eq!(rows[0].used_percent, 40.0);
        assert!(rows[0].resets_at_unix > 0);
    }

    #[test]
    fn maps_five_hour_limits_window_to_the_session_row() {
        let rows = parse(&serde_json::json!({
            "limits": [
                { "detail": {
                    "used": 150, "limit": 200,
                    "window": { "duration": 300, "timeUnit": "minutes" },
                    "resetAt": "2026-07-17T18:00:00Z"
                }},
                { "detail": {
                    "used": 5, "limit": 10,
                    "window": { "duration": 5, "timeUnit": "HOURS" },
                    "resetAt": "2026-07-17T19:00:00Z"
                }},
                { "detail": {
                    "used": 90, "limit": 100,
                    "window": { "duration": 10080, "timeUnit": "minutes" },
                    "resetAt": "2026-07-24T18:00:00Z"
                }}
            ]
        }));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.period == Period::Session));
        assert_eq!(rows[0].used_percent, 75.0);
        assert_eq!(rows[1].used_percent, 50.0);
    }

    #[test]
    fn parses_the_live_payload_shape() {
        // Captured 2026-07-18 from GET https://api.kimi.com/coding/v1/usages: string-typed
        // numbers, `resetTime` with nano fractions, TIME_UNIT_MINUTE window spelling.
        let rows = parse(&serde_json::json!({
            "usage": { "limit": "100", "used": "11", "remaining": "89",
                       "resetTime": "2026-07-25T13:19:58.615267Z" },
            "limits": [{
                "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
                "detail": { "limit": "100", "used": "54", "remaining": "46",
                            "resetTime": "2026-07-18T18:19:58.615267Z" }
            }]
        }));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].period, Period::Week);
        assert_eq!(rows[0].used_percent, 11.0);
        assert_eq!(rows[1].period, Period::Session);
        assert_eq!(rows[1].used_percent, 54.0);
        assert!(rows.iter().all(|row| row.resets_at_unix > 0));
    }

    #[test]
    fn five_hour_name_label_also_marks_the_session_window() {
        let rows = parse(&serde_json::json!({
            "limits": [{ "name": "5h rolling", "used": 10, "limit": 50, "reset_at": "2026-07-17T18:00:00Z" }]
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].period, Period::Session);
        assert_eq!(rows[0].used_percent, 20.0);
    }

    #[test]
    fn derives_used_from_remaining_when_used_is_absent() {
        let rows = parse(&serde_json::json!({
            "usage": { "remaining": 25, "limit": 100, "resetAt": "2026-07-24T18:00:00Z" }
        }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].used_percent, 75.0);
    }

    #[test]
    fn reset_in_is_seconds_from_now() {
        let before = now_unix();
        let rows = parse(&serde_json::json!({
            "usage": { "used": 1, "limit": 4, "reset_in": 3600 }
        }));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].resets_at_unix >= before + 3600);
        assert!(rows[0].resets_at_unix <= now_unix() + 3600);
    }

    #[test]
    fn accepts_fractional_reset_at_and_reset_in_camel_alias() {
        let rows = parse(&serde_json::json!({
            "usage": { "used": 1, "limit": 4, "resetAt": "2025-12-12T20:59:59.707736+00:00" },
            "limits": [{ "detail": {
                "used": 1, "limit": 2,
                "window": { "duration": 5, "timeUnit": "hours" },
                "resetIn": 60
            }}]
        }));
        assert_eq!(rows.len(), 2);
        assert!(rows[0].resets_at_unix > 0);
        assert!(rows[1].resets_at_unix > 0);
    }

    #[test]
    fn missing_reset_omits_the_row() {
        let rows = parse(&serde_json::json!({
            "usage": { "used": 40, "limit": 100 }
        }));
        assert!(rows.is_empty());
    }

    #[test]
    fn non_positive_limit_omits_the_row() {
        let rows = parse(&serde_json::json!({
            "usage": { "used": 0, "limit": 0, "resetAt": "2026-07-24T18:00:00Z" }
        }));
        assert!(rows.is_empty());
    }

    #[test]
    fn booster_wallet_is_never_a_quota_row() {
        let rows = parse(&serde_json::json!({
            "boosterWallet": { "used": 5, "limit": 10, "resetAt": "2026-07-24T18:00:00Z" }
        }));
        assert!(rows.is_empty());
    }

    #[test]
    fn access_token_accepts_snake_and_camel_and_rejects_blank() {
        assert_eq!(
            access_token(&serde_json::json!({ "access_token": "tok" })),
            Some("tok")
        );
        assert_eq!(
            access_token(&serde_json::json!({ "accessToken": "tok2" })),
            Some("tok2")
        );
        assert_eq!(
            access_token(&serde_json::json!({ "access_token": "  " })),
            None
        );
        assert_eq!(access_token(&serde_json::json!({})), None);
    }

    #[test]
    fn missing_or_empty_token_is_a_not_found_error() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        // No credentials file at all.
        let error = fetch(&paths).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

        let credentials_dir = paths.kimi_credentials_json.parent().unwrap();
        std::fs::create_dir_all(credentials_dir).unwrap();
        std::fs::write(
            &paths.kimi_credentials_json,
            r#"{"refresh_token":"r","expires_at":0,"scope":"s","token_type":"Bearer"}"#,
        )
        .unwrap();
        let error = fetch(&paths).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
