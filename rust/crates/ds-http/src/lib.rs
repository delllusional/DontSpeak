//! Blocking HTTP for downloads and JSON probes. Auth/schema/retry stay in callers.
//! Default `max_redirections(0)` (Authorization re-sent cross-origin). CDN GETs opt in.
//! Connect + read inactivity; optional `total_timeout` after connect.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

pub use attohttpc::{Method, RequestBuilder, Response, StatusCode, body};

/// Cap on preserved init diagnostics (paths stripped at source; this bounds text only).
const MAX_ROOTS_DIAGNOSTIC_LEN: usize = 256;
/// How many load-error contexts to include in a partial/empty diagnostic.
const MAX_ROOTS_ERROR_CONTEXTS: usize = 3;

struct Roots {
    /// OS store first, then the bundled Mozilla set. Duplicates are harmless — path
    /// building matches on subject/SPKI, not on store position.
    certs: Vec<rustls_pki_types::CertificateDer<'static>>,
    /// Empty/partial OS load note (#114).
    diagnostic: Option<String>,
}

/// Trust anchors once (workspace rustls leaves attohttpc's own root store empty).
/// OS store UNIONED with bundled Mozilla roots: the OS store alone is incomplete on a
/// fresh install, and the bundle alone would ignore enterprise/private CAs.
fn roots() -> &'static Roots {
    static ROOTS: OnceLock<Roots> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let loaded = rustls_native_certs::load_native_certs();
        // `Error::context` is path-free static phrase.
        let contexts: Vec<&str> = loaded.errors.iter().map(|e| e.context).collect();
        let diagnostic = describe_native_roots_init(loaded.certs.len(), &contexts);
        let mut certs = loaded.certs;
        let native_count = certs.len();
        certs.extend(webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned());
        if let Some(note) = diagnostic.as_deref() {
            log::warn!("tls {note}; bundled roots still available");
        }
        log::debug!(
            "tls trust anchors: {native_count} from OS store + {} bundled",
            certs.len() - native_count
        );
        Roots { certs, diagnostic }
    })
}

/// Empty/partial native roots note; `None` if healthy. Independent of log init.
pub fn native_roots_diagnostic() -> Option<&'static str> {
    roots().diagnostic.as_deref()
}

/// Pure empty/partial cert-load diagnose (#114). Path-free `error_contexts`.
/// `None` when ≥1 cert and no errors.
pub fn describe_native_roots_init(cert_count: usize, error_contexts: &[&str]) -> Option<String> {
    if cert_count > 0 && error_contexts.is_empty() {
        return None;
    }

    let mut msg = if cert_count == 0 {
        String::from("native TLS root store empty (0 certificates loaded)")
    } else {
        format!("native TLS root store partially loaded ({cert_count} certificates)")
    };

    if !error_contexts.is_empty() {
        msg.push_str("; load errors: ");
        let take = error_contexts.len().min(MAX_ROOTS_ERROR_CONTEXTS);
        for (i, ctx) in error_contexts.iter().take(take).enumerate() {
            if i > 0 {
                msg.push_str(", ");
            }
            push_sanitized(&mut msg, ctx);
        }
        let remaining = error_contexts.len().saturating_sub(take);
        if remaining > 0 {
            msg.push_str(&format!(", +{remaining} more"));
        }
    }

    clamp_diagnostic(&mut msg);
    Some(msg)
}

fn push_sanitized(out: &mut String, s: &str) {
    for ch in s.chars() {
        if ch.is_control() {
            out.push('?');
        } else {
            out.push(ch);
        }
    }
}

fn clamp_diagnostic(msg: &mut String) {
    if msg.len() <= MAX_ROOTS_DIAGNOSTIC_LEN {
        return;
    }
    let mut end = MAX_ROOTS_DIAGNOSTIC_LEN;
    while end > 0 && !msg.is_char_boundary(end) {
        end -= 1;
    }
    msg.truncate(end);
    msg.push('…');
}

/// Blocking request: budgets, native roots, no redirects (Auth not re-sent cross-origin).
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
        .max_redirections(0);
    if let Some(total) = total_timeout {
        builder = builder.timeout(total);
    }
    for cert in &roots().certs {
        builder = builder.add_root_certificate(cert.clone());
    }
    builder
}

/// Body with hard size cap.
pub fn read_bytes_limited(response: Response, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let mut response = response
        .error_for_status()
        .map_err(|error| std::io::Error::other(format!("HTTP request failed: {error}")))?;
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

/// UTF-8 body with hard size cap.
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

        let oversized =
            read_utf8_limited(get(&server.url("/large"), None).unwrap(), 4).unwrap_err();
        assert_eq!(oversized.kind(), std::io::ErrorKind::InvalidData);
        assert!(read_utf8_limited(get(&server.url("/failure"), None).unwrap(), 32).is_err());
    }

    // #108: max_redirections(0) must not follow 302 (Authorization leak surface).
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

    // #107: wall-clock total budget aborts a delayed response.
    #[test]
    fn total_timeout_aborts_slow_response() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/slow");
            then.status(200).delay(Duration::from_secs(2)).body("late");
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

    // The bundled Mozilla set must be in the anchors regardless of what the OS
    // store holds — a fresh Windows ships ~20 roots and lacks Amazon Root CA 1, which
    // every huggingface.co download chains to.
    #[test]
    fn trust_anchors_include_bundled_roots() {
        let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS;
        assert!(!bundled.is_empty(), "bundled root set must not be empty");
        let anchors = &roots().certs;
        assert!(
            anchors.len() >= bundled.len(),
            "anchors ({}) must include the {} bundled roots",
            anchors.len(),
            bundled.len()
        );
        for cert in bundled {
            assert!(
                anchors.contains(cert),
                "bundled root missing from the trust anchors"
            );
        }
    }

    // #114: pure helper — empty store without load errors.
    #[test]
    fn describe_empty_roots_without_errors() {
        let d = describe_native_roots_init(0, &[]).expect("empty store must diagnose");
        assert!(d.contains("empty"), "{d}");
        assert!(d.contains("0 certificates"), "{d}");
        assert!(!d.contains("load errors"), "{d}");
    }

    // #114: empty store + load errors include sanitized contexts.
    #[test]
    fn describe_empty_roots_with_errors() {
        let d =
            describe_native_roots_init(0, &["failed to open system store", "path\nwith\0controls"])
                .expect("empty+errors must diagnose");
        assert!(d.contains("empty"), "{d}");
        assert!(d.contains("failed to open system store"), "{d}");
        assert!(!d.contains('\n'), "{d}");
        assert!(!d.contains('\0'), "{d}");
        assert!(d.contains('?'), "{d}");
    }

    // #114: healthy load is silent.
    #[test]
    fn describe_healthy_roots_has_no_diagnostic() {
        assert!(describe_native_roots_init(42, &[]).is_none());
    }

    // #114: partial load (certs present but errors) is reported.
    #[test]
    fn describe_partial_roots_with_errors() {
        let d = describe_native_roots_init(3, &["failed to read PEM from file"])
            .expect("partial load must diagnose");
        assert!(d.contains("partially loaded"), "{d}");
        assert!(d.contains("3 certificates"), "{d}");
        assert!(d.contains("failed to read PEM from file"), "{d}");
    }

    // #114: many errors are capped; remainder counted.
    #[test]
    fn describe_roots_caps_error_contexts() {
        let contexts = ["a", "b", "c", "d", "e"];
        let d = describe_native_roots_init(0, &contexts).expect("empty must diagnose");
        assert!(d.contains("+2 more"), "{d}");
        assert!(d.contains('a') && d.contains('c'), "{d}");
        assert!(!d.contains(", d"), "{d}");
    }

    // #114: diagnostic text is length-bounded.
    #[test]
    fn describe_roots_bounds_diagnostic_length() {
        let long = "x".repeat(MAX_ROOTS_DIAGNOSTIC_LEN + 64);
        let d = describe_native_roots_init(0, &[&long]).expect("empty must diagnose");
        // Cap + ellipsis (ellipsis may be multi-byte).
        assert!(
            d.chars().count() <= MAX_ROOTS_DIAGNOSTIC_LEN + 1,
            "{}",
            d.len()
        );
        assert!(d.ends_with('…'), "{d}");
    }
}
