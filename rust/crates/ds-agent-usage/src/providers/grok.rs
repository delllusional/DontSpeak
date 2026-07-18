use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{
    number_at, read_json_file, request, resolve_binary, rfc3339_timestamp, rpc, string_at,
};
use crate::{Period, UsageRow};

/// CodexBar web billing endpoint used when `grok agent stdio` lacks `x.ai/billing`.
const WEB_BILLING_URL: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
/// Empty protobuf message framed as gRPC-web (flags=0, length=0).
const GRPC_WEB_EMPTY_FRAME: &[u8] = &[0x00, 0x00, 0x00, 0x00, 0x00];
const OIDC_SCOPE_PREFIX: &str = "https://auth.x.ai::";
const LEGACY_SESSION_SCOPE: &str = "https://accounts.x.ai/sign-in";
const MAX_WEB_BODY: usize = 256 * 1024;

pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    match fetch_cli(paths) {
        Ok(windows) if !windows.is_empty() => Ok(windows),
        Ok(_) | Err(_) => fetch_web(paths),
    }
}

fn fetch_cli(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let binary = resolve_binary("grok", paths)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Grok CLI unavailable"))?;
    let result = rpc::call(rpc::Request {
        binary: &binary,
        arguments: &["agent", "stdio"],
        initialize_params: serde_json::json!({
            "protocolVersion": "1",
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false,
            },
            "clientInfo": {
                "name": "DontSpeak",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
        send_initialized: false,
        method: "x.ai/billing",
        params: serde_json::json!({}),
        initialize_timeout: Duration::from_secs(8),
        request_timeout: Duration::from_secs(12),
    })?;
    Ok(parse_cli(&result).into_iter().collect())
}

fn fetch_web(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let credentials = read_json_file(&paths.grok_dir.join("auth.json"))?;
    let token = access_token(&credentials).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "Grok OAuth token unavailable")
    })?;
    let response = request(ds_http::Method::POST, WEB_BILLING_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/?_s=usage")
        .header("Accept", "*/*")
        .header("Content-Type", "application/grpc-web+proto")
        .header("x-grpc-web", "1")
        .header("x-user-agent", "connect-es/2.1.1")
        .header("User-Agent", "DontSpeak-agent-usage")
        .bytes(GRPC_WEB_EMPTY_FRAME.to_vec())
        .send()
        .map_err(|error| std::io::Error::other(format!("Grok web billing failed: {error}")))?;
    let body = ds_http::read_bytes_limited(response, MAX_WEB_BODY)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    parse_web_billing(&body, now)
        .map(|window| vec![window])
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Grok web billing response unusable",
            )
        })
}

/// Prefer SuperGrok OIDC scope entries, then legacy session scopes (CodexBar).
fn access_token(auth: &Value) -> Option<String> {
    let object = auth.as_object()?;
    let mut oidc = None;
    let mut legacy = None;
    for (scope, entry) in object {
        let Some(key) = string_at(entry, &["key"]).map(str::to_owned) else {
            continue;
        };
        if scope.starts_with(OIDC_SCOPE_PREFIX) {
            oidc = Some(key);
        } else if scope == LEGACY_SESSION_SCOPE || scope.contains("/sign-in") {
            legacy = Some(key);
        }
    }
    oidc.or(legacy)
}

/// Local identity from `~/.grok/auth.json` session entry `email`. No network.
pub(crate) fn account(paths: &ds_config::Paths) -> Option<String> {
    let auth = read_json_file(&paths.grok_dir.join("auth.json")).ok()?;
    account_from_auth(&auth)
}

fn account_from_auth(auth: &Value) -> Option<String> {
    let object = auth.as_object()?;
    // Prefer OIDC scopes (same order as token selection), then any entry with email.
    let mut oidc = None;
    let mut legacy = None;
    let mut any = None;
    for (scope, entry) in object {
        let Some(email) = string_at(entry, &["email"]).map(str::to_owned) else {
            continue;
        };
        if scope.starts_with(OIDC_SCOPE_PREFIX) {
            oidc = Some(email);
        } else if scope == LEGACY_SESSION_SCOPE || scope.contains("/sign-in") {
            legacy = Some(email);
        } else {
            any = Some(email);
        }
    }
    oidc.or(legacy).or(any)
}

fn parse_cli(json: &Value) -> Option<UsageRow> {
    let limit = json
        .get("monthlyLimit")
        .and_then(|value| number_at(value, "val"))?;
    if limit <= 0.0 {
        return None;
    }
    let used = json
        .get("usage")?
        .get("totalUsed")
        .and_then(|value| number_at(value, "val"))?;
    let reset = json
        .get("billingCycle")?
        .get("billingPeriodEnd")?
        .as_str()
        .and_then(rfc3339_timestamp)?;
    // CLI surface names the field monthly; keep that semantic when present.
    UsageRow::checked(Period::Month, used / limit * 100.0, reset)
}

/// Parse CodexBar-compatible gRPC-web billing: fixed32 percent + future reset.
fn parse_web_billing(body: &[u8], now_unix: i64) -> Option<UsageRow> {
    let frames = grpc_web_data_frames(body);
    let payloads = if frames.is_empty() && looks_like_protobuf(body) {
        vec![body]
    } else {
        frames
    };
    if payloads.is_empty() {
        return None;
    }
    if grpc_web_failed(body) {
        return None;
    }

    let mut scan = ProtobufScan::default();
    for payload in payloads {
        // Wire types 3/4 (or unknown) make the scan unusable — bail rather than
        // misalign on a 1-byte advance (issue #109 m6).
        scan.merge(scan_protobuf(payload, 0, &[])?);
    }

    let used_percent = scan
        .fixed32
        .iter()
        .filter(|field| {
            field.path.last() == Some(&1)
                && field.value.is_finite()
                && (0.0..=100.0).contains(&field.value)
        })
        .min_by(|left, right| {
            left.path
                .len()
                .cmp(&right.path.len())
                .then(left.order.cmp(&right.order))
        })
        .map(|field| f64::from(field.value));

    let reset_candidates: Vec<(Vec<u64>, i64)> = scan
        .varints
        .iter()
        .filter_map(|field| {
            let raw = i64::try_from(field.value).ok()?;
            if (1_700_000_000..=2_100_000_000).contains(&raw) {
                Some((field.path.clone(), raw))
            } else {
                None
            }
        })
        .collect();
    let future: Vec<(Vec<u64>, i64)> = reset_candidates
        .into_iter()
        .filter(|(_, ts)| *ts > now_unix)
        .collect();
    // Preferred path [1, 5, 1]: nested reset timestamp from CodexBar-compatible
    // GetGrokCreditsConfig captures (see `parses_live_shaped_web_billing_frame`
    // fixture). Re-derive from a live frame if upstream schema drifts; fallback
    // is the earliest future unix varint (#110 n5).
    let preferred = future
        .iter()
        .filter(|(path, _)| path.as_slice() == [1, 5, 1])
        .map(|(_, ts)| *ts)
        .min();
    let resets_at = preferred.or_else(|| future.iter().map(|(_, ts)| *ts).min())?;

    let has_usage_period = scan.varints.iter().any(|field| {
        field.path.starts_with(&[1, 6])
            || (field.path.as_slice() == [1, 8, 1] && (field.value == 1 || field.value == 2))
    });
    let used_percent = match used_percent {
        Some(value) => value,
        None if has_usage_period && scan.fixed32.is_empty() => 0.0,
        None => return None,
    };

    let period = period_from_reset_distance(resets_at.saturating_sub(now_unix));
    UsageRow::checked(period, used_percent, resets_at)
}

/// CodexBar labels Grok credit windows by remaining cycle length.
fn period_from_reset_distance(seconds: i64) -> Period {
    let days = ((seconds as f64) / 86_400.0).round() as i64;
    if (4..=12).contains(&days) {
        Period::Week
    } else {
        Period::Month
    }
}

fn grpc_web_data_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let end = start.saturating_add(length);
        if end > data.len() {
            return Vec::new();
        }
        if flags & 0x80 == 0 {
            frames.push(&data[start..end]);
        }
        index = end;
    }
    frames
}

fn grpc_web_failed(data: &[u8]) -> bool {
    // Trailer frames (MSB set) may carry grpc-status / grpc-message as text.
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = u32::from_be_bytes([
            data[index + 1],
            data[index + 2],
            data[index + 3],
            data[index + 4],
        ]) as usize;
        let start = index + 5;
        let end = start.saturating_add(length);
        if end > data.len() {
            break;
        }
        if flags & 0x80 != 0
            && let Ok(text) = std::str::from_utf8(&data[start..end])
        {
            for line in text.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                if key.trim().eq_ignore_ascii_case("grpc-status") {
                    let status = value.trim();
                    if status != "0" && !status.is_empty() {
                        return true;
                    }
                }
            }
        }
        index = end;
    }
    false
}

fn looks_like_protobuf(data: &[u8]) -> bool {
    let Some(first) = data.first() else {
        return false;
    };
    let field_number = first >> 3;
    let wire_type = first & 0x07;
    field_number > 0 && matches!(wire_type, 0 | 1 | 2 | 5)
}

#[derive(Default)]
struct ProtobufScan {
    fixed32: Vec<Fixed32Field>,
    varints: Vec<VarintField>,
}

#[derive(Clone)]
struct Fixed32Field {
    path: Vec<u64>,
    value: f32,
    order: usize,
}

#[derive(Clone)]
struct VarintField {
    path: Vec<u64>,
    value: u64,
}

impl ProtobufScan {
    fn merge(&mut self, other: Self) {
        self.fixed32.extend(other.fixed32);
        self.varints.extend(other.varints);
    }
}

/// Walk length-delimited protobuf, collecting fixed32 + varint leaves.
///
/// Returns `None` on deprecated wire types 3/4 (start/end group) or unknown
/// types 6/7: advancing one byte would desync the rest of the stream and
/// invent garbage fields. Modern protos never emit groups; treat the payload
/// unusable rather than guess (issue #109 m6).
fn scan_protobuf(data: &[u8], depth: u8, path: &[u64]) -> Option<ProtobufScan> {
    let mut scan = ProtobufScan::default();
    let mut index = 0;
    let mut order = 0usize;
    while index < data.len() {
        let field_start = index;
        let Some(key) = read_varint(data, &mut index) else {
            index = field_start.saturating_add(1);
            continue;
        };
        if key == 0 {
            index = field_start.saturating_add(1);
            continue;
        }
        let field_number = key >> 3;
        let wire_type = key & 0x07;
        let mut field_path = path.to_vec();
        field_path.push(field_number);
        match wire_type {
            0 => {
                if let Some(value) = read_varint(data, &mut index) {
                    scan.varints.push(VarintField {
                        path: field_path,
                        value,
                    });
                } else {
                    index = field_start.saturating_add(1);
                }
            }
            1 => {
                if index + 8 > data.len() {
                    break;
                }
                index += 8;
            }
            2 => {
                let Some(length) = read_varint(data, &mut index) else {
                    index = field_start.saturating_add(1);
                    continue;
                };
                let length = length as usize;
                if index.saturating_add(length) > data.len() {
                    index = field_start.saturating_add(1);
                    continue;
                }
                let nested = &data[index..index + length];
                index += length;
                if depth < 4 {
                    scan.merge(scan_protobuf(nested, depth + 1, &field_path)?);
                }
            }
            5 => {
                if index + 4 > data.len() {
                    break;
                }
                let bits = u32::from_le_bytes([
                    data[index],
                    data[index + 1],
                    data[index + 2],
                    data[index + 3],
                ]);
                scan.fixed32.push(Fixed32Field {
                    path: field_path,
                    value: f32::from_bits(bits),
                    order,
                });
                order += 1;
                index += 4;
            }
            // 3/4 = deprecated start/end group; 6/7 reserved. Never 1-byte-advance
            // (desync + garbage fields) — treat payload unusable instead.
            _ => return None,
        }
    }
    Some(scan)
}

fn read_varint(data: &[u8], index: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u64;
    while *index < data.len() && shift < 64 {
        let byte = data[*index];
        *index += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cli_monthly_included_usage() {
        let window = parse_cli(&serde_json::json!({
            "billingCycle": { "billingPeriodEnd": "2026-08-01T00:00:00Z" },
            "monthlyLimit": { "val": 99900 },
            "usage": { "totalUsed": { "val": 49950 } }
        }))
        .unwrap();
        assert_eq!(window.period, Period::Month);
        assert_eq!(window.used_percent, 50.0);
    }

    #[test]
    fn missing_cli_limit_is_unavailable() {
        assert!(
            parse_cli(&serde_json::json!({
                "billingCycle": { "billingPeriodEnd": "2026-08-01T00:00:00Z" },
                "usage": { "totalUsed": { "val": 10 } }
            }))
            .is_none()
        );
    }

    #[test]
    fn prefers_oidc_scope_token() {
        let auth = serde_json::json!({
            "https://accounts.x.ai/sign-in": { "key": "legacy-token" },
            "https://auth.x.ai::abc": { "key": "oidc-token" }
        });
        assert_eq!(access_token(&auth).as_deref(), Some("oidc-token"));
    }

    #[test]
    fn account_prefers_oidc_email() {
        let auth = serde_json::json!({
            "https://accounts.x.ai/sign-in": {
                "key": "legacy",
                "email": "legacy@x.ai"
            },
            "https://auth.x.ai::abc": {
                "key": "oidc",
                "email": "user@x.ai"
            }
        });
        assert_eq!(account_from_auth(&auth).as_deref(), Some("user@x.ai"));
    }

    #[test]
    fn parses_live_shaped_web_billing_frame() {
        // Captured shape from GetGrokCreditsConfig (percent + weekly reset).
        // gRPC-web: data frame (len 86) + trailer grpc-status:0.
        // Exact capture from a live GetGrokCreditsConfig response (usage only).
        let body = hex_literal(
            "00000000560a540d0000004112001a00220c089effe6d20610f894f79f022a0c\
             089ef48bd30610f894f79f023a0708021500000041421e0802120c089effe6d2\
             0610f894f79f021a0c089ef48bd30610f894f79f02580162006801800000000f\
             677270632d7374617475733a300d0a",
        );
        // now just before the preferred reset (2026-07-24T05:37:34Z = 1784871454)
        let window = parse_web_billing(&body, 1_784_300_000).unwrap();
        assert!((window.used_percent - 8.0).abs() < 0.01);
        assert_eq!(window.resets_at_unix, 1_784_871_454);
        assert_eq!(window.period, Period::Week);
    }

    #[test]
    fn rejects_nonzero_grpc_status_trailer() {
        let body = b"\x80\x00\x00\x00\x0egrpc-status:7\r\n";
        assert!(parse_web_billing(body, 1_800_000_000).is_none());
    }

    #[test]
    fn rejects_deprecated_group_wire_types() {
        // Field 1, wire type 3 (start group) — must not 1-byte-advance into garbage.
        let start_group = [0x0b]; // (1 << 3) | 3
        assert!(scan_protobuf(&start_group, 0, &[]).is_none());
        // Field 1, wire type 4 (end group).
        let end_group = [0x0c]; // (1 << 3) | 4
        assert!(scan_protobuf(&end_group, 0, &[]).is_none());
        // Nested: length-delimited field containing a start-group tag.
        // key=(1<<3)|2=0x0a, len=1, body=0x0b
        let nested_group = [0x0a, 0x01, 0x0b];
        assert!(scan_protobuf(&nested_group, 0, &[]).is_none());
        // Raw body with only a group tag: parse_web_billing treats as unusable.
        assert!(parse_web_billing(&start_group, 1_800_000_000).is_none());
    }

    #[test]
    fn period_from_reset_distance_matches_codexbar() {
        assert_eq!(period_from_reset_distance(7 * 86_400), Period::Week);
        assert_eq!(period_from_reset_distance(30 * 86_400), Period::Month);
    }

    fn hex_literal(hex: &str) -> Vec<u8> {
        let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..cleaned.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
            .collect()
    }
}
