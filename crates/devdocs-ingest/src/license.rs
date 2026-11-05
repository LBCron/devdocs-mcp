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
/// the URL (the `pending_review` deny-by-default refusal).
pub fn gate_fetch(config: &Config, url: &Url) -> Result<LicenseGrant, Error> {
    let Some((name, entry)) = find_entry(config, url)? else {
        return Err(Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            detail: format!(
                "source {url} is pending_review; record it under [sources.<name>] to allow fetching"
            ),
        });
    };

    // TEMPORARY until the M7 store-backed gate lands (step 3): the config
    // no longer carries the mutable license status — the store owns it
    // (docs/implementation/07 §License gate) — so a registered entry
    // mechanically falls back to the index stage and the fetch and publish
    // gates keep their current behavior.
    let status = LicenseStatus::ApprovedIndex;

    Ok(LicenseGrant {
        name,
        license: entry.license.clone(),
        status,
    })
}

/// The registry status behind a gate refusal for `url`, when the URL has
/// an entry with a parseable status. The add path uses it to withdraw a
/// source whose license turned terminal before the refusal returns
/// (docs/implementation/09 §License gate enforcement points).
///
/// TEMPORARY until the M7 store-backed gate lands (step 3): the config no
/// longer records a license status, so this always returns `None` and the
/// add path withdraws nothing on a refusal.
#[must_use]
pub fn refused_status(config: &Config, url: &Url) -> Option<LicenseStatus> {
    let _ = (config, url);
    None
}

/// Finds the registry entry whose URL equals `url` exactly.
fn find_entry<'a>(
    config: &'a Config,
    url: &Url,
) -> Result<Option<(String, &'a devdocs_model::config::SourceConfig)>, Error> {
    for (name, entry) in &config.sources {
        let Some(candidate) = entry.parsed_url(&format!("sources.{name}"))? else {
            continue;
        };
        if candidate.as_str() == url.as_str() {
            return Ok(Some((name.clone(), entry)));
        }
    }
    Ok(None)
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

    /// A config with one registered source. The entry carries no license
    /// status: the store owns that state (docs/implementation/07 §License
    /// gate); until the store-backed gate lands, a registered entry falls
    /// back to the index stage.
    fn config_with(name: &str, url: &str) -> Config {
        let mut sources = BTreeMap::new();
        sources.insert(
            String::from(name),
            SourceConfig {
                kind: Some(String::from("llms_txt")),
                url: Some(String::from(url)),
                license: Some(String::from("MIT")),
                attribution: None,
                expires: None,
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
            ingest: devdocs_model::config::IngestConfig {
                git_extensions: devdocs_model::config::DEFAULT_GIT_EXTENSIONS
                    .iter()
                    .map(|ext| (*ext).to_owned())
                    .collect(),
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
        let config = config_with("other", "https://docs.other.test/llms.txt");
        let error = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap_err();
        match error {
            Error::Upstream {
                code: UpstreamCode::FetchBlocked,
                detail,
            } => assert!(detail.contains("pending_review"), "{detail}"),
            other => panic!("expected FetchBlocked, got {other:?}"),
        }
    }

    /// A registered source passes and carries the grant with the
    /// temporary index-stage fallback.
    #[test]
    fn registered_source_passes() {
        let config = config_with("acme", "https://docs.acme.test/llms.txt");
        let grant = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap();
        assert_eq!(grant.name, "acme");
        assert_eq!(grant.status, LicenseStatus::ApprovedIndex);
        assert_eq!(grant.license.as_deref(), Some("MIT"));
    }

    /// A refusal never names a registry status while the config carries no
    /// license state: nothing is withdrawn on a refusal.
    #[test]
    fn refused_status_is_none_without_config_status() {
        let registered = config_with("acme", "https://docs.acme.test/llms.txt");
        assert_eq!(
            super::refused_status(&registered, &url("https://docs.acme.test/llms.txt")),
            None
        );
        let unregistered = config_with("other", "https://docs.other.test/llms.txt");
        assert_eq!(
            super::refused_status(&unregistered, &url("https://docs.acme.test/llms.txt")),
            None
        );
    }

    /// A registry entry without a URL is skipped, not an error.
    #[test]
    fn entry_without_a_url_is_skipped() {
        let mut config = config_with("acme", "https://docs.acme.test/llms.txt");
        config.sources.insert(
            String::from("ghost"),
            devdocs_model::config::SourceConfig {
                kind: None,
                url: None,
                license: None,
                attribution: None,
                expires: None,
                refresh_days: None,
            },
        );
        let grant = gate_fetch(&config, &url("https://docs.acme.test/llms.txt")).unwrap();
        assert_eq!(grant.name, "acme");
        assert_eq!(
            super::refused_status(&config, &url("https://docs.acme.test/llms.txt")),
            None
        );
    }
}
