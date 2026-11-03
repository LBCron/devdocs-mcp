//! Unit tests for the fetch guard: the URL policy, the blocklist exit, the
//! transport error mapping, and the error-type contracts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{FetchGuard, GuardError};
use devdocs_model::config::FetchConfig;
use url::Url;

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

/// Parses one URL in the test body.
fn url(raw: &str) -> Url {
    Url::parse(raw).unwrap()
}

/// HTTPS with no userinfo and a default or HTTP(S) port passes.
#[test]
fn check_accepts_a_plain_https_url() {
    guard()
        .check(&url("https://nextjs.org/docs/llms.txt"))
        .unwrap();
    guard()
        .check(&url("https://nextjs.org:443/docs/llms.txt"))
        .unwrap();
    guard()
        .check(&url("https://nextjs.org:80/docs/llms.txt"))
        .unwrap();
}

/// Every non-HTTPS scheme refuses before DNS.
#[test]
fn check_refuses_other_schemes() {
    for raw in [
        "http://nextjs.org/docs/llms.txt",
        "ftp://nextjs.org/docs/llms.txt",
        "file:///etc/passwd",
        "data:text/plain,hello",
    ] {
        let error = guard().check(&url(raw)).unwrap_err();
        assert!(matches!(error, GuardError::InvalidScheme { .. }), "{raw}");
    }
}

/// Userinfo in the URL refuses.
#[test]
fn check_refuses_userinfo() {
    for raw in [
        "https://user@nextjs.org/docs/llms.txt",
        "https://user:pass@nextjs.org/docs/llms.txt",
        "https://:pass@nextjs.org/docs/llms.txt",
    ] {
        let error = guard().check(&url(raw)).unwrap_err();
        assert!(matches!(error, GuardError::Userinfo { .. }), "{raw}");
    }
}

/// Non-HTTP(S) ports refuse.
#[test]
fn check_refuses_non_http_ports() {
    for raw in [
        "https://nextjs.org:22/docs/llms.txt",
        "https://nextjs.org:8080/docs/llms.txt",
        "https://nextjs.org:9200/docs/llms.txt",
    ] {
        let error = guard().check(&url(raw)).unwrap_err();
        assert!(matches!(error, GuardError::BlockedPort { .. }), "{raw}");
    }
}

/// IP-literal hosts on the blocklist refuse synchronously in `check`.
#[test]
fn check_refuses_blocked_ip_literals() {
    let mut failures: Vec<&str> = Vec::new();
    for raw in [
        "https://10.1.2.3/docs/llms.txt",
        "https://172.16.0.9/docs/llms.txt",
        "https://192.168.1.1/docs/llms.txt",
        "https://127.0.0.1/llms.txt",
        "https://169.254.169.254/computeMetadata/v1/",
        "https://192.0.2.1/llms.txt",
        "https://198.51.100.7/llms.txt",
        "https://203.0.113.9/llms.txt",
        "https://0.0.0.0/llms.txt",
        "https://224.0.0.1/llms.txt",
        "https://255.255.255.255/llms.txt",
        "https://100.64.0.1/llms.txt",
        "https://[::1]/llms.txt",
        "https://[fc00::1]/llms.txt",
        "https://[fe80::1]/llms.txt",
        "https://[ff02::1]/llms.txt",
        "https://[2001:db8::1]/llms.txt",
        "https://[::ffff:127.0.0.1]/llms.txt",
        "https://[64:ff9b::192.0.2.1]/llms.txt",
    ] {
        match guard().check(&url(raw)) {
            Ok(()) => failures.push(raw),
            Err(error) => assert!(
                matches!(error, GuardError::BlockedAddress { .. }),
                "{raw} → {error}"
            ),
        }
    }
    assert!(
        failures.is_empty(),
        "passed check but must block: {failures:?}"
    );
}

/// Public IP literals pass `check`; the blocklist does not over-refuse.
#[test]
fn check_accepts_public_ip_literals() {
    for raw in [
        "https://1.1.1.1/llms.txt",
        "https://8.8.8.8/llms.txt",
        "https://[2606:4700::1111]/llms.txt",
    ] {
        guard().check(&url(raw)).unwrap();
    }
}

/// The production constructor keeps the loopback escape off.
#[test]
fn production_guard_refuses_loopback() {
    let error = guard()
        .check(&url("https://127.0.0.1/llms.txt"))
        .unwrap_err();
    assert!(matches!(error, GuardError::BlockedAddress { .. }));
}

/// Refusals report as refusals; transport failures do not.
#[test]
fn refusals_classify_for_the_error_mapping() {
    assert!(
        guard()
            .check(&url("https://10.0.0.1/x"))
            .unwrap_err()
            .is_refusal()
    );
    assert!(!GuardError::Timeout.is_refusal());
    assert!(
        !GuardError::Dns {
            host: String::from("x"),
            source: Box::new(std::io::Error::other("down")),
        }
        .is_refusal()
    );
}

/// The pinned resolver reports a typed refusal sentinel when every
/// address for the host is on the blocklist, and the transport error
/// mapping rebuilds the typed `NoAllowedAddress` refusal from it.
#[test]
fn resolver_refusal_surfaces_as_the_typed_refusal() {
    use std::str::FromStr;

    use super::refusal_in_source_chain;
    use crate::resolver::PinnedResolver;
    use reqwest::dns::Resolve;

    let refusal = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            PinnedResolver {
                allow_loopback_for_tests: false,
            }
            .resolve(reqwest::dns::Name::from_str("127.0.0.1").unwrap())
            .await
            .err()
            .expect("loopback is refused with the exception off")
        });

    // The sentinel survives in the chain, and the walk rebuilds the
    // refusal; the caller attaches the URL from the transport error.
    let rebuilt =
        refusal_in_source_chain(Some(refusal.as_ref())).expect("sentinel in source chain");
    match &rebuilt {
        GuardError::NoAllowedAddress { host, url } => {
            assert_eq!(host, "127.0.0.1");
            assert!(url.is_empty(), "url must be empty");
        }
        other => panic!("expected NoAllowedAddress, got {other}"),
    }
    assert!(rebuilt.is_refusal());
}

/// A resolver refusal that traveled inside a real reqwest transport
/// error rebuilds to the typed refusal: the client is built the same
/// way `get` builds its own, and `localhost` resolves to loopback
/// through the hosts file, so the refusal fires inside the connector
/// before any TCP connect. If reqwest ever drops the sentinel from its
/// error chain, this reports `Transport` and fails.
#[test]
fn map_transport_rebuilds_a_resolver_refusal_from_a_real_transport_error() {
    use super::map_transport;
    use crate::resolver::PinnedResolver;

    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            reqwest::Client::builder()
                .dns_resolver(PinnedResolver {
                    allow_loopback_for_tests: false,
                })
                .build()
                .unwrap()
                .get(url("https://localhost/llms.txt"))
                .send()
                .await
                .expect_err("loopback is refused with the exception off")
        });
    match map_transport(error) {
        GuardError::NoAllowedAddress { host, url } => {
            assert_eq!(host, "localhost");
            assert!(url.contains("localhost"), "{url}");
        }
        other => panic!("expected NoAllowedAddress, got {other}"),
    }
}

/// `subject` names the URL or host for the refusal variants, so the CLI
/// detail always carries context.
#[test]
fn subject_names_the_refusal_variants() {
    use super::GuardError as E;
    let cases: Vec<(E, &str)> = vec![
        (
            E::InvalidScheme {
                url: String::from("u1"),
                scheme: String::from("ftp"),
            },
            "u1",
        ),
        (
            E::Userinfo {
                url: String::from("u2"),
            },
            "u2",
        ),
        (
            E::BlockedPort {
                url: String::from("u3"),
                port: 22,
            },
            "u3",
        ),
        (
            E::BlockedAddress {
                url: String::from("u4"),
                address: "10.0.0.1".parse().unwrap(),
            },
            "u4",
        ),
        (
            E::NoAllowedAddress {
                host: String::from("h"),
                url: String::from("u5"),
            },
            "u5",
        ),
        (
            E::RedirectLimit {
                url: String::from("u6"),
                limit: 3,
            },
            "u6",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.subject(), expected, "{error}");
    }
}

/// `subject` names the URL or host for the transport-only variants.
#[test]
fn subject_names_the_transport_variants() {
    use super::GuardError as E;
    let cases: Vec<(E, &str)> = vec![
        (
            E::BodyTooLarge {
                url: String::from("u7"),
                limit: 1,
            },
            "u7",
        ),
        (
            E::Dns {
                host: String::from("h2"),
                source: Box::new(std::io::Error::other("down")),
            },
            "h2",
        ),
        (E::Timeout, "request"),
        (
            E::Http {
                url: String::from("u8"),
                status: reqwest::StatusCode::NOT_FOUND,
            },
            "u8",
        ),
        (
            E::NoPeerAddress {
                url: String::from("u9"),
            },
            "u9",
        ),
        (E::FixtureMiss(String::from("u10")), "u10"),
    ];
    for (error, expected) in cases {
        assert_eq!(error.subject(), expected, "{error}");
    }
}

/// `policy_reconstruction` clones the refusal variants and declines the
/// transport-only ones.
#[test]
fn policy_reconstruction_covers_the_refusal_variants() {
    use super::GuardError as E;
    let refused: Vec<E> = vec![
        E::InvalidScheme {
            url: String::from("u"),
            scheme: String::from("ftp"),
        },
        E::Userinfo {
            url: String::from("u"),
        },
        E::BlockedPort {
            url: String::from("u"),
            port: 22,
        },
        E::NoAllowedAddress {
            host: String::from("h"),
            url: String::from("u"),
        },
        E::RedirectLimit {
            url: String::from("u"),
            limit: 3,
        },
    ];
    for error in refused {
        assert!(error.policy_reconstruction().is_some(), "{error}");
    }
    // The byte cap carries no URL state to rebuild.
    assert!(
        E::BodyTooLarge {
            url: String::from("u"),
            limit: 1,
        }
        .policy_reconstruction()
        .is_none(),
        "transport-only variants do not reconstruct"
    );
    assert!(
        E::Timeout.policy_reconstruction().is_none(),
        "transport-only variants do not reconstruct"
    );
}

/// A source chain without a refusal sentinel yields nothing.
#[test]
fn source_chain_without_a_refusal_yields_none() {
    let plain = std::io::Error::other("no guard here");
    assert!(
        super::refusal_in_source_chain(Some(&plain)).is_none(),
        "a plain io error carries no refusal"
    );
}

/// The resolver refusal's `Display` names the host and the blocklist.
#[test]
fn resolver_refusal_displays_the_host() {
    let refusal = super::ResolverRefusal {
        host: String::from("internal.example"),
    };
    let text = refusal.to_string();
    assert!(text.contains("internal.example"), "{text}");
    assert!(text.contains("blocklist"), "{text}");
}

/// With the test-double exception on, the resolver pins loopback.
#[test]
fn resolver_pins_an_allowed_address() {
    use std::str::FromStr;

    use super::PinnedResolver;
    use reqwest::dns::Resolve;

    let mut pinned = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            PinnedResolver {
                allow_loopback_for_tests: true,
            }
            .resolve(reqwest::dns::Name::from_str("127.0.0.1").unwrap())
            .await
            .expect("loopback is allowed with the exception on")
        });
    let first = pinned.next().expect("one pinned address");
    assert_eq!(first.ip().to_string(), "127.0.0.1");
}

/// The local-double test recognizes loopback forms and declines
/// everything else.
#[test]
fn local_double_matches_only_loopback() {
    assert!(super::is_local_double(&url("http://localhost:8080/x")));
    assert!(super::is_local_double(&url("http://[::1]:8080/x")));
    assert!(!super::is_local_double(&url("https://nextjs.org/x")));
    // A URL with no host at all is not a local double.
    assert!(!super::is_local_double(&url("data:text/plain,hi")));
}

/// The production guard refuses a loopback hostname at resolution time:
/// every address `localhost` resolves to is on the blocklist, so the
/// fetch reports the typed refusal without any network round trip.
#[test]
fn fetch_refuses_a_loopback_hostname() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(async {
        guard()
            .get(&url("https://localhost/llms.txt"))
            .await
            .unwrap_err()
    });
    match error {
        GuardError::NoAllowedAddress { host, .. } => assert_eq!(host, "localhost"),
        other => panic!("expected NoAllowedAddress, got {other}"),
    }
}
