//! Property tests for the fetch guard URL checks.
//!
//! docs/implementation/04 §Property tests: an allowed URL stays allowed
//! through zero to three allowlisted redirects, and private, loopback,
//! link-local, and metadata forms always fail. The checks here run at the
//! synchronous `check` level; the wiremock suite covers the network path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use proptest::prelude::*;
use url::Url;

use devdocs_ingest::guard::{FetchGuard, GuardError};
use devdocs_ingest::resolver::ip_allowed;
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
        any::<[u8; 4]>().prop_map(|o| IpAddr::V4(Ipv4Addr::from(o))),
        any::<[u8; 16]>().prop_map(|o| IpAddr::V6(Ipv6Addr::from(o))),
    ]
}

/// Any IP address the blocklist admits.
fn allowed_ip() -> impl Strategy<Value = IpAddr> {
    any_ip().prop_filter("must be allowed", |ip| ip_allowed(ip, false))
}

/// Any IP address the blocklist refuses.
///
/// The addresses come from an explicit enumeration of the documented
/// blocked ranges, not from `ip_allowed` itself: if `ip_allowed` starts
/// admitting part of a range, the generated inputs keep covering it and
/// the property fails loudly instead of silently losing coverage
/// (docs/implementation/09 §Network guard).
const BLOCKED_V4: &[(std::net::Ipv4Addr, u8)] = &[
    (Ipv4Addr::UNSPECIFIED, 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// The blocked v6 ranges, as `(base, host-bit count)` pairs.
const BLOCKED_V6: &[(std::net::Ipv6Addr, u32)] = &[
    (Ipv6Addr::UNSPECIFIED, 0),
    (Ipv6Addr::LOCALHOST, 0),
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 121),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 118),
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 120),
    (Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 96),
    (Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 112),
    (Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 0), 64),
];

fn blocked_ip() -> impl Strategy<Value = IpAddr> {
    let v4 = BLOCKED_V4.iter().map(|&(base, prefix)| {
        any::<u32>()
            .prop_map(move |x| {
                let host = if prefix == 0 {
                    0
                } else {
                    x & (u32::MAX >> prefix)
                };
                IpAddr::V4(Ipv4Addr::from(u32::from(base) | host))
            })
            .boxed()
    });
    let v6 = BLOCKED_V6.iter().map(|&(base, host_bits)| {
        any::<u128>()
            .prop_map(move |x| {
                let mask = if host_bits == 0 {
                    0
                } else {
                    u128::MAX >> (128 - host_bits)
                };
                IpAddr::V6(Ipv6Addr::from(u128::from(base) | (x & mask)))
            })
            .boxed()
    });
    proptest::strategy::Union::new(v4.chain(v6))
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
