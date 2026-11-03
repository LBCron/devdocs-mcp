//! Wiremock-backed tests for the fetch guard network path.
//!
//! docs/implementation/04 §Fixtures: wiremock servers bind `127.0.0.1` on
//! an ephemeral port. The blocklist refuses loopback, so every server in
//! this file runs behind the documented test-double escape hatch
//! ([`FetchGuard::with_loopback_allowed_for_tests`]); production paths
//! never set it, and the guard's own unit tests pin that.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

use devdocs_ingest::guard::{FetchGuard, GuardError};
use devdocs_model::config::FetchConfig;

/// A guard with the given overrides and the test-double exception on.
fn test_guard(max_bytes: u64, timeout_ms: u64, max_redirects: u32) -> FetchGuard {
    FetchGuard::from_fetch_config(&FetchConfig {
        concurrency: 4,
        timeout_ms,
        max_bytes,
        max_redirects,
        user_agent: String::from("devdocs/0.2.0 (+https://github.com/OWNER/devdocs-mcp)"),
    })
    .with_loopback_allowed_for_tests(true)
}

/// Mounts `path → 302 Location: location` on the server.
async fn mount_redirect(server: &MockServer, from: &str, location: &str) {
    Mock::given(path(from))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", location))
        .mount(server)
        .await;
}

/// Mounts `path → 200 body` on the server.
async fn mount_ok(server: &MockServer, at: &str, body: &str) {
    Mock::given(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn get_serves_a_plain_response() {
    let server = MockServer::start().await;
    mount_ok(&server, "/llms.txt", "index body").await;

    let url = format!("{}/llms.txt", server.uri());
    let url = url::Url::parse(&url).unwrap();
    let body = test_guard(1024, 5_000, 3).get(&url).await.unwrap();
    assert_eq!(body.as_ref(), b"index body");
}

#[tokio::test]
async fn get_follows_a_two_hop_redirect_chain() {
    let server = MockServer::start().await;
    mount_redirect(&server, "/a", "/b").await;
    mount_redirect(&server, "/b", "/c").await;
    mount_ok(&server, "/c", "final").await;

    let url = url::Url::parse(&format!("{}/a", server.uri())).unwrap();
    let body = test_guard(1024, 5_000, 3).get(&url).await.unwrap();
    assert_eq!(body.as_ref(), b"final");
}

#[tokio::test]
async fn get_refuses_a_chain_past_the_limit() {
    let server = MockServer::start().await;
    // Three hops with limit 2: the third follow exceeds the policy.
    mount_redirect(&server, "/a", "/b").await;
    mount_redirect(&server, "/b", "/c").await;
    mount_redirect(&server, "/c", "/d").await;
    mount_ok(&server, "/d", "unreachable").await;

    let url = url::Url::parse(&format!("{}/a", server.uri())).unwrap();
    let error = test_guard(1024, 5_000, 2).get(&url).await.unwrap_err();
    assert!(matches!(error, GuardError::RedirectLimit { .. }), "{error}");
    assert!(error.is_refusal());
}

#[tokio::test]
async fn get_refuses_a_redirect_into_a_blocked_address() {
    let server = MockServer::start().await;
    // The hop target is a documentation-range IP literal, so the redirect
    // policy's URL check refuses before any connection is attempted.
    mount_redirect(&server, "/a", "https://203.0.113.9/llms.txt").await;

    let url = url::Url::parse(&format!("{}/a", server.uri())).unwrap();
    let error = test_guard(1024, 5_000, 3).get(&url).await.unwrap_err();
    assert!(
        matches!(error, GuardError::BlockedAddress { .. }),
        "expected BlockedAddress, got {error}"
    );
    assert!(error.is_refusal());
}

#[tokio::test]
async fn get_refuses_a_redirect_with_userinfo() {
    let server = MockServer::start().await;
    mount_redirect(&server, "/a", "https://agent@example.test/llms.txt").await;

    let url = url::Url::parse(&format!("{}/a", server.uri())).unwrap();
    let error = test_guard(1024, 5_000, 3).get(&url).await.unwrap_err();
    assert!(
        matches!(error, GuardError::Userinfo { .. }),
        "expected Userinfo, got {error}"
    );
}

#[tokio::test]
async fn get_enforces_the_byte_cap() {
    let server = MockServer::start().await;
    Mock::given(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 256 * 1024]))
        .mount(&server)
        .await;

    let url = url::Url::parse(&format!("{}/big", server.uri())).unwrap();
    let error = test_guard(64 * 1024, 5_000, 3).get(&url).await.unwrap_err();
    match error {
        GuardError::BodyTooLarge { limit, .. } => assert_eq!(limit, 64 * 1024),
        other => panic!("expected BodyTooLarge, got {other}"),
    }
}

#[tokio::test]
async fn get_times_out_on_a_slow_response() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let url = url::Url::parse(&format!("{}/slow", server.uri())).unwrap();
    let error = test_guard(1024, 300, 3).get(&url).await.unwrap_err();
    assert!(matches!(error, GuardError::Timeout), "{error}");
    assert!(!error.is_refusal());
}

#[tokio::test]
async fn get_reports_an_http_error_status() {
    let server = MockServer::start().await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let url = url::Url::parse(&format!("{}/missing", server.uri())).unwrap();
    let error = test_guard(1024, 5_000, 3).get(&url).await.unwrap_err();
    match error {
        GuardError::Http { status, .. } => assert_eq!(status.as_u16(), 404),
        other => panic!("expected Http, got {other}"),
    }
}

#[tokio::test]
async fn get_serves_an_ipv6_literal_url() {
    // Wiremock binds 127.0.0.1 by default; bind a second server on the
    // IPv6 loopback so the fetch path exercises a bracketed IPv6 host
    // end to end (the resolver must strip the brackets before lookup).
    let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
    let server = MockServer::builder().listener(listener).start().await;
    mount_ok(&server, "/llms.txt", "ipv6 body").await;

    let url = url::Url::parse(&format!("{}/llms.txt", server.uri())).unwrap();
    assert!(url.host_str().unwrap_or_default().starts_with('['));
    let body = test_guard(1024, 5_000, 3).get(&url).await.unwrap();
    assert_eq!(body.as_ref(), b"ipv6 body");
}
