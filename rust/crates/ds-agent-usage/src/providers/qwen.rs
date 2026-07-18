use std::io::Read;
use std::path::Path;

use serde_json::Value;

use super::{
    MAX_CREDENTIAL_BYTES, integer_at, number_at, read_json_file, request, send_json, string_at,
};
use crate::{Period, UsageRow};

const INTL_BASE: &str = "https://modelstudio.console.alibabacloud.com";
const CN_BASE: &str = "https://bailian.console.aliyun.com";
const QUOTA_PATH: &str = "/data/api.json?action=zeldaEasy.broadscope-bailian.codingPlan.queryCodingPlanInstanceInfoV2&product=broadscope-bailian&api=queryCodingPlanInstanceInfoV2";
const API_KEY_NAMES: [&str; 4] = [
    "BAILIAN_CODING_PLAN_API_KEY",
    "ALIBABA_CODING_PLAN_API_KEY",
    "ALIBABA_QWEN_API_KEY",
    "DASHSCOPE_API_KEY",
];

pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let settings = read_json_file(&paths.qwen_settings).unwrap_or(Value::Null);
    let token = qwen_token(paths, &settings).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Qwen Coding Plan key unavailable",
        )
    })?;
    let is_cn = string_at(&settings, &["codingPlan", "region"])
        .is_some_and(|region| region.eq_ignore_ascii_case("cn"));
    let (base, region, commodity) = if is_cn {
        (CN_BASE, "cn-beijing", "sfm_codingplan_public_cn")
    } else {
        (INTL_BASE, "ap-southeast-1", "sfm_codingplan_public_intl")
    };
    let url = format!("{base}{QUOTA_PATH}&currentRegionId={region}");
    let body = serde_json::to_vec(&serde_json::json!({
        "queryCodingPlanInstanceInfoRequest": { "commodityCode": commodity }
    }))
    .map_err(std::io::Error::other)?;
    let json = send_json(
        request(ds_http::Method::POST, &url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-api-key", &token)
            .header("X-DashScope-API-Key", &token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Origin", base)
            .header("User-Agent", "DontSpeak-agent-usage")
            .bytes(body),
    )?;
    Ok(parse(&json))
}

fn qwen_token(paths: &ds_config::Paths, settings: &Value) -> Option<String> {
    API_KEY_NAMES
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        // Qwen loads its own dotenv before the general home dotenv.
        .or_else(|| token_from_dotenv(&paths.qwen_dir.join(".env")))
        .or_else(|| token_from_dotenv(&paths.home.join(".env")))
        .or_else(|| {
            API_KEY_NAMES
                .into_iter()
                .find_map(|key| string_at(settings, &["env", key]).map(str::to_owned))
        })
}

fn token_from_dotenv(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CREDENTIAL_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return None;
    }
    let contents = String::from_utf8(bytes).ok()?;
    API_KEY_NAMES
        .into_iter()
        .find_map(|key| dotenv_value(&contents, key))
}

fn dotenv_value(contents: &str, wanted: &str) -> Option<String> {
    // dotenv uses the final declaration when a key appears more than once.
    contents.lines().rev().find_map(|line| {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, raw) = line.split_once('=')?;
        if key.trim() != wanted {
            return None;
        }
        let raw = raw.trim();
        let value = if let Some(quoted) = raw.strip_prefix('"') {
            quoted.strip_suffix('"')?
        } else if let Some(quoted) = raw.strip_prefix('\'') {
            quoted.strip_suffix('\'')?
        } else {
            let comment = raw.char_indices().find_map(|(index, character)| {
                (character == '#' && (index == 0 || raw[..index].ends_with(char::is_whitespace)))
                    .then_some(index)
            });
            raw[..comment.unwrap_or(raw.len())].trim_end()
        };
        (!value.trim().is_empty()).then(|| value.to_owned())
    })
}

/// Coding Plan supplies five-hour, weekly, and billing-month counters (CodexBar
/// primary / secondary / tertiary). Emit every complete triple of used/total/reset.
fn parse(json: &Value) -> Vec<UsageRow> {
    let Some(quota) = find_quota_object(json) else {
        return Vec::new();
    };
    [
        parse_quota(
            quota,
            Period::Session,
            &["per5HourUsedQuota", "perFiveHourUsedQuota"],
            &["per5HourTotalQuota", "perFiveHourTotalQuota"],
            &[
                "per5HourQuotaNextRefreshTime",
                "perFiveHourQuotaNextRefreshTime",
            ],
        ),
        parse_quota(
            quota,
            Period::Week,
            &["perWeekUsedQuota"],
            &["perWeekTotalQuota"],
            &["perWeekQuotaNextRefreshTime"],
        ),
        parse_quota(
            quota,
            Period::Month,
            &["perBillMonthUsedQuota", "perMonthUsedQuota"],
            &["perBillMonthTotalQuota", "perMonthTotalQuota"],
            &[
                "perBillMonthQuotaNextRefreshTime",
                "perMonthQuotaNextRefreshTime",
            ],
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn find_quota_object(value: &Value) -> Option<&Value> {
    if value.get("perWeekUsedQuota").is_some()
        || value.get("perBillMonthUsedQuota").is_some()
        || value.get("per5HourUsedQuota").is_some()
        || value.get("perFiveHourUsedQuota").is_some()
    {
        return Some(value);
    }
    match value {
        Value::Object(object) => object.values().find_map(find_quota_object),
        Value::Array(array) => array.iter().find_map(find_quota_object),
        _ => None,
    }
}

fn parse_quota(
    quota: &Value,
    period: Period,
    used_keys: &[&str],
    total_keys: &[&str],
    reset_keys: &[&str],
) -> Option<UsageRow> {
    let total = total_keys.iter().find_map(|key| number_at(quota, key))?;
    if total <= 0.0 {
        return None;
    }
    let used = used_keys.iter().find_map(|key| number_at(quota, key))?;
    let raw_reset = reset_keys.iter().find_map(|key| integer_at(quota, key))?;
    let reset = if raw_reset > 10_000_000_000 {
        raw_reset / 1000
    } else {
        raw_reset
    };
    UsageRow::checked(period, used / total * 100.0, reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_five_hour_weekly_and_monthly_quota_fields() {
        let windows = parse(&serde_json::json!({
            "data": { "codingPlanQuotaInfo": {
                "per5HourUsedQuota": 20,
                "per5HourTotalQuota": 100,
                "per5HourQuotaNextRefreshTime": 1_800_000_000_000_i64,
                "perWeekUsedQuota": 800,
                "perWeekTotalQuota": 2000,
                "perWeekQuotaNextRefreshTime": 1_800_100_000_000_i64,
                "perBillMonthUsedQuota": 1200,
                "perBillMonthTotalQuota": 4000,
                "perBillMonthQuotaNextRefreshTime": 1_801_000_000_000_i64
            }}
        }));
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].period, Period::Session);
        assert_eq!(windows[0].used_percent, 20.0);
        assert_eq!(windows[0].resets_at_unix, 1_800_000_000);
        assert_eq!(windows[1].period, Period::Week);
        assert_eq!(windows[1].used_percent, 40.0);
        assert_eq!(windows[2].period, Period::Month);
        assert_eq!(windows[2].used_percent, 30.0);
    }

    #[test]
    fn non_positive_total_is_omitted() {
        let windows = parse(&serde_json::json!({
            "codingPlanQuotaInfo": {
                "perWeekUsedQuota": 0,
                "perWeekTotalQuota": 0,
                "perWeekQuotaNextRefreshTime": 1_800_000_000
            }
        }));
        assert!(windows.is_empty());
    }

    #[test]
    fn dotenv_parser_supports_exports_quotes_comments_and_last_value() {
        let dotenv = "\
# comment\n\
export DASHSCOPE_API_KEY=old\n\
DASHSCOPE_API_KEY=token#fragment # trailing comment\n\
ALIBABA_QWEN_API_KEY='quoted token'\n";
        assert_eq!(
            dotenv_value(dotenv, "DASHSCOPE_API_KEY").as_deref(),
            Some("token#fragment")
        );
        assert_eq!(
            dotenv_value(dotenv, "ALIBABA_QWEN_API_KEY").as_deref(),
            Some("quoted token")
        );
    }

    #[test]
    fn qwen_dotenv_has_key_priority_and_a_size_limit() {
        let root = tempfile::tempdir().unwrap();
        let dotenv = root.path().join(".env");
        std::fs::write(
            &dotenv,
            "DASHSCOPE_API_KEY=fallback\nBAILIAN_CODING_PLAN_API_KEY=preferred\n",
        )
        .unwrap();
        assert_eq!(token_from_dotenv(&dotenv).as_deref(), Some("preferred"));

        std::fs::write(&dotenv, vec![b'x'; MAX_CREDENTIAL_BYTES as usize + 1]).unwrap();
        assert_eq!(token_from_dotenv(&dotenv), None);
    }
}
