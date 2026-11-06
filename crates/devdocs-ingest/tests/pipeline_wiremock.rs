//! End-to-end ingest pipeline tests over the live fetch path: the same
//! vertical slice as the fixture-replay tests, but the bytes come from a
//! wiremock server instead of a recorded replay set.
//!
//! docs/implementation/04 §Fixtures: wiremock servers bind `127.0.0.1` on
//! an ephemeral port. The blocklist refuses loopback, so every server in
//! this file runs behind the documented test-double escape hatch
//! ([`FetchGuard::with_loopback_allowed_for_tests`]); production paths
//! never set it, and the guard's own unit tests pin that.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{IngestError, IngestJob};
use devdocs_model::config::{Config, SourceConfig};
use devdocs_testkit::base_config;

/// The index body: one markdown page. The link is relative, so it joins
/// against the wiremock base URL the index actually came from.
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](/intro.md): Start here\n"
);

/// The intro markdown page.
const INTRO: &str = concat!(
    "# Intro\n\nThe Acme SDK ships a client for every runtime.\n\n",
    "## Install\n\nRun the installer.\n"
);

/// A config with the acme source registered at the wiremock index URL.
fn config(index_url: &str) -> Config {
    let mut config = base_config();
    config.sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("llms_txt")),
            url: Some(index_url.to_owned()),
            license: Some(String::from("MIT")),
            status: Some(String::from("approved_index")),
            refresh_days: None,
        },
    );
    config
}

/// A guard for the config with the test-double exception on: the wiremock
/// server sits on loopback, which the blocklist otherwise refuses
/// (docs/implementation/04 §Fixtures).
fn test_guard(config: &Config) -> FetchGuard {
    FetchGuard::from_fetch_config(&config.fetch).with_loopback_allowed_for_tests(true)
}

/// A fresh migrated store with its database directory: the directory must
/// outlive the store, so the pair returns together.
fn store() -> (tempfile::TempDir, Arc<devdocs_store::Store>) {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("docs.db");
    let store = Arc::new(devdocs_testkit::open_migrated(&path).unwrap());
    (db, store)
}

/// Mounts `path → 200 body` on the server.
async fn mount_ok(server: &MockServer, at: &str, body: &str) {
    Mock::given(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// Mounts `path → status` on the server.
async fn mount_status(server: &MockServer, at: &str, status: u16) {
    Mock::given(path(at))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

/// The wire path runs the whole pipeline over the network: the index and
/// its pages fetch from the wiremock server, the absent llms-full probe
/// reads the 404 as "not there", and the run publishes the page
/// (docs/implementation/07 §llms.txt adapter rule 3).
#[tokio::test]
async fn wire_ingest_fetches_the_index_and_pages_over_the_network() {
    let server = MockServer::start().await;
    mount_ok(&server, "/llms.txt", INDEX).await;
    mount_ok(&server, "/intro.md", INTRO).await;

    let index_url = format!("{}/llms.txt", server.uri());
    let config = config(&index_url);
    let guard = test_guard(&config);
    let (_db, store) = store();

    let index_url = url::Url::parse(&index_url).unwrap();
    let job = IngestJob::new(&config, &guard, None);
    let outcome = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();

    assert_eq!(outcome.pages, 1, "the page fetched over the wire");
    assert_eq!(
        outcome.events.full_exports, 0,
        "the absent export probe found nothing"
    );
    let hits = store
        .search_fts(&devdocs_store::build_match_query("installer"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the wire-fetched page is searchable");
}

/// The llms-full probe on the wire: when the export exists next to the
/// index it becomes one pre-built document, segmented like any other
/// markdown body (docs/implementation/07 §llms.txt adapter rule 3).
#[tokio::test]
async fn wire_ingest_probes_and_ingests_the_llms_full_export() {
    let server = MockServer::start().await;
    mount_ok(&server, "/llms.txt", INDEX).await;
    mount_ok(&server, "/intro.md", INTRO).await;
    let export = concat!(
        "# Intro\n\nThe Acme SDK ships a client for every runtime.\n\n",
        "# Advanced\n\nPagination follows cursors across pages of widgets.\n"
    );
    mount_ok(&server, "/llms-full.txt", export).await;

    let index_url = format!("{}/llms.txt", server.uri());
    let config = config(&index_url);
    let guard = test_guard(&config);
    let (_db, store) = store();

    let index_url = url::Url::parse(&index_url).unwrap();
    let job = IngestJob::new(&config, &guard, None);
    let outcome = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();

    assert_eq!(outcome.events.full_exports, 1, "the probe found the export");
    assert_eq!(outcome.pages, 2, "the page and the export both ingest");
}

/// The probe on the wire holds to the fetch contracts: a server error on
/// the export URL is not an expected absence — it fails the run closed
/// (docs/implementation/07 §llms.txt adapter rule 3).
#[tokio::test]
async fn wire_ingest_fails_when_the_llms_full_probe_hits_a_server_error() {
    let server = MockServer::start().await;
    mount_ok(&server, "/llms.txt", INDEX).await;
    mount_ok(&server, "/intro.md", INTRO).await;
    mount_status(&server, "/llms-full.txt", 500).await;

    let index_url = format!("{}/llms.txt", server.uri());
    let config = config(&index_url);
    let guard = test_guard(&config);
    let (_db, store) = store();

    let index_url = url::Url::parse(&index_url).unwrap();
    let job = IngestJob::new(&config, &guard, None);
    let error = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap_err();
    match error {
        IngestError::Guard(devdocs_ingest::GuardError::Http { status, .. }) => {
            assert_eq!(status.as_u16(), 500);
        }
        other => panic!("expected Guard(Http(500)), got {other}"),
    }
}
