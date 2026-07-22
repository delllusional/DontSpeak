use serde_json::Value;

use super::{
    FetchError, MAX_CREDENTIAL_BYTES, read_json_file, request, rfc3339_timestamp, send_json,
    string_at,
};
use crate::{Period, UsageRow};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Which copy a token came from — a rejected file token earns one keychain retry.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CredentialSource {
    File,
    Keychain,
}

/// `interactive` only from authorize FFI; implicit paths pass `false`.
pub(crate) fn fetch(
    paths: &ds_config::Paths,
    interactive: bool,
) -> Result<Vec<UsageRow>, FetchError> {
    let (credentials, source) = read_credentials(paths, interactive)?;
    let rows = usage_rows(&credentials);
    // Rotation/re-login leaves a revoked token in the file while the keychain copy
    // stays current — its `expiresAt` can't reveal that, only the 401 can.
    if matches!(rows, Err(FetchError::Unauthorized))
        && source == CredentialSource::File
        && let Some(keychain) = keychain_retry(probe_keychain(interactive))?
    {
        return usage_rows(&keychain);
    }
    rows
}

/// After a 401, retry readable keychain data, surface guarded access, or keep refusal.
fn keychain_retry(probe: KeychainProbe) -> Result<Option<Value>, FetchError> {
    match probe {
        KeychainProbe::Data(bytes) => Ok(credentials_from_bytes(bytes).ok()),
        KeychainProbe::ItemPresent => Err(FetchError::Guarded),
        KeychainProbe::Absent => Ok(None),
    }
}

fn usage_rows(credentials: &Value) -> Result<Vec<UsageRow>, FetchError> {
    let token = string_at(credentials, &["claudeAiOauth", "accessToken"])
        .or_else(|| string_at(credentials, &["claude_ai_oauth", "access_token"]))
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

/// `ItemPresent` → [`FetchError::Guarded`] (ACL blocks silent data read).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum KeychainProbe {
    Data(Vec<u8>),
    ItemPresent,
    Absent,
}

fn read_credentials(
    paths: &ds_config::Paths,
    interactive: bool,
) -> Result<(Value, CredentialSource), FetchError> {
    let file = read_json_file(&paths.claude_dir.join(".credentials.json"));
    resolve_credentials(file, now_unix_millis(), || probe_keychain(interactive))
}

/// Non-macOS keeps credentials in the file alone (Guarded unreachable).
fn probe_keychain(interactive: bool) -> KeychainProbe {
    #[cfg(target_os = "macos")]
    {
        keychain_probe(interactive)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = interactive;
        KeychainProbe::Absent
    }
}

/// A live file token wins; probe on file miss — and on a lapsed file token, since
/// Claude Code refreshes the keychain copy and can leave the file behind for days.
fn resolve_credentials(
    file: std::io::Result<Value>,
    now_ms: i64,
    probe: impl FnOnce() -> KeychainProbe,
) -> Result<(Value, CredentialSource), FetchError> {
    let no_keychain = match file {
        Ok(credentials) if !is_expired(&credentials, now_ms) => {
            return Ok((credentials, CredentialSource::File));
        }
        Ok(_) => FetchError::Unauthorized,
        Err(error) => FetchError::Io(error),
    };
    match probe() {
        KeychainProbe::Data(bytes) => {
            credentials_from_bytes(bytes).map(|value| (value, CredentialSource::Keychain))
        }
        KeychainProbe::ItemPresent => Err(FetchError::Guarded),
        KeychainProbe::Absent => Err(no_keychain),
    }
}

fn credentials_from_bytes(bytes: Vec<u8>) -> Result<Value, FetchError> {
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential payload exceeds size limit",
        )
        .into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
}

/// Absent `expiresAt` counts as live — staleness must be proven, not assumed. The
/// grace window keeps a fast local clock from condemning a good token.
fn is_expired(credentials: &Value, now_ms: i64) -> bool {
    const GRACE_MS: i64 = 5 * 60 * 1000;
    expires_at_ms(credentials).is_some_and(|expiry| expiry.saturating_add(GRACE_MS) <= now_ms)
}

/// Claude Code writes unix ms; a seconds-valued file is rescaled rather than read
/// as 1970 (which would condemn every token).
fn expires_at_ms(credentials: &Value) -> Option<i64> {
    /// Unix ms in 2001 — anything smaller is a seconds timestamp.
    const SECONDS_CEILING_MS: i64 = 1_000_000_000_000;
    let raw = credentials
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("expiresAt"))
        .or_else(|| {
            credentials
                .get("claude_ai_oauth")
                .and_then(|oauth| oauth.get("expires_at"))
        })?;
    let value = raw
        .as_i64()
        .or_else(|| raw.as_f64().map(|number| number as i64))
        .or_else(|| raw.as_str()?.trim().parse().ok())?;
    Some(if value.abs() < SECONDS_CEILING_MS {
        value.saturating_mul(1000)
    } else {
        value
    })
}

fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Only interactive data search may prompt (ACL dialog). Silent + attributes: no UI.
#[cfg(target_os = "macos")]
fn keychain_probe(interactive: bool) -> KeychainProbe {
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

    fn first_data(results: Vec<SearchResult>) -> Option<Vec<u8>> {
        results.into_iter().find_map(|result| match result {
            SearchResult::Data(bytes) => Some(bytes),
            _ => None,
        })
    }

    if let Ok(results) = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .load_data(true)
        .skip_authenticated_items(true)
        .search()
        && let Some(bytes) = first_data(results)
    {
        return KeychainProbe::Data(bytes);
    }

    if interactive
        && let Ok(results) = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .service(KEYCHAIN_SERVICE)
            .load_data(true)
            .search()
        && let Some(bytes) = first_data(results)
    {
        return KeychainProbe::Data(bytes);
    }

    let attributes = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .load_attributes(true)
        .limit(1)
        .search();
    match attributes {
        Ok(results) if !results.is_empty() => KeychainProbe::ItemPresent,
        _ => KeychainProbe::Absent,
    }
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

    // resolve_credentials table — synthetic probes, no live keychain.

    const NOW_MS: i64 = 1_784_600_000_000;

    fn miss() -> std::io::Result<Value> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no credential file",
        ))
    }

    fn file_token(expires_at_ms: i64) -> std::io::Result<Value> {
        Ok(serde_json::json!({
            "claudeAiOauth": { "accessToken": "file", "expiresAt": expires_at_ms }
        }))
    }

    /// Past the expiry *and* the grace window.
    fn expired_file_token() -> std::io::Result<Value> {
        file_token(NOW_MS - 60 * 60 * 1000)
    }

    fn token_of(resolved: &Result<(Value, CredentialSource), FetchError>) -> Option<&str> {
        let (value, _) = resolved.as_ref().ok()?;
        string_at(value, &["claudeAiOauth", "accessToken"])
    }

    #[test]
    fn readable_file_wins_and_probe_never_runs() {
        let resolved =
            resolve_credentials(Ok(serde_json::json!({"claudeAiOauth": {}})), NOW_MS, || {
                panic!("probe must not run when the file read succeeds")
            });
        assert!(resolved.is_ok());
        let live = resolve_credentials(file_token(NOW_MS + 1), NOW_MS, || {
            panic!("probe must not run while the file token is live")
        });
        assert_eq!(
            live.as_ref().map(|(_, source)| *source).ok(),
            Some(CredentialSource::File)
        );
    }

    #[test]
    fn expired_file_token_defers_to_the_keychain_copy() {
        let resolved = resolve_credentials(expired_file_token(), NOW_MS, || {
            KeychainProbe::Data(br#"{"claudeAiOauth":{"accessToken":"keychain"}}"#.to_vec())
        });
        assert_eq!(token_of(&resolved), Some("keychain"));
        assert_eq!(
            resolved.map(|(_, source)| source).ok(),
            Some(CredentialSource::Keychain)
        );
    }

    #[test]
    fn expired_file_token_without_a_keychain_is_unauthorized() {
        let resolved = resolve_credentials(expired_file_token(), NOW_MS, || KeychainProbe::Absent);
        assert!(matches!(resolved, Err(FetchError::Unauthorized)));
    }

    /// A fast local clock must not condemn a token that is still good.
    #[test]
    fn expiry_inside_the_grace_window_still_counts_as_live() {
        let resolved = resolve_credentials(file_token(NOW_MS - 60_000), NOW_MS, || {
            panic!("probe must not run for a token inside the grace window")
        });
        assert_eq!(token_of(&resolved), Some("file"));
    }

    #[test]
    fn string_and_seconds_expiries_are_read_like_millis() {
        let stale_string = Ok(serde_json::json!({
            "claude_ai_oauth": { "access_token": "file", "expires_at": "1784500000000" }
        }));
        assert!(matches!(
            resolve_credentials(stale_string, NOW_MS, || KeychainProbe::Absent),
            Err(FetchError::Unauthorized)
        ));
        // Seconds-valued (10 digits) and still in the future — not expired.
        let seconds = Ok(serde_json::json!({
            "claudeAiOauth": { "accessToken": "file", "expiresAt": NOW_MS / 1000 + 3600 }
        }));
        let resolved = resolve_credentials(seconds, NOW_MS, || {
            panic!("a future seconds-valued expiry must not read as 1970")
        });
        assert_eq!(token_of(&resolved), Some("file"));
    }

    #[test]
    fn refused_file_token_with_a_guarded_keychain_offers_authorize() {
        assert!(matches!(
            keychain_retry(KeychainProbe::ItemPresent),
            Err(FetchError::Guarded)
        ));
    }

    #[test]
    fn refused_file_token_retries_readable_and_keeps_refusal_otherwise() {
        let readable = keychain_retry(KeychainProbe::Data(
            br#"{"claudeAiOauth":{"accessToken":"keychain"}}"#.to_vec(),
        ));
        assert!(matches!(readable, Ok(Some(_))));
        assert!(matches!(keychain_retry(KeychainProbe::Absent), Ok(None)));
        let unreadable = keychain_retry(KeychainProbe::Data(b"not json".to_vec()));
        assert!(matches!(unreadable, Ok(None)));
    }

    #[test]
    fn file_miss_with_present_item_is_guarded() {
        let resolved = resolve_credentials(miss(), NOW_MS, || KeychainProbe::ItemPresent);
        assert!(matches!(resolved, Err(FetchError::Guarded)));
    }

    #[test]
    fn file_miss_with_absent_item_keeps_the_file_error() {
        let resolved = resolve_credentials(miss(), NOW_MS, || KeychainProbe::Absent);
        match resolved {
            Err(FetchError::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
            _ => panic!("expected the original file error"),
        }
    }

    #[test]
    fn probe_data_parses_or_maps_to_invalid_data() {
        let ok = resolve_credentials(miss(), NOW_MS, || {
            KeychainProbe::Data(br#"{"claudeAiOauth":{"accessToken":"t"}}"#.to_vec())
        });
        assert_eq!(token_of(&ok), Some("t"));

        for bad in [
            b"not json".to_vec(),
            vec![b'x'; (MAX_CREDENTIAL_BYTES + 1) as usize],
        ] {
            match resolve_credentials(miss(), NOW_MS, || KeychainProbe::Data(bad)) {
                Err(FetchError::Io(error)) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
                }
                _ => panic!("expected InvalidData"),
            }
        }
    }
}
