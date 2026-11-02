//! Property tests for the fetch guard URL checks.
//!
//! docs/implementation/04 §Property tests: an allowed URL stays allowed
//! through zero to three allowlisted redirects, and private, loopback,
//! link-local, and metadata forms always fail. The checks here run at the
//! synchronous `check` level; the wiremock suite covers the network path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::IpAddr;

use proptest::prelude::*;
use url::Url;

use devdocs_ingest::guard::{FetchGuard, GuardError, ip_allowed};
use devdocs_model::config::FetchConfig;

/// A guard with the documented defaults.
fn guard() -> FetchGuard {
    FetchGuard::from_fetch_config(&FetchConfig {
        concurrency: 4,
        timeout_ms: 15_000,
        max_bytes: 20 * 1024 * 1024,
        max_redirects: 3,
        user_agent: String::from("devdocs/0.2.0 (+https://github.com/OWNER/devdocs-mcp)"),
    })
}

/// Any IP address, v4 or v6.
fn any_ip() -> impl Strategy<Value = IpAddr> {
    prop_oneof![
        any::<[u8; 4]>().prop_map(|o| IpAddr::V4(std::net::Ipv4Addr::from(o))),
        any::<[u8; 16]>().prop_map(|o| IpAddr::V6(std::net::Ipv6Addr::from(o))),
    ]
}

/// Any IP address the blocklist admits.
fn allowed_ip() -> impl Strategy<Value = IpAddr> {
    any_ip().prop_filter("must be allowed", |ip| ip_allowed(ip, false))
}

/// Any IP address the blocklist refuses.
fn blocked_ip() -> impl Strategy<Value = IpAddr> {
    any_ip().prop_filter("must be blocked", |ip| !ip_allowed(ip, false))
}

/// An https URL whose host is an allowed IP literal, with a default,
/// 80, or 443 port.
fn allowed_ip_url() -> impl Strategy<Value = Url> {
    (allowed_ip(), 0..4usize).prop_map(|(ip, port_choice)| {
        let host = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let port = ["", "", ":443", ":80"][port_choice];
        Url::parse(&format!("https://{host}{port}/docs/llms.txt")).unwrap()
    })
}

/// A plausible hostname; `check` is host-name agnostic, so these stay
/// allowed at the URL level.
fn allowed_hostname_url() -> impl Strategy<Value = Url> {
    ("[a-z]{3,10}", 0..2usize).prop_map(|(label, port_choice)| {
        let port = if port_choice == 0 { "" } else { ":443" };
        Url::parse(&format!("https://{label}.example.org{port}/docs/llms.txt")).unwrap()
    })
}

/// An allowlisted URL: an allowed IP literal or a hostname.
fn allowed_url() -> impl Strategy<Value = Url> {
    prop_oneof![allowed_ip_url().prop_map(|u| u), allowed_hostname_url()]
}

/// An allowed URL stays allowed through zero to three allowlisted
/// redirects: every hop passes `check` (docs/04 §Property tests).
#[test]
fn allowed_url_stays_allowed_through_zero_to_three_redirects() {
    proptest!(|(chain in prop::collection::vec(allowed_url(), 1..=4))| {
        let guard = guard();
        for hop in &chain {
            guard.check(hop).unwrap_or_else(|e| {
                panic!("chain hop {hop} refused: {e}")
            });
        }
    });
}

/// A blocked IP literal always fails `check`, in every form the URL parser
/// accepts (docs/04 §Property tests: private, loopback, link-local, and
/// metadata forms always fail).
#[test]
fn blocked_ip_forms_always_fail() {
    proptest!(|(ip in blocked_ip())| {
        let host = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let url = Url::parse(&format!("https://{host}/docs/llms.txt")).unwrap();
        match guard().check(&url) {
            Ok(()) => panic!("check allowed a blocked literal: {url}"),
            Err(GuardError::BlockedAddress { address, .. }) => prop_assert_eq!(address, ip),
            Err(other) => panic!("expected BlockedAddress for {url}, got {other:?}"),
        }
    });
}

/// Scheme, userinfo, and port invariants hold over arbitrary inputs.
#[test]
fn scheme_userinfo_port_invariants_hold() {
    proptest!(|(label in "[a-z]{3,10}", port in 0u16..=65535)| {
        let guard = guard();
        // Non-HTTPS schemes refuse.
        for scheme in ["http", "ftp", "file"] {
            let url = Url::parse(&format!("{scheme}://{label}.example.org/docs")).unwrap();
            match guard.check(&url) {
                Ok(()) => panic!("check allowed scheme {scheme}"),
                Err(GuardError::InvalidScheme { .. }) => {}
                Err(other) => panic!("expected InvalidScheme for {url}, got {other:?}"),
            }
        }
        // Userinfo refuses.
        let url = Url::parse(&format!("https://agent@{label}.example.org/docs")).unwrap();
        match guard.check(&url) {
            Ok(()) => panic!("check allowed userinfo"),
            Err(GuardError::Userinfo { .. }) => {}
            Err(other) => panic!("expected Userinfo for {url}, got {other:?}"),
        }
        // Ports outside 80/443 refuse; 80 and 443 pass.
        let url = Url::parse(&format!("https://{label}.example.org:{port}/")).unwrap();
        match guard.check(&url) {
            Ok(()) => assert!(port == 80 || port == 443, "port {port} must refuse"),
            Err(GuardError::BlockedPort { .. }) => {
                assert!(port != 80 && port != 443, "port {port} must pass");
            }
            Err(other) => panic!("expected BlockedPort for {url}, got {other:?}"),
        }
    });
}
