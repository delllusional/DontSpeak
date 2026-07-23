use std::io::Read;
use std::path::Path;
use std::time::Duration;

use ds_config::WiredClient;
use serde_json::Value;

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod grok;
pub(crate) mod hermes;
pub(crate) mod kimi;
pub(crate) mod qwen;
mod rpc;

#[derive(Debug)]
pub(crate) enum FetchError {
    Guarded,
    /// Provider rejected a stale or revoked token.
    Unauthorized,
    // Tests assert kinds; production branches on the credential variants only.
    Io(#[allow(dead_code)] std::io::Error),
}

impl From<std::io::Error> for FetchError {
    fn from(error: std::io::Error) -> Self {
        // Keychain clients distinguish refusal from other provider failures.
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            return Self::Unauthorized;
        }
        Self::Io(error)
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Credential probes: connect + body wall-clock.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_CREDENTIAL_BYTES: u64 = 1024 * 1024;

fn request(method: ds_http::Method, url: &str) -> std::io::Result<ds_http::RequestBuilder> {
    if !url
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage provider URL must use HTTPS",
        ));
    }
    Ok(ds_http::request(
        method,
        url,
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        Some(TOTAL_TIMEOUT),
    ))
}

/// Preserve 401/403 without exposing response bodies.
fn send_json<B: ds_http::body::Body>(
    builder: ds_http::RequestBuilder<B>,
) -> std::io::Result<Value> {
    let response = builder
        .send()
        .map_err(|error| std::io::Error::other(format!("provider request failed: {error}")))?;
    reject_unauthorized(response.status())?;
    let body = ds_http::read_utf8_limited(response, MAX_JSON_BYTES)?;
    serde_json::from_str(&body)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Message stays credential-free (statuses only).
fn reject_unauthorized(status: ds_http::StatusCode) -> std::io::Result<()> {
    if matches!(status.as_u16(), 401 | 403) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "provider rejected the credential (HTTP {})",
                status.as_u16()
            ),
        ));
    }
    Ok(())
}

fn read_json_file(path: &Path) -> std::io::Result<Value> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential file exceeds size limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "credential file exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn number_at(value: &Value, key: &str) -> Option<f64> {
    let raw = value.get(key)?;
    raw.as_f64()
        .or_else(|| raw.as_i64().map(|number| number as f64))
        .or_else(|| raw.as_str()?.trim().parse().ok())
}

fn integer_at(value: &Value, key: &str) -> Option<i64> {
    let raw = value.get(key)?;
    raw.as_i64()
        .or_else(|| raw.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| raw.as_str()?.trim().parse().ok())
}

/// Rfc3339; strips fractional seconds Anthropic sends.
fn rfc3339_timestamp(raw: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(date) = time::OffsetDateTime::parse(raw, &Rfc3339) {
        return Some(date.unix_timestamp());
    }
    // Drop fractional seconds before zone: ".707736+00:00" / ".707Z".
    let dot = raw.find('.')?;
    let tail = &raw[dot + 1..];
    let zone_at = tail.find(['Z', '+', '-'])?;
    let cleaned = format!("{}{}", &raw[..dot], &tail[zone_at..]);
    time::OffsetDateTime::parse(&cleaned, &Rfc3339)
        .ok()
        .map(|date| date.unix_timestamp())
}

fn resolve_binary(client: WiredClient, paths: &ds_config::Paths) -> Option<std::path::PathBuf> {
    ds_config::resolve_client_binary(client, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_requests_require_https() {
        let error = request(ds_http::Method::GET, "http://provider.test/usage")
            .err()
            .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(request(ds_http::Method::GET, "HTTPS://provider.test/usage").is_ok());
    }

    /// Loopback only; `request()` refuses plain HTTP, so build the probe directly.
    fn probe(url: &str) -> ds_http::RequestBuilder {
        ds_http::request(
            ds_http::Method::GET,
            url,
            CONNECT_TIMEOUT,
            READ_TIMEOUT,
            Some(TOTAL_TIMEOUT),
        )
    }

    #[test]
    fn rejected_credential_becomes_an_unauthorized_fetch_error() {
        let server = httpmock::MockServer::start();
        for status in [401, 403] {
            let mut endpoint = server.mock(|when, then| {
                when.path("/usage");
                then.status(status)
                    .body(r#"{"error":{"message":"OAuth token has expired"}}"#);
            });
            let error = send_json(probe(&server.url("/usage"))).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert!(matches!(FetchError::from(error), FetchError::Unauthorized));
            endpoint.assert();
            endpoint.delete();
        }
    }

    #[test]
    fn successful_probe_still_parses_json() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.path("/usage");
            then.status(200).body(r#"{"five_hour":{"utilization":7}}"#);
        });
        let json = send_json(probe(&server.url("/usage"))).unwrap();
        assert_eq!(json["five_hour"]["utilization"], 7);
    }

    /// A JSON body on a 5xx must not read as usage data.
    #[test]
    fn server_errors_stay_io_errors() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.path("/usage");
            then.status(503).body(r#"{"five_hour":{"utilization":7}}"#);
        });
        let error = send_json(probe(&server.url("/usage"))).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(matches!(FetchError::from(error), FetchError::Io(_)));
    }
}
