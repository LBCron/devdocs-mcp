//! The fetch guard — the only network code in the tool.
//!
//! Contracts: docs/implementation/09 §Fetch guard invariants and
//! docs/implementation/07 §Fetch guard. The invariants: HTTPS only; no
//! userinfo; HTTP(S) ports only; DNS resolves once through a validating
//! resolver that pins one address; the resolved IP passes the blocklist at
//! connect time; every redirect hop re-runs the URL checks; the byte cap
//! applies to decompressed bytes; the per-request timeout covers the whole
//! response; any guard error fails the whole source ingest closed.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use devdocs_model::config::FetchConfig;
use thiserror::Error as ThisError;
use url::Url;

/// Why the guard refused or failed one request
/// (docs/implementation/09 §Fetch guard invariants).
///
/// Policy refusals (scheme, userinfo, port, address, redirect limit, byte
/// cap) surface as `FETCH_BLOCKED`; transport failures (DNS, connection,
/// HTTP status, timeout) surface as `UPSTREAM_ERROR` (docs/implementation/08
/// §Error codes).
#[derive(Debug, ThisError)]
pub enum GuardError {
    /// The URL scheme is not HTTPS.
    #[error("scheme `{scheme}` is refused: {url}")]
    InvalidScheme {
        /// The refused URL.
        url: String,
        /// The refused scheme.
        scheme: String,
    },
    /// The URL carries userinfo.
    #[error("userinfo is refused: {url}")]
    Userinfo {
        /// The refused URL.
        url: String,
    },
    /// The URL port is not an HTTP(S) port.
    #[error("port {port} is not an HTTP(S) port: {url}")]
    BlockedPort {
        /// The refused URL.
        url: String,
        /// The refused port.
        port: u16,
    },
    /// The host resolves to — or is — an address on the blocklist.
    #[error("address {address} is on the blocklist: {url}")]
    BlockedAddress {
        /// The refused URL.
        url: String,
        /// The refused address.
        address: std::net::IpAddr,
    },
    /// The host resolved, but every address is on the blocklist.
    #[error("host {host} has no address outside the blocklist: {url}")]
    NoAllowedAddress {
        /// The refused host.
        host: String,
        /// The refused URL.
        url: String,
    },
    /// The redirect limit (3 by config) was reached.
    #[error("more than {limit} redirects: {url}")]
    RedirectLimit {
        /// The refused URL.
        url: String,
        /// The configured redirect limit.
        limit: u32,
    },
    /// The response exceeded the decompressed byte cap.
    #[error("response exceeds the {limit}-byte cap: {url}")]
    BodyTooLarge {
        /// The refused URL.
        url: String,
        /// The configured byte cap.
        limit: u64,
    },
    /// The host did not resolve.
    #[error("cannot resolve {host}: {source}")]
    Dns {
        /// The host that failed to resolve.
        host: String,
        /// The resolver error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// The response carried a non-success HTTP status.
    #[error("HTTP {status}: {url}")]
    Http {
        /// The URL that returned the status.
        url: String,
        /// The non-success status.
        status: reqwest::StatusCode,
    },
    /// The transport failed outside the cases above.
    #[error("transport failure: {0}")]
    Transport(#[from] reqwest::Error),
    /// The connection exposed no peer address. Cannot happen on TCP; the
    /// variant keeps the impossible case non-panicking.
    #[error("no peer address: {url}")]
    NoPeerAddress {
        /// The URL whose connection exposed no peer address.
        url: String,
    },
    /// The fixture replay has no recorded response for the URL.
    #[error("no fixture response recorded for {0}")]
    FixtureMiss(String),
}

impl GuardError {
    /// Whether this is a guard refusal (`FETCH_BLOCKED`) rather than a
    /// transport failure (`UPSTREAM_ERROR`).
    #[must_use]
    pub const fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::InvalidScheme { .. }
                | Self::Userinfo { .. }
                | Self::BlockedPort { .. }
                | Self::BlockedAddress { .. }
                | Self::NoAllowedAddress { .. }
                | Self::RedirectLimit { .. }
                | Self::BodyTooLarge { .. }
        )
    }

    /// The URL or host context carried in the error, for the CLI detail.
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::InvalidScheme { url, .. }
            | Self::Userinfo { url }
            | Self::BlockedPort { url, .. }
            | Self::BlockedAddress { url, .. }
            | Self::NoAllowedAddress { url, .. }
            | Self::RedirectLimit { url, .. }
            | Self::BodyTooLarge { url, .. }
            | Self::Http { url, .. }
            | Self::NoPeerAddress { url }
            | Self::FixtureMiss(url) => url.clone(),
            Self::Dns { host, .. } => host.clone(),
            Self::Timeout => String::from("request"),
            Self::Transport(e) => e
                .url()
                .map_or_else(|| String::from("request"), ToString::to_string),
        }
    }

    /// The cloneable reconstruction of a refusal: the redirect policy can
    /// only emit the URL-level variants, and reqwest wraps them inside a
    /// redirect-flavored transport error.
    fn policy_reconstruction(&self) -> Option<GuardError> {
        Some(match self {
            Self::InvalidScheme { url, scheme } => Self::InvalidScheme {
                url: url.clone(),
                scheme: scheme.clone(),
            },
            Self::Userinfo { url } => Self::Userinfo { url: url.clone() },
            Self::BlockedPort { url, port } => Self::BlockedPort {
                url: url.clone(),
                port: *port,
            },
            Self::BlockedAddress { url, address } => Self::BlockedAddress {
                url: url.clone(),
                address: *address,
            },
            Self::NoAllowedAddress { host, url } => Self::NoAllowedAddress {
                host: host.clone(),
                url: url.clone(),
            },
            Self::RedirectLimit { url, limit } => Self::RedirectLimit {
                url: url.clone(),
                limit: *limit,
            },
            _ => return None,
        })
    }
}

/// The URL-level guard checks, shared by [`FetchGuard::check`] and the
/// redirect policy (docs/implementation/09 invariants 1, 2, and 4's
/// IP-literal rule).
#[derive(Debug, Clone, Copy)]
pub(crate) struct GuardPolicy {
    max_redirects: u32,
    allow_loopback_for_tests: bool,
}

impl GuardPolicy {
    /// The synchronous URL checks: HTTPS only, no userinfo, HTTP(S) ports
    /// only, and IP-literal hosts against the blocklist. Hostname hosts
    /// validate at resolution and connect time.
    ///
    /// With the loopback test-double exception on, a local HTTP double
    /// (loopback host, http scheme, ephemeral port) is admitted; every
    /// other host keeps the full checks.
    // The docs/13 binding sketch fixes the shape as `check(&Url)`.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn check(&self, url: &Url) -> Result<(), GuardError> {
        if self.allow_loopback_for_tests && is_local_double(url) {
            return Ok(());
        }
        if url.scheme() != "https" {
            return Err(GuardError::InvalidScheme {
                url: url.to_string(),
                scheme: url.scheme().to_owned(),
            });
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(GuardError::Userinfo {
                url: url.to_string(),
            });
        }
        if let Some(port) = url.port_or_known_default()
            && port != 80
            && port != 443
        {
            return Err(GuardError::BlockedPort {
                url: url.to_string(),
                port,
            });
        }
        if let Some(host) = url.host_str() {
            // IPv6 hosts serialize with brackets; strip them before the
            // IP-literal parse.
            let bare = host
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .unwrap_or(host);
            if let Ok(ip) = bare.parse::<IpAddr>()
                && !ip_allowed(&ip, self.allow_loopback_for_tests)
            {
                return Err(GuardError::BlockedAddress {
                    url: url.to_string(),
                    address: ip,
                });
            }
        }
        Ok(())
    }
}

/// The fetch guard: one configured client-side policy plus the pinned
/// resolution and connect-time checks (docs/13 §M3 binding sketch:
/// `FetchGuard { client(), check(&Url), get(&Url) }`).
#[derive(Debug, Clone)]
pub struct FetchGuard {
    policy: GuardPolicy,
    max_bytes: u64,
    timeout: Duration,
    user_agent: String,
}

impl FetchGuard {
    /// Builds the guard from the `[fetch]` config section.
    #[must_use]
    pub fn from_fetch_config(fetch: &FetchConfig) -> Self {
        Self {
            policy: GuardPolicy {
                max_redirects: fetch.max_redirects,
                allow_loopback_for_tests: false,
            },
            max_bytes: fetch.max_bytes,
            timeout: Duration::from_millis(fetch.timeout_ms),
            user_agent: fetch.user_agent.clone(),
        }
    }

    /// The test-double escape hatch: wiremock and other local doubles bind
    /// `127.0.0.1`, which the blocklist refuses. Production paths never
    /// call this; the CLI builds the guard from config only, and the guard
    /// test asserts that default (docs/implementation/09 §Fetch guard
    /// invariants — the blocklist has no config escape).
    #[doc(hidden)]
    #[must_use]
    pub fn with_loopback_allowed_for_tests(mut self, allowed: bool) -> Self {
        self.policy.allow_loopback_for_tests = allowed;
        self
    }

    /// The synchronous URL checks: scheme, userinfo, ports, and IP-literal
    /// hosts against the blocklist.
    ///
    /// # Errors
    /// [`GuardError`] when any URL-level invariant fails.
    pub fn check(&self, url: &Url) -> Result<(), GuardError> {
        self.policy.check(url)
    }

    /// Resolves `host` once, validates every address against the blocklist,
    /// and pins the first allowed one. This is the one DNS lookup the guard
    /// does for the request; the connection opens only to the returned
    /// address, and the connect-time re-check re-validates it.
    async fn resolve_pinned(&self, url: &Url) -> Result<IpAddr, GuardError> {
        let Some(host) = bare_host(url) else {
            return Err(GuardError::NoAllowedAddress {
                host: String::from("<no host>"),
                url: url.to_string(),
            });
        };
        // The lookup shares the per-request budget: a stalled resolver must
        // surface as `Timeout`, not as a 600 s total-cap trip
        // (docs/implementation/09 invariant 7).
        let addrs = tokio::time::timeout(self.timeout, tokio::net::lookup_host((host, 0)))
            .await
            .map_err(|_| GuardError::Timeout)?
            .map_err(|e| GuardError::Dns {
                host: host.to_owned(),
                source: Box::new(e),
            })?;
        for addr in addrs {
            if ip_allowed(&addr.ip(), self.policy.allow_loopback_for_tests) {
                return Ok(addr.ip());
            }
        }
        Err(GuardError::NoAllowedAddress {
            host: host.to_owned(),
            url: url.to_string(),
        })
    }

    /// Fetches `url` through every guard invariant and returns the body
    /// plus the URL the response actually came from — the last URL after
    /// redirects, which relative links must join against.
    ///
    /// # Errors
    /// [`GuardError`] when any check refuses or the transport fails.
    pub async fn get(&self, url: &Url) -> Result<(bytes::Bytes, Url), GuardError> {
        self.check(url)?;
        let pinned = self.resolve_pinned(url).await?;

        let port = url.port_or_known_default().unwrap_or(443);
        let builder = reqwest::Client::builder()
            .user_agent(&self.user_agent)
            .redirect(self.redirect_policy())
            .dns_resolver(PinnedResolver {
                allow_loopback_for_tests: self.policy.allow_loopback_for_tests,
            })
            // The one resolution happened above; connect only to that
            // address, with SNI intact (docs/09 invariant 3).
            .resolve_to_addrs(
                url.host_str().unwrap_or_default(),
                &[SocketAddr::new(pinned, port)],
            );
        let client = builder.build().map_err(GuardError::Transport)?;

        let response = client
            .get(url.clone())
            .timeout(self.timeout)
            .send()
            .await
            .map_err(map_transport)?;
        self.verify_connect_time(&response, pinned, url)?;

        let status = response.status();
        if !status.is_success() {
            return Err(GuardError::Http {
                url: url.to_string(),
                status,
            });
        }

        // The byte cap applies to decompressed bytes: with the gzip feature
        // on, `chunk` yields the decompressed stream
        // (docs/implementation/09 invariant 6).
        let final_url = response.url().clone();
        let mut body = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(map_transport)? {
            let total = body.len() as u64 + chunk.len() as u64;
            if total > self.max_bytes {
                return Err(GuardError::BodyTooLarge {
                    url: url.to_string(),
                    limit: self.max_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok((bytes::Bytes::from(body), final_url))
    }

    /// The redirect policy: every hop re-runs the URL checks before the
    /// follow, and the hop limit is 3 by config
    /// (docs/implementation/09 invariant 5).
    fn redirect_policy(&self) -> reqwest::redirect::Policy {
        let policy = self.policy;
        reqwest::redirect::Policy::custom(move |attempt| {
            let limit = usize::try_from(policy.max_redirects).unwrap_or(usize::MAX);
            if attempt.previous().len() >= limit {
                let url = attempt.url().to_string();
                return attempt.error(GuardError::RedirectLimit {
                    url,
                    limit: policy.max_redirects,
                });
            }
            match policy.check(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(error) => attempt.error(error),
            }
        })
    }

    /// The connect-time re-check: the peer address the connection actually
    /// opened to must pass the blocklist. This is independent code that
    /// works with or without the pinned resolver
    /// (docs/implementation/13 §M3 risk register).
    fn verify_connect_time(
        &self,
        response: &reqwest::Response,
        pinned: IpAddr,
        url: &Url,
    ) -> Result<(), GuardError> {
        let Some(peer) = response.remote_addr() else {
            return Err(GuardError::NoPeerAddress {
                url: url.to_string(),
            });
        };
        if !ip_allowed(&peer.ip(), self.policy.allow_loopback_for_tests) {
            return Err(GuardError::BlockedAddress {
                url: url.to_string(),
                address: peer.ip(),
            });
        }
        // The initial connect must have opened to the pinned address; a
        // mismatch means the transport ignored the pin.
        if response.url().as_str() == url.as_str() && peer.ip() != pinned {
            return Err(GuardError::BlockedAddress {
                url: url.to_string(),
                address: peer.ip(),
            });
        }
        Ok(())
    }
}

/// Maps a transport error onto the guard taxonomy: timeouts and redirect
/// failures carry their own variants. A redirect failure whose source chain
/// carries a guard refusal unwraps to it, so the CLI detail names the real
/// reason (for example a redirect into a blocked address).
fn map_transport(error: reqwest::Error) -> GuardError {
    if error.is_timeout() {
        GuardError::Timeout
    } else if error.is_redirect() {
        unwrap_policy_error(&error).unwrap_or_else(|| GuardError::RedirectLimit {
            url: error
                .url()
                .map_or_else(|| String::from("<unknown>"), ToString::to_string),
            limit: 0,
        })
    } else if let Some(rebuilt) = unwrap_policy_error(&error) {
        // A connect-time failure whose source chain carries a guard refusal:
        // the pinned resolver refused every address for a redirect hop, so
        // the same host reports `FETCH_BLOCKED` on hop 0 and on a later hop
        // alike (docs/implementation/09 invariant 4).
        rebuilt
    } else {
        GuardError::Transport(error)
    }
}

/// Searches the error source chain for the [`GuardError`] a redirect policy
/// pass emitted, or for the pinned resolver's [`ResolverRefusal`] sentinel,
/// and rebuilds the cloneable refusal variants.
fn unwrap_policy_error(error: &reqwest::Error) -> Option<GuardError> {
    let rebuilt = refusal_in_source_chain(std::error::Error::source(error))?;
    Some(match rebuilt {
        // The resolver learns only the host name; attach the hop URL that
        // reqwest carries on the transport error.
        GuardError::NoAllowedAddress { host, url } if url.is_empty() => {
            GuardError::NoAllowedAddress {
                host,
                url: error
                    .url()
                    .map_or_else(|| String::from("<unknown>"), ToString::to_string),
            }
        }
        other => other,
    })
}

/// Walks a source chain looking for a rebuildable guard refusal: the
/// [`GuardError`] a redirect policy pass emitted, or the pinned resolver's
/// [`ResolverRefusal`]. A resolver-built refusal carries an empty URL that
/// the caller fills from the transport error.
fn refusal_in_source_chain(
    mut source: Option<&(dyn std::error::Error + 'static)>,
) -> Option<GuardError> {
    while let Some(current) = source {
        if let Some(guard) = current.downcast_ref::<GuardError>()
            && let Some(rebuilt) = guard.policy_reconstruction()
        {
            return Some(rebuilt);
        }
        if let Some(refusal) = current.downcast_ref::<ResolverRefusal>() {
            return Some(GuardError::NoAllowedAddress {
                host: refusal.host.clone(),
                url: String::new(),
            });
        }
        source = current.source();
    }
    None
}

/// The pinned resolver's blocklist refusal, as a distinct error type so
/// `map_transport` can rebuild the typed [`GuardError::NoAllowedAddress`]
/// refusal from inside a reqwest transport error. The resolver learns only
/// the host name from reqwest; `map_transport` attaches the hop URL.
#[derive(Debug)]
struct ResolverRefusal {
    host: String,
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
fn bare_host(url: &Url) -> Option<&str> {
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
fn is_local_double(url: &Url) -> bool {
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
struct PinnedResolver {
    allow_loopback_for_tests: bool,
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

    use super::{FetchGuard, GuardError, ip_allowed};
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

    /// The resolution host drops the brackets around an IPv6 literal;
    /// `lookup_host` rejects the bracketed form.
    #[test]
    fn bare_host_strips_ipv6_brackets() {
        assert_eq!(
            super::bare_host(&url("https://[2606:4700::1111]/llms.txt")),
            Some("2606:4700::1111")
        );
        assert_eq!(
            super::bare_host(&url("https://nextjs.org/llms.txt")),
            Some("nextjs.org")
        );
        assert_eq!(super::bare_host(&url("data:text/plain,hi")), None);
    }

    /// The pinned resolver reports a typed refusal sentinel when every
    /// address for the host is on the blocklist, and the transport error
    /// mapping rebuilds the typed `NoAllowedAddress` refusal from it.
    #[test]
    fn resolver_refusal_surfaces_as_the_typed_refusal() {
        use std::str::FromStr;

        use super::{PinnedResolver, refusal_in_source_chain};
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

    /// `subject` names the URL or host for every variant, so the CLI detail
    /// always carries context.
    #[test]
    fn subject_names_every_variant() {
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
}
