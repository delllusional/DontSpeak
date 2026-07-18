//! Small blocking-HTTP policy shared by model downloads and bounded JSON probes.
//! Provider URLs, authentication, response schemas, and retry decisions stay in
//! their owning crates.
//!
//! # Redirects
//! `request` disables redirect following (`max_redirections(0)`). attohttpc does
//! not strip `Authorization` on cross-origin redirects; credential-bearing probes
//! must not hop. Callers that need redirects (e.g. public CDN GETs with no auth)
//! must opt in explicitly via `.follow_redirects(true).max_redirections(n)`.
//!
//! # Timeouts
//! Connect + per-read inactivity always apply. Optional `total_timeout` is a
//! wall-clock budget after TCP connect (attohttpc's `.timeout`). Bounded probes
//! pass `Some(...)`; large model downloads pass `None` so multi-minute transfers
//! only abort on connect failure or read stall — not on overall duration.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

pub use attohttpc::{Method, RequestBuilder, Response, body};

/// OS trust store once. `attohttpc`'s selected rustls feature does not populate
/// its root store under the workspace feature combination, so inject native roots.
///
/// Partial load failures (`.errors`) are intentionally silent: this crate has no
/// logger yet; fail-closed still holds (HTTPS fails if needed roots are missing).
fn os_root_certs() -> &'static [rustls_pki_types::CertificateDer<'static>] {
    static ROOTS: OnceLock<Vec<rustls_pki_types::CertificateDer<'static>>> = OnceLock::new();
    ROOTS.get_or_init(|| rustls_native_certs::load_native_certs().certs)
}

/// Create a blocking request with explicit connect/read inactivity budgets,
/// optional wall-clock total budget, native TLS roots, and no redirect following.
///
/// `total_timeout`: `Some` for bounded/credential-bearing probes; `None` for large
/// downloads that intentionally rely only on connect + read inactivity.
pub fn request(
    method: Method,
    url: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
    total_timeout: Option<Duration>,
) -> RequestBuilder {
    let mut builder = RequestBuilder::new(method, url)
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        // Default 5 hops + Authorization re-sent cross-origin — refuse all hops.
        .max_redirections(0);
    if let Some(total) = total_timeout {
        builder = builder.timeout(total);
    }
    for cert in os_root_certs() {
        builder = builder.add_root_certificate(cert.clone());
    }
    builder
}

/// Read a successful response body with a hard size cap.
pub fn read_bytes_limited(response: Response, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut response = response
        .error_for_status()
        .map_err(|error| std::io::Error::other(format!("HTTP request failed: {error}")))?;
    // Hard bound is `take(limit)` (max_bytes + 1); Vec grows as needed up to that.
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("HTTP body read failed: {error}")))?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP response exceeds size limit",
        ));
    }
    Ok(bytes)
}

/// Read a successful response into a UTF-8 string without allowing a small JSON
/// endpoint to allocate an unbounded body.
pub fn read_utf8_limited(response: Response, max_bytes: usize) -> std::io::Result<String> {
    let bytes = read_bytes_limited(response, max_bytes)?;
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn get(url: &str, total: Option<Duration>) -> attohttpc::Result<Response> {
        request(
            Method::GET,
            url,
            Duration::from_secs(2),
            Duration::from_secs(2),
            total,
        )
        .send()
    }

    #[test]
    fn reads_bounded_utf8_response() {
        let server = httpmock::MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/usage");
            then.status(200).body("{\"ok\":true}");
        });
        assert_eq!(
            read_utf8_limited(get(&server.url("/usage"), None).unwrap(), 32).unwrap(),
            "{\"ok\":true}"
        );
        endpoint.assert();
    }

    #[test]
    fn rejects_oversized_and_non_successful_responses() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/large");
            then.status(200).body("12345");
        });
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/failure");
            then.status(503).body("unavailable");
        });

        let oversized = read_utf8_limited(get(&server.url("/large"), None).unwrap(), 4).unwrap_err();
        assert_eq!(oversized.kind(), std::io::ErrorKind::InvalidData);
        assert!(read_utf8_limited(get(&server.url("/failure"), None).unwrap(), 32).is_err());
    }

    /// #108: default `max_redirections(0)` must not follow a 302 (Authorization leak surface).
    #[test]
    fn does_not_follow_redirects_by_default() {
        let server = httpmock::MockServer::start();
        let target = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/secret");
            then.status(200).body("leaked");
        });
        let redirect = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/start");
            then.status(302)
                .header("Location", server.url("/secret"))
                .body("");
        });

        let result = get(&server.url("/start"), None);
        // attohttpc: first hop increments redirections past max 0 → TooManyRedirections.
        match result {
            Err(err) => assert!(
                matches!(err.kind(), attohttpc::ErrorKind::TooManyRedirections),
                "expected TooManyRedirections, got {err:?}"
            ),
            Ok(resp) => panic!(
                "redirect must not yield a successful hop; status={}",
                resp.status()
            ),
        }
        redirect.assert();
        assert_eq!(
            target.calls(),
            0,
            "redirect target must not be requested under max_redirections(0)"
        );
    }

    /// #107: wall-clock total budget aborts a deliberately delayed response.
    #[test]
    fn total_timeout_aborts_slow_response() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/slow");
            then.status(200)
                .delay(Duration::from_secs(2))
                .body("late");
        });

        let started = Instant::now();
        let result = get(&server.url("/slow"), Some(Duration::from_millis(200)));
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "expected total timeout error, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "total timeout must fire before the mock delay completes: {elapsed:?}"
        );
    }
}
