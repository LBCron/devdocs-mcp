//! The pinned DNS resolver and the IP blocklist
//! (docs/implementation/09 invariants 3 and 4).
//!
//! The resolver is the one DNS lookup path in the tool: every connection's
//! lookup validates every resolved address against the blocklist and pins
//! one address, so a redirect hop cannot resolve-and-connect to a refused
//! address. The blocklist has no config escape; the one exception is the
//! test-double flag that only wiremock-backed tests set.

use std::net::{IpAddr, SocketAddr};
use url::Url;

/// The pinned resolver's blocklist refusal, as a distinct error type so
/// `map_transport` can rebuild the typed
/// [`GuardError::NoAllowedAddress`](crate::guard::GuardError::NoAllowedAddress)
/// refusal from inside a reqwest transport error. The resolver learns only
/// the host name from reqwest; `map_transport` attaches the hop URL.
#[derive(Debug)]
pub(crate) struct ResolverRefusal {
    /// The host whose every address was refused.
    pub(crate) host: String,
}

impl std::fmt::Display for ResolverRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "every resolved address for {} is on the blocklist",
            self.host
        )
    }
}

impl std::error::Error for ResolverRefusal {}

/// The IP blocklist (docs/implementation/09 invariant 4): private, loopback,
/// link-local, multicast, unspecified, documentation, cloud-metadata, and —
/// beyond the doc list, still fail-closed — shared address space, the
/// benchmarking range, the reserved block, and the broadcast address.
#[must_use]
pub fn ip_allowed(ip: &IpAddr, allow_loopback_for_tests: bool) -> bool {
    if allow_loopback_for_tests && is_loopback_form(ip) {
        return true;
    }
    !is_blocked(ip)
}

/// The host to resolve: `url::Url` keeps the brackets around an IPv6
/// literal, and neither getaddrinfo nor tokio's `ToSocketAddrs` accepts the
/// bracketed form, so strip them the way the policy check does.
pub(crate) fn bare_host(url: &Url) -> Option<&str> {
    url.host_str().map(|host| {
        host.strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(host)
    })
}

/// Loopback forms: plain IPv4/IPv6 loopback and the IPv4-mapped form.
fn is_loopback_form(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Whether the URL targets a local HTTP test double: a loopback host over
/// http or https, any port. Only reachable with the loopback exception on.
pub(crate) fn is_local_double(url: &Url) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => {
            let bare = host
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or(host);
            bare.parse::<IpAddr>().is_ok_and(|ip| is_loopback_form(&ip))
        }
        None => false,
    }
}

/// The blocklist test. v4-mapped and NAT64 forms canonicalize to their
/// embedded IPv4 first, so an embedded private address is caught by the v4
/// checks.
fn is_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(*v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    }
}

fn v4_blocked(v4: std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_private() // 10/8, 172.16/12, 192.168/16
        || v4.is_loopback() // 127/8
        || v4.is_link_local() // 169.254/16 incl. cloud metadata 169.254.169.254
        || v4.is_multicast() // 224/4
        || v4.is_unspecified() // 0.0.0.0
        || v4.is_broadcast() // 255.255.255.255
        || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 0b0100_0000) // 100.64/10 shared
        || (o[0] == 198 && (o[1] & 0b1111_1110) == 18) // 198.18/15 benchmarking
        || o[0] == 0 // 0/8 "this network" — includes 0.0.0.0
        || o[0] >= 240 // 240/4 reserved + 255.255.255.255 broadcast
}

fn v6_blocked(v6: &std::net::Ipv6Addr) -> bool {
    // Canonicalize IPv4-mapped and IPv4-compatible forms so the embedded
    // IPv4 goes through the v4 checks.
    if let Some(v4) = v6.to_ipv4_mapped() {
        return v4_blocked(v4);
    }
    let s = v6.segments();
    v6.is_unspecified() // ::
        || v6.is_loopback() // ::1
        || v6.is_unique_local() // fc00::/7
        || v6.is_unicast_link_local() // fe80::/10
        || v6.is_multicast() // ff00::/8
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0; 4]) // 64:ff9b::/96 NAT64
        || (s[0] == 0x2002) // 2002::/16 6to4 — embedded v4 unreachable to check
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // 100::/64 discard-only
}

/// The `reqwest::dns::Resolve` impl: every connection's lookup validates
/// against the blocklist and pins one address, so a redirect hop cannot
/// resolve-and-connect to a refused address (docs/09 invariants 3 and 4).
pub(crate) struct PinnedResolver {
    /// The test-double escape hatch: admits loopback forms so offline
    /// wiremock tests can exercise the live request path.
    pub(crate) allow_loopback_for_tests: bool,
}

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(
        &self,
        name: reqwest::dns::Name,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn Iterator<Item = SocketAddr> + Send>,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send,
        >,
    > {
        let host = name.as_str().to_owned();
        let allow_loopback = self.allow_loopback_for_tests;
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            let pinned = addrs
                .into_iter()
                .find(|addr| ip_allowed(&addr.ip(), allow_loopback))
                .ok_or_else(|| {
                    Box::new(ResolverRefusal { host: host.clone() })
                        as Box<dyn std::error::Error + Send + Sync>
                })?;
            Ok(Box::new(std::iter::once(pinned)) as Box<dyn Iterator<Item = SocketAddr> + Send>)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{bare_host, ip_allowed};
    use url::Url;

    /// Parses one URL in the test body.
    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// Every docs/09 blocklist literal is refused, address by address.
    #[test]
    fn ip_blocklist_covers_the_documented_ranges() {
        let blocked: [&str; 23] = [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "127.0.0.1",
            "127.8.8.8",
            "169.254.0.1",
            "169.254.169.254",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "100.64.0.1",
            "100.127.255.255",
            "198.18.0.1",
            "198.19.255.255",
            "::1",
        ];
        // The one deliberate non-blocked address in the list.
        assert!(ip_allowed(&"172.15.255.255".parse().unwrap(), false));
        // The benchmarking range ends at 198.19.255.255; 198.20 stays public.
        assert!(ip_allowed(&"198.20.0.1".parse().unwrap(), false));
        for raw in blocked {
            let ip: std::net::IpAddr = raw.parse().unwrap();
            assert!(!ip_allowed(&ip, false), "{raw} must be blocked");
        }
    }

    /// IPv6 forms the docs call out refuse, including embedded-v4 forms.
    #[test]
    fn ip_blocklist_covers_ipv6_forms() {
        for raw in [
            "::",
            "::1",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001:db8:1234:5678::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::10.0.0.1",
            "64:ff9b::7f00:1",
        ] {
            let ip: std::net::IpAddr = raw.parse().unwrap();
            assert!(!ip_allowed(&ip, false), "{raw} must be blocked");
        }
    }

    /// Public addresses stay allowed; the blocklist is not over-broad.
    #[test]
    fn ip_blocklist_admits_public_addresses() {
        for raw in [
            "1.1.1.1",
            "8.8.4.4",
            "172.32.0.1",  // just above 172.16/12
            "100.128.0.1", // just above 100.64/10
            "2606:4700::1111",
            "2620:0:ccc::2",
            "::ffff:1.1.1.1", // mapped public v4 stays public
        ] {
            let ip: std::net::IpAddr = raw.parse().unwrap();
            assert!(ip_allowed(&ip, false), "{raw} must be allowed");
        }
    }

    /// The resolution host drops the brackets around an IPv6 literal;
    /// `lookup_host` rejects the bracketed form.
    #[test]
    fn bare_host_strips_ipv6_brackets() {
        assert_eq!(
            bare_host(&url("https://[2606:4700::1111]/llms.txt")),
            Some("2606:4700::1111")
        );
        assert_eq!(
            bare_host(&url("https://nextjs.org/llms.txt")),
            Some("nextjs.org")
        );
        assert_eq!(bare_host(&url("data:text/plain,hi")), None);
    }
}
