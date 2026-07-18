use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use super::{integer_at, number_at, resolve_binary, rpc, string_at};
use crate::{Period, UsageRow};

/// CodexBar classifies Codex rate windows by exact duration minutes:
/// 300 → session/five-hour, 10080 → weekly.
const FIVE_HOUR_MINUTES: i64 = 5 * 60;
const WEEK_MINUTES: i64 = 7 * 24 * 60;
const FIVE_HOUR_SECONDS: i64 = FIVE_HOUR_MINUTES * 60;
const WEEK_SECONDS: i64 = WEEK_MINUTES * 60;

pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let binary = resolve_binary("codex", paths).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Codex CLI unavailable")
    })?;
    let result = rpc::call(rpc::Request {
        binary: &binary,
        arguments: &["app-server", "--listen", "stdio://"],
        initialize_params: serde_json::json!({
            "clientInfo": {
                "name": "dontspeak",
                "title": "DontSpeak",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
        send_initialized: true,
        method: "account/rateLimits/read",
        params: serde_json::json!({}),
        initialize_timeout: Duration::from_secs(4),
        request_timeout: Duration::from_secs(8),
    })?;
    Ok(parse(&result))
}

/// Local identity from `~/.codex/auth.json` JWT `id_token` email claim. No network.
pub(crate) fn account(paths: &ds_config::Paths) -> Option<String> {
    let auth = super::read_json_file(&paths.codex_dir.join("auth.json")).ok()?;
    let token = string_at(&auth, &["tokens", "id_token"])?;
    jwt_claim(token, "email").or_else(|| jwt_claim(token, "preferred_username"))
}

/// JWT payload claim (middle segment). No signature check — local credential.
fn jwt_claim(token: &str, claim: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    // JWT uses unpadded base64url (`-`/`_`). Already in the graph via ds-http/attohttpc.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    string_at(&json, &[claim]).map(str::to_owned)
}

fn parse(json: &Value) -> Vec<UsageRow> {
    let Some(rate_limit) = json.get("rateLimits").or_else(|| json.get("rate_limit")) else {
        return Vec::new();
    };
    ["primary", "secondary", "primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|key| parse_window(rate_limit.get(key)?))
        .collect()
}

fn parse_window(window: &Value) -> Option<UsageRow> {
    let explicit_period = ["period", "windowType", "window_type"]
        .into_iter()
        .find_map(|key| string_at(window, &[key]))
        .and_then(period_from_label);
    let duration_seconds = integer_at(window, "limit_window_seconds").or_else(|| {
        integer_at(window, "windowDurationMins")
            .or_else(|| integer_at(window, "window_duration_mins"))
            .and_then(|minutes| minutes.checked_mul(60))
    });
    let period = explicit_period.or(match duration_seconds {
        Some(FIVE_HOUR_SECONDS) => Some(Period::Session),
        Some(WEEK_SECONDS) => Some(Period::Week),
        // Calendar month cannot be inferred from an approximate day count.
        _ => None,
    })?;
    let used = number_at(window, "usedPercent").or_else(|| number_at(window, "used_percent"))?;
    let reset = integer_at(window, "resetsAt").or_else(|| integer_at(window, "reset_at"))?;
    UsageRow::checked(period, used, reset)
}

fn period_from_label(label: &str) -> Option<Period> {
    match label.trim().to_ascii_lowercase().as_str() {
        "five_hour" | "5h" | "5_hour" | "session" => Some(Period::Session),
        "week" | "weekly" | "seven_day" | "7_day" => Some(Period::Week),
        "month" | "monthly" | "billing_month" => Some(Period::Month),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_five_hour_and_weekly_from_duration() {
        let windows = parse(&serde_json::json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": {
                    "usedPercent": 12,
                    "resetsAt": 1_800_000_000,
                    "windowDurationMins": 300
                },
                "secondary": {
                    "usedPercent": 43,
                    "resetsAt": 1_800_500_000,
                    "windowDurationMins": 10_080
                }
            }
        }));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].period, Period::Session);
        assert_eq!(windows[0].used_percent, 12.0);
        assert_eq!(windows[1].period, Period::Week);
        assert_eq!(windows[1].used_percent, 43.0);
    }

    #[test]
    fn keeps_compatibility_with_snake_case_usage_shape() {
        let window = parse_window(&serde_json::json!({
            "used_percent": 9,
            "reset_at": 1_800_000_000,
            "limit_window_seconds": 604_800
        }))
        .unwrap();
        assert_eq!(window.period, Period::Week);
    }

    #[test]
    fn does_not_infer_a_calendar_month_from_thirty_days() {
        assert!(
            parse_window(&serde_json::json!({
                "usedPercent": 9,
                "resetsAt": 1_800_000_000,
                "windowDurationMins": 43_200
            }))
            .is_none()
        );
    }

    #[test]
    fn accepts_an_explicit_monthly_period() {
        let window = parse_window(&serde_json::json!({
            "period": "billing_month",
            "usedPercent": 9,
            "resetsAt": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(window.period, Period::Month);
    }

    #[test]
    fn accepts_explicit_session_label() {
        let window = parse_window(&serde_json::json!({
            "windowType": "session",
            "usedPercent": 55,
            "resetsAt": 1_800_000_000
        }))
        .unwrap();
        assert_eq!(window.period, Period::Session);
    }

    #[test]
    fn jwt_claim_reads_email_from_payload() {
        // {"email":"dev@openai.com"} base64url
        let payload = "eyJlbWFpbCI6ImRldkBvcGVuYWkuY29tIn0";
        let token = format!("hdr.{payload}.sig");
        assert_eq!(
            jwt_claim(&token, "email").as_deref(),
            Some("dev@openai.com")
        );
    }
}
