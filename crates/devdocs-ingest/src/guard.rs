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
use std::time::{Duration, Instant};

use crate::resolver::{PinnedResolver, ResolverRefusal, bare_host, ip_allowed, is_local_double};
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
    /// address, and the connect-time re-check re-validates it. The lookup
    /// spends from the `deadline` the caller passes, so the DNS stall and
    /// the request share one per-request budget
    /// (docs/implementation/09 invariant 7).
    async fn resolve_pinned(&self, url: &Url, deadline: Instant) -> Result<IpAddr, GuardError> {
        let Some(host) = bare_host(url) else {
            return Err(GuardError::NoAllowedAddress {
                host: String::from("<no host>"),
                url: url.to_string(),
            });
        };
        let addrs = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            tokio::net::lookup_host((host, 0)),
        )
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
        // One deadline spans the DNS lookup and the request: the lookup
        // spends from it, and the request gets only the time left
        // (docs/implementation/09 invariant 7).
        let deadline = Instant::now() + self.timeout;
        let pinned = self.resolve_pinned(url, deadline).await?;

        let port = url.port_or_known_default().unwrap_or(443);
        let builder = reqwest::Client::builder()
            .user_agent(&self.user_agent)
            // Ambient proxies are off: HTTPS_PROXY and friends must not pull
            // the request away from the pinned address, and a proxy resolves
            // hostnames itself, which would bypass the pinned resolver
            // (docs/implementation/09 invariant 3).
            .no_proxy()
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

        // The request spends only the budget left after the lookup; an
        // exhausted budget refuses before any connection attempt.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(GuardError::Timeout);
        }
        let response = client
            .get(url.clone())
            .timeout(remaining)
            .send()
            .await
            .map_err(map_transport)?;
        self.verify_connect_time(&response, pinned, url)?;

        // The status and size checks run on the response of the last hop, so
        // the errors name that hop's URL; a direct fetch reports the request
        // URL because `final_url` equals it.
        let final_url = response.url().clone();
        let status = response.status();
        if !status.is_success() {
            return Err(GuardError::Http {
                url: final_url.to_string(),
                status,
            });
        }

        // The byte cap applies to decompressed bytes: with the gzip feature
        // on, `chunk` yields the decompressed stream
        // (docs/implementation/09 invariant 6).
        let mut body = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(map_transport)? {
            let total = body.len() as u64 + chunk.len() as u64;
            if total > self.max_bytes {
                return Err(GuardError::BodyTooLarge {
                    url: final_url.to_string(),
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

#[cfg(test)]
mod tests;
