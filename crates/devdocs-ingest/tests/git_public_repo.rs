//! The public-repo clone path: a real `https://` clone through the git
//! adapter, run on demand only (`just test-slow`) — CI never reaches the
//! network through this suite (docs/implementation/07 §Git adapter).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{GitPin, IngestJob, gate_fetch};
use devdocs_model::config::{Config, SourceConfig};
use devdocs_testkit::base_config;
use tempfile::tempdir;
use url::Url;

/// `dtolnay/prettyplease`: a tiny repository whose default branch carries
/// a `README.md`, with stable release tags. The pin is a tag (not 40 hex,
/// so the plan classifies it as a tag pin).
const TAG: &str = "0.3.0";
const REPO_URL: &str = "https://github.com/dtolnay/prettyplease";

/// A config with the public repo registered as the `acme` git source.
fn config(url: &Url) -> Config {
    let mut config = base_config();
    config.sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("git")),
            url: Some(url.to_string()),
            license: Some(String::from("MIT")),
            attribution: None,
            expires: None,
            refresh_days: None,
        },
    );
    config
}

/// A tag-pin clone of a small public repository through the full git
/// adapter: the tag checks out, the published capture commit is a valid
/// 40-hex SHA, and the walk yields at least one allowlisted file (the
/// repo's README).
#[tokio::test]
#[ignore = "clones a public repository over the network; run via just test-slow"]
async fn public_repo_clone_yields_allowlisted_docs_at_the_pinned_tag() {
    let url = Url::parse(REPO_URL).unwrap();
    let config = config(&url);
    let grant = gate_fetch(&config, &url).unwrap();
    assert_eq!(grant.name, "acme");

    let db = tempdir().unwrap();
    let store = Arc::new(devdocs_testkit::open_migrated(&db.path().join("docs.db")).unwrap());
    let data = tempdir().unwrap();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let outcome = IngestJob::new(&config, &guard, None)
        .run_git(
            Arc::clone(&store),
            &url,
            "acme",
            "1.0",
            GitPin::from(TAG),
            data.path(),
        )
        .await
        .unwrap();

    // The walk found at least one allowlisted file and segmented it.
    assert!(
        outcome.pages >= 1,
        "the README is allowlisted and ingested: {outcome:?}"
    );
    assert!(outcome.chunks > 0);

    // The published source row records a valid capture commit: `git
    // rev-parse HEAD` at the checkout returned exactly 40 hex characters.
    let sources = store.list_sources().unwrap();
    assert_eq!(sources.len(), 1);
    let commit_sha = sources[0].commit_sha.as_deref().expect("capture commit");
    assert_eq!(
        commit_sha.len(),
        40,
        "rev-parse HEAD returned a 40-hex SHA: {commit_sha}"
    );
    assert!(
        commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the capture commit is hex: {commit_sha}"
    );
}
