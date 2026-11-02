//! The fetch-time license gate (docs/implementation/07 §License gate).
//!
//! Deny by default: a source with no `[sources]` registry entry is
//! `pending_review` and cannot fetch. The gate runs before any fetch, so a
//! refused source costs no bytes on the wire. The store's publish gate
//! (`approved_index`) is the second enforcement point
//! (docs/implementation/09 §License gate enforcement points).

use devdocs_model::config::Config;
use devdocs_model::error::{Error, UpstreamCode};
use devdocs_model::source::LicenseStatus;
use url::Url;

/// The fetch-time license decision for one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseGrant {
    /// The `[sources.<name>]` key; the library name for the ingest run.
    pub name: String,
    /// The recorded SPDX id, when the entry names one.
    pub license: Option<String>,
    /// The recorded approval stage.
    pub status: LicenseStatus,
}

/// Resolves the fetch-time license decision for the index URL.
///
/// # Errors
/// [`Error::Upstream`] with `FETCH_BLOCKED` when no registry entry matches
/// the URL (the `pending_review` deny-by-default refusal) or when the
/// recorded stage sits below `approved_fetch`; [`Error::ConfigInvalid`]
/// when an entry carries an unparsable status string.
pub fn gate_fetch(config: &Config, url: &Url) -> Result<LicenseGrant, Error> {
    let mut match_result: Option<(String, &devdocs_model::config::SourceConfig)> = None;
    for (name, entry) in &config.sources {
        let Some(candidate) = entry.parsed_url(&format!("sources.{name}"))? else {
            continue;
        };
        if candidate.as_str() == url.as_str() {
            match_result = Some((name.clone(), entry));
            break;
        }
    }

    let Some((name, entry)) = match_result else {
        return Err(Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            detail: format!(
                "source {url} is pending_review; record it under [sources.<name>] with a status of \
                 approved_fetch or higher to allow fetching"
            ),
        });
    };

    let status = entry
        .status
        .as_deref()
        .map_or(LicenseStatus::PendingReview, |raw| {
            LicenseStatus::parse(raw).unwrap_or(LicenseStatus::PendingReview)
        });
    if let Some(raw) = entry.status.as_deref()
        && LicenseStatus::parse(raw).is_none()
    {
        return Err(Error::ConfigInvalid {
            key_path: format!("sources.{name}.status"),
            reason: format!("unknown license status {raw:?}"),
        });
    }

    if !status.allows_fetch() {
        return Err(Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            detail: format!(
                "source {url} has license status {status}; fetching needs approved_fetch or higher"
            ),
        });
    }

    Ok(LicenseGrant {
        name,
        license: entry.license.clone(),
        status,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeMap;

    use super::gate_fetch;
    use devdocs_model::config::{Config, SourceConfig};
    use devdocs_model::error::{Error, UpstreamCode};
    use devdocs_model::source::LicenseStatus;
    use url::Url;

    /// A config with one registered source.
    fn config_with(name: &str, url: &str, status: Option<&str>) -> Config {
        let mut sources = BTreeMap::new();
        sources.insert(
            String::from(name),
            SourceConfig {
                kind: Some(String::from("llms_txt")),
                url: Some(String::from(url)),
                license: Some(String::from("MIT")),
                status: status.map(str::to_owned),
                refresh_days: None,
            },
        );
        Config {
            fetch: devdocs_model::config::FetchConfig {
                concurrency: 4,
                timeout_ms: 15_000,
                max_bytes: 20 * 1024 * 1024,
                max_redirects: 3,
                user_agent: String::from("devdocs/0.2.0"),
            },
            index: devdocs_model::config::IndexConfig {
                path: std::path::PathBuf::from("/tmp/unused.db"),
                embeddings: false,
            },
            search: devdocs_model::config::SearchConfig {
                max_tokens_default: 2000,
                limit_default: 8,
                rrf_k: 60,
            },
            sources,
        }
    }

    /// A URL for the tests.
    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// No registry entry denies with `FETCH_BLOCKED` — the deny-by-default
    /// contract.
    #[test]
    fn missing_entry_denies_by_default() {
        let config = config_with(
            "other",
            "https://docs.other.test/llms.txt",
            Some("approved_index"),
        );
        let error = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap_err();
        match error {
            Error::Upstream {
                code: UpstreamCode::FetchBlocked,
                detail,
            } => assert!(detail.contains("pending_review"), "{detail}"),
            other => panic!("expected FetchBlocked, got {other:?}"),
        }
    }

    /// A registered source at `approved_index` passes and carries the grant.
    #[test]
    fn registered_source_passes() {
        let config = config_with(
            "acme",
            "https://docs.acme.test/llms.txt",
            Some("approved_index"),
        );
        let grant = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap();
        assert_eq!(grant.name, "acme");
        assert_eq!(grant.status, LicenseStatus::ApprovedIndex);
        assert_eq!(grant.license.as_deref(), Some("MIT"));
    }

    /// A source below `approved_fetch` refuses with `FETCH_BLOCKED`.
    #[test]
    fn below_fetch_stage_refuses() {
        let config = config_with("acme", "https://docs.acme.test/llms.txt", Some("denied"));
        let error = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap_err();
        match error {
            Error::Upstream {
                code: UpstreamCode::FetchBlocked,
                detail,
            } => assert!(detail.contains("denied"), "{detail}"),
            other => panic!("expected FetchBlocked, got {other:?}"),
        }
    }

    /// A missing status string parses as `pending_review` and refuses.
    #[test]
    fn missing_status_stays_pending_review() {
        let config = config_with("acme", "https://docs.acme.test/llms.txt", None);
        let error = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap_err();
        match error {
            Error::Upstream {
                code: UpstreamCode::FetchBlocked,
                ..
            } => {}
            other => panic!("expected FetchBlocked, got {other:?}"),
        }
    }

    /// An unknown status string is a config error naming the key path.
    #[test]
    fn unknown_status_is_a_config_error() {
        let config = config_with("acme", "https://docs.acme.test/llms.txt", Some("approved"));
        let error = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap_err();
        match error {
            Error::ConfigInvalid { key_path, .. } => {
                assert_eq!(key_path, "sources.acme.status");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }
}
