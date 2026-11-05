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

use std::collections::BTreeMap;
use std::sync::Arc;

use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{IngestError, IngestJob};
use devdocs_model::config::{Config, FetchConfig, IndexConfig, SearchConfig, SourceConfig};

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
    let mut sources = BTreeMap::new();
    sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("llms_txt")),
            url: Some(index_url.to_owned()),
            license: Some(String::from("MIT")),
            status: Some(String::from("approved_index")),
            refresh_days: None,
        },
    );
    Config {
        fetch: FetchConfig {
            concurrency: 4,
            timeout_ms: 15_000,
            max_bytes: 20 * 1024 * 1024,
            max_redirects: 3,
            user_agent: String::from("devdocs/0.2.0"),
        },
        index: IndexConfig {
            path: std::path::PathBuf::from("/tmp/unused.db"),
            embeddings: false,
        },
        search: SearchConfig {
            max_tokens_default: 2000,
            limit_default: 8,
            rrf_k: 60,
        },
        sources,
    }
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

/// The wire path publishes the symbol pass: a page with a rust fenced
/// block carries its primary symbol on `chunks.symbol`, the `symbols`
/// table holds the extracted rows, and republishing the same generation
/// leaves the symbol row counts unchanged — idempotent
/// (docs/implementation/07 §Normalization pipeline, the symbol pass).
#[tokio::test]
async fn wire_ingest_publishes_symbols_idempotently() {
    let server = MockServer::start().await;
    mount_ok(&server, "/llms.txt", INDEX).await;
    mount_ok(
        &server,
        "/intro.md",
        concat!(
            "# Intro\n\nThe Acme SDK ships a client for every runtime.\n\n",
            "## parse_config\n\n",
            "```rust\n",
            "fn parse_config(path: &str) -> Config {\n",
            "    Config::load(path)\n",
            "}\n",
            "```\n"
        ),
    )
    .await;

    let index_url = format!("{}/llms.txt", server.uri());
    let config = config(&index_url);
    let guard = test_guard(&config);
    let (_db, store) = store();

    let index_url = url::Url::parse(&index_url).unwrap();
    let job = IngestJob::new(&config, &guard, None);
    job.run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();

    // The fence's section names the symbol, so `chunks.symbol` carries it
    // and the FTS hit for the code body exposes it.
    let source_id = store.find_source("acme", "1.0").unwrap().unwrap();
    let hits = store
        .search_fts_source(&devdocs_store::build_match_query("Config"), source_id, 10)
        .unwrap();
    assert!(
        hits.iter()
            .any(|hit| hit.symbol.as_deref() == Some("parse_config")),
        "the fenced chunk carries the primary symbol: {hits:?}"
    );

    // The `symbols` table holds the extracted rows, joined back to the
    // active source with their kind.
    let lookups = store
        .lookup_symbol("parse_config", devdocs_model::SourceId(source_id))
        .unwrap();
    assert_eq!(lookups.len(), 1, "one symbol row for the fenced block");
    assert_eq!(lookups[0].symbol_kind.as_deref(), Some("function"));

    // Republishing the same generation replaces the chunk set; the symbol
    // row counts land unchanged.
    let before = lookups.len();
    job.run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();
    let after = store
        .lookup_symbol("parse_config", devdocs_model::SourceId(source_id))
        .unwrap();
    assert_eq!(
        after.len(),
        before,
        "republish leaves symbol rows unchanged"
    );
}
