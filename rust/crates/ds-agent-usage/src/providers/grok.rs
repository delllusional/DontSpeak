use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::{
    number_at, read_json_file, request, resolve_binary, rfc3339_timestamp, rpc, string_at,
};
use crate::{Period, UsageRow};

/// Web fallback when CLI lacks `x.ai/billing`.
const WEB_BILLING_URL: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
/// Empty gRPC-web frame.
const GRPC_WEB_EMPTY_FRAME: &[u8] = &[0x00, 0x00, 0x00, 0x00, 0x00];
const OIDC_SCOPE_PREFIX: &str = "https://auth.x.ai::";
const LEGACY_SESSION_SCOPE: &str = "https://accounts.x.ai/sign-in";
const MAX_WEB_BODY: usize = 256 * 1024;
/// Unix-second range accepted as a billing window bound (same as reset candidates).
const UNIX_TS_MIN: i64 = 1_700_000_000;
const UNIX_TS_MAX: i64 = 2_100_000_000;

pub(crate) fn fetch(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let cli = fetch_cli(paths);
    match cli {
        Ok(ref windows) if !windows.is_empty() => cli,
        // CLI missing/timeout/RPC/unusable → try web; on dual failure keep CLI category (#115).
        cli_outcome => match fetch_web(paths) {
            Ok(windows) if !windows.is_empty() => Ok(windows),
            Ok(_) => Err(finalize_after_web(
                cli_outcome.err(),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Grok web billing response unusable",
                ),
            )),
            Err(web_err) => Err(finalize_after_web(cli_outcome.err(), web_err)),
        },
    }
}

/// Prefer CLI ErrorKind when both fail; empty CLI → unusable payload.
fn finalize_after_web(cli_err: Option<std::io::Error>, web_err: std::io::Error) -> std::io::Error {
    match cli_err {
        Some(cli) => merge_cli_web_errors(cli, web_err),
        None => std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Grok CLI billing unusable; web: {}",
                sanitize_error_message(&web_err)
            ),
        ),
    }
}

/// Sanitized dual-failure message; `ErrorKind` from the CLI stage (NotFound / TimedOut / …).
fn merge_cli_web_errors(cli: std::io::Error, web: std::io::Error) -> std::io::Error {
    let kind = cli.kind();
    let cli_msg = sanitize_error_message(&cli);
    let web_msg = sanitize_error_message(&web);
    std::io::Error::new(kind, format!("Grok CLI: {cli_msg}; web: {web_msg}"))
}

/// Categories only — strip bodies/tokens/Authorization.
fn sanitize_error_message(error: &std::io::Error) -> String {
    let raw = error.to_string();
    if raw.len() > 160
        || raw.contains("Bearer ")
        || raw.contains("eyJ")
        || raw.to_ascii_lowercase().contains("authorization")
    {
        return match error.kind() {
            std::io::ErrorKind::NotFound => "unavailable".into(),
            std::io::ErrorKind::TimedOut => "timed out".into(),
            std::io::ErrorKind::InvalidData => "unusable payload".into(),
            _ => "failed".into(),
        };
    }
    raw
}

fn fetch_cli(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    let binary = resolve_binary(ds_config::WiredAgent::Grok, paths)
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
    match parse_cli(&result) {
        Some(row) => Ok(vec![row]),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Grok CLI billing unusable",
        )),
    }
}

fn fetch_web(paths: &ds_config::Paths) -> std::io::Result<Vec<UsageRow>> {
    // Credentials dropped before the request; token zeroed after the Authorization header is built.
    let mut token = {
        let credentials = read_json_file(&paths.grok_dir.join("auth.json"))?;
        let raw = access_token(&credentials).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "Grok OAuth token unavailable")
        })?;
        SecretString(raw)
    };
    let authorization = format!("Bearer {}", token.as_str());
    token.clear();

    // `authorization` is moved into the request and dropped with the response path.
    let response = request(ds_http::Method::POST, WEB_BILLING_URL)?
        .header("Authorization", authorization)
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
    super::reject_unauthorized(response.status())?;
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

/// Zeroed on clear/drop.
struct SecretString(String);

impl SecretString {
    fn as_str(&self) -> &str {
        &self.0
    }

    fn clear(&mut self) {
        zero_string(&mut self.0);
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        zero_string(&mut self.0);
    }
}

fn zero_string(value: &mut String) {
    // Best-effort overwrite before release; no log/Debug surface on SecretString.
    // SAFETY: `as_mut_vec` bytes are overwritten with 0 (still valid UTF-8) then clear().
    let bytes = unsafe { value.as_mut_vec() };
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` points at owned String buffer; write_volatile(0) is in-bounds.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    value.clear();
}

/// OIDC scope first, then legacy sign-in.
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

/// Local email from auth.json. No network.
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

/// gRPC-web: fixed32 percent + future reset.
fn parse_web_billing(body: &[u8], now_unix: i64) -> Option<UsageRow> {
    let payloads = grpc_web_data_frames(body);
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
            if (UNIX_TS_MIN..=UNIX_TS_MAX).contains(&raw) {
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
    // Prefer path [1,5,1] reset (CodexBar capture); else earliest future unix varint (#110 n5).
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

    // Period from full cycle length (start→reset), not remaining distance — remaining flips near
    // end-of-cycle (#115). Start: prefer [1,4,1], else [1,8,2,1]. No start → Month (stable).
    let period = period_from_scan(&scan, resets_at);
    UsageRow::checked(period, used_percent, resets_at)
}

fn period_from_scan(scan: &ProtobufScan, resets_at: i64) -> Period {
    let start = cycle_start(scan, resets_at);
    match start {
        Some(start) if start < resets_at => {
            period_from_cycle_length(resets_at.saturating_sub(start))
        }
        // Stable fallback: never reclassify solely because wall-clock advanced.
        _ => Period::Month,
    }
}

/// Billing window start from the live GetGrokCreditsConfig shape.
fn cycle_start(scan: &ProtobufScan, resets_at: i64) -> Option<i64> {
    let mut preferred = None;
    let mut any = None;
    for field in &scan.varints {
        let Ok(raw) = i64::try_from(field.value) else {
            continue;
        };
        if !(UNIX_TS_MIN..=UNIX_TS_MAX).contains(&raw) || raw >= resets_at {
            continue;
        }
        any = Some(any.map_or(raw, |prev: i64| prev.max(raw)));
        if field.path.as_slice() == [1, 4, 1] || field.path.as_slice() == [1, 8, 2, 1] {
            preferred = Some(preferred.map_or(raw, |prev: i64| prev.max(raw)));
        }
    }
    preferred.or(any)
}

/// Classify by full cycle length (CodexBar prefers duration over remaining when known).
/// 4–12 day windows → Week; everything else → Month (CLI is monthly-named).
fn period_from_cycle_length(seconds: i64) -> Period {
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

/// fixed32 + varint leaves. `None` on wire 3/4/6/7 (no 1-byte advance; #109).
fn scan_protobuf(data: &[u8], depth: u8, path: &[u64]) -> Option<ProtobufScan> {
    let mut scan = ProtobufScan::default();
    let mut index = 0;
    let mut order = 0usize;
    while index < data.len() {
        let key = read_varint(data, &mut index)?;
        if key == 0 {
            return None;
        }
        let field_number = key >> 3;
        let wire_type = key & 0x07;
        let mut field_path = path.to_vec();
        field_path.push(field_number);
        match wire_type {
            0 => {
                let value = read_varint(data, &mut index)?;
                scan.varints.push(VarintField {
                    path: field_path,
                    value,
                });
            }
            1 => {
                if index + 8 > data.len() {
                    return None;
                }
                index += 8;
            }
            2 => {
                let length = read_varint(data, &mut index)?;
                let length = length as usize;
                if index.saturating_add(length) > data.len() {
                    return None;
                }
                let nested = &data[index..index + length];
                index += length;
                if depth < 4 {
                    scan.merge(scan_protobuf(nested, depth + 1, &field_path)?);
                }
            }
            5 => {
                if index + 4 > data.len() {
                    return None;
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
        if shift == 63 && byte > 1 {
            return None;
        }
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
        // Live GetGrokCreditsConfig capture (percent + weekly reset).
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
    fn live_weekly_period_stable_near_cycle_end() {
        // Same capture: start [1,4,1]=1784266654, reset [1,5,1]=1784871454 (exactly 7 days).
        // Remaining ~1 day would flip the old remaining-distance heuristic to Month.
        let body = hex_literal(
            "00000000560a540d0000004112001a00220c089effe6d20610f894f79f022a0c\
             089ef48bd30610f894f79f023a0708021500000041421e0802120c089effe6d2\
             0610f894f79f021a0c089ef48bd30610f894f79f02580162006801800000000f\
             677270632d7374617475733a300d0a",
        );
        let near_end = 1_784_871_454 - 86_400;
        let window = parse_web_billing(&body, near_end).unwrap();
        assert_eq!(window.period, Period::Week);
        assert_eq!(window.resets_at_unix, 1_784_871_454);
    }

    #[test]
    fn period_from_cycle_length_matches_week_and_month_bands() {
        assert_eq!(period_from_cycle_length(7 * 86_400), Period::Week);
        assert_eq!(period_from_cycle_length(30 * 86_400), Period::Month);
        assert_eq!(period_from_cycle_length(2 * 86_400), Period::Month);
    }

    #[test]
    fn period_defaults_month_without_cycle_start() {
        // fixed32 percent at path [1] + future reset only (no start) → stable Month.
        let mut payload = Vec::new();
        // field 1 wire 5 (fixed32): key=(1<<3)|5=0x0d, bits of 12.0f32
        payload.push(0x0d);
        payload.extend_from_slice(&12.0f32.to_le_bytes());
        // field 5 nested: key=(5<<3)|2=0x2a, then field 1 varint reset
        let reset: u64 = 1_900_000_000;
        let mut nested = Vec::new();
        nested.push(0x08); // field 1 wire 0
        write_varint(&mut nested, reset);
        payload.push(0x2a);
        write_varint(&mut payload, nested.len() as u64);
        payload.extend_from_slice(&nested);

        let mut body = vec![0x00]; // flags
        let len = (payload.len() as u32).to_be_bytes();
        body.extend_from_slice(&len);
        body.extend_from_slice(&payload);
        // trailer grpc-status:0
        let trailer = b"grpc-status:0\r\n";
        body.push(0x80);
        body.extend_from_slice(&(trailer.len() as u32).to_be_bytes());
        body.extend_from_slice(trailer);

        let early = parse_web_billing(&body, 1_800_000_000).unwrap();
        let late = parse_web_billing(&body, 1_899_000_000).unwrap();
        assert_eq!(early.period, Period::Month);
        assert_eq!(late.period, Period::Month);
        assert_eq!(early.period, late.period);
    }

    #[test]
    fn monthly_cycle_length_labels_month() {
        // start + reset 30 days apart with percent.
        let start: u64 = 1_800_000_000;
        let reset: u64 = start + 30 * 86_400;
        let mut inner = Vec::new();
        // field 1 fixed32 = 40%
        inner.push(0x0d);
        inner.extend_from_slice(&40.0f32.to_le_bytes());
        // field 4 nested start
        let mut f4 = Vec::new();
        f4.push(0x08);
        write_varint(&mut f4, start);
        inner.push(0x22); // (4<<3)|2
        write_varint(&mut inner, f4.len() as u64);
        inner.extend_from_slice(&f4);
        // field 5 nested reset
        let mut f5 = Vec::new();
        f5.push(0x08);
        write_varint(&mut f5, reset);
        inner.push(0x2a); // (5<<3)|2
        write_varint(&mut inner, f5.len() as u64);
        inner.extend_from_slice(&f5);

        let mut msg = Vec::new();
        msg.push(0x0a); // field 1 length-delimited
        write_varint(&mut msg, inner.len() as u64);
        msg.extend_from_slice(&inner);

        let mut body = vec![0x00];
        body.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        body.extend_from_slice(&msg);
        let trailer = b"grpc-status:0\r\n";
        body.push(0x80);
        body.extend_from_slice(&(trailer.len() as u32).to_be_bytes());
        body.extend_from_slice(trailer);

        // Near end of cycle: remaining ~1 day; cycle length still 30 → Month.
        let window = parse_web_billing(&body, (reset as i64) - 86_400).unwrap();
        assert_eq!(window.period, Period::Month);
        assert!((window.used_percent - 40.0).abs() < 0.01);
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
    fn rejects_unframed_and_truncated_protobuf() {
        assert!(parse_web_billing(&[0x08, 0x01], 1_800_000_000).is_none());
        for body in [
            &[0x08, 0x80][..],
            &[0x09, 0x00][..],
            &[0x0a, 0x02, 0x08][..],
            &[0x0d, 0x00][..],
        ] {
            assert!(scan_protobuf(body, 0, &[]).is_none());
        }
    }

    #[test]
    fn rejects_overflowing_tenth_varint_byte() {
        let mut index = 0;
        let mut bytes = vec![0xff; 9];
        bytes.push(0x02);
        assert!(read_varint(&bytes, &mut index).is_none());
    }

    #[test]
    fn merge_preserves_cli_error_category() {
        for (cli, web, want_kind, must_contain) in [
            (
                std::io::Error::new(std::io::ErrorKind::NotFound, "Grok CLI unavailable"),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Grok web billing response unusable",
                ),
                std::io::ErrorKind::NotFound,
                &["CLI", "web"][..],
            ),
            (
                std::io::Error::new(std::io::ErrorKind::TimedOut, "provider RPC timed out"),
                std::io::Error::other("Grok web billing failed: connection refused"),
                std::io::ErrorKind::TimedOut,
                &[][..],
            ),
            (
                std::io::Error::other("provider RPC returned an error"),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Grok web billing response unusable",
                ),
                std::io::ErrorKind::Other,
                &["provider RPC returned an error"][..],
            ),
        ] {
            let merged = merge_cli_web_errors(cli, web);
            assert_eq!(merged.kind(), want_kind);
            let msg = merged.to_string();
            for needle in must_contain {
                assert!(msg.contains(needle), "missing `{needle}` in {msg}");
            }
            assert!(!msg.contains("Bearer"), "must not leak credentials: {msg}");
        }
    }

    #[test]
    fn empty_cli_maps_to_unusable_when_web_fails() {
        let err = finalize_after_web(
            None,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Grok web billing response unusable",
            ),
        );
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("CLI billing unusable"));
    }

    #[test]
    fn sanitize_strips_bearer_shaped_messages() {
        let dirty = std::io::Error::other("Authorization: Bearer eyJhbGciOi.x.y leaked");
        assert_eq!(sanitize_error_message(&dirty), "failed");
        let clean = std::io::Error::new(std::io::ErrorKind::NotFound, "Grok CLI unavailable");
        assert_eq!(sanitize_error_message(&clean), "Grok CLI unavailable");
    }

    #[test]
    fn zero_string_overwrites_then_clears() {
        let mut secret = String::from("super-secret-token-value");
        zero_string(&mut secret);
        assert!(secret.is_empty());
        // clear() after volatile zeros; capacity may remain but contents empty.
        assert_eq!(secret.len(), 0);

        // SecretString::clear is the same zero_string path on the wrapper.
        let mut wrapped = SecretString(String::from("token-abc"));
        assert_eq!(wrapped.as_str(), "token-abc");
        wrapped.clear();
        assert_eq!(wrapped.as_str(), "");
    }

    fn write_varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn hex_literal(hex: &str) -> Vec<u8> {
        let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..cleaned.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).unwrap())
            .collect()
    }
}
