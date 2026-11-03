//! End-to-end ingest pipeline tests over recorded fixtures: the whole
//! vertical slice runs offline — index → pages → chunks → published store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use devdocs_ingest::fixture::FixtureSet;
use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{IngestJob, gate_fetch};
use devdocs_model::config::{Config, FetchConfig, IndexConfig, SearchConfig, SourceConfig};
use devdocs_model::error::{Error, UpstreamCode};
use devdocs_model::source::LicenseStatus;
use url::Url;

/// The index body: two markdown links and one HTML link.
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.md): Start here\n",
    "- [Advanced](https://docs.acme.test/advanced.md): Deep dive\n",
    "- [Printable](https://docs.acme.test/intro.html): The HTML twin\n"
);

const INTRO: &str = concat!(
    "# Intro\n\n",
    "The Acme SDK ships a client for every runtime.\n\n",
    "## Install\n\n",
    "Run the installer and export the key.\n\n",
    "```bash\nexport ACME_KEY=sk-test\n```\n"
);

const ADVANCED: &str = concat!(
    "# Advanced\n\n",
    "Pagination follows cursors across pages of widgets.\n\n",
    "## Cursors\n\n",
    "Pass the cursor to walk the whole list without double counting.\n"
);

/// Builds the fixture directory with the index and both pages.
fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
        { "url": "https://docs.acme.test/advanced.md", "status": 200, "headers": {}, "body": ADVANCED },
    ]);
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_string(&responses).unwrap(),
    )
    .unwrap();
    dir
}

/// A config with the acme source registered at the index stage.
fn config() -> Config {
    let mut sources = BTreeMap::new();
    sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("llms_txt")),
            url: Some(String::from("https://docs.acme.test/llms.txt")),
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

#[tokio::test]
async fn ingest_runs_index_to_published_chunks_offline() {
    let dir = fixture_dir();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    let config = config();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("docs.db");
    let store = Arc::new(devdocs_testkit::open_migrated(&db_path).unwrap());

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let grant = gate_fetch(&config, &index_url).unwrap();
    assert_eq!(grant.name, "acme");
    assert_eq!(grant.status, LicenseStatus::ApprovedIndex);

    let job = IngestJob::new(&config, &guard, Some(&fixture));
    // The debug shape reports whether a replay set is loaded, and never
    // the config contents.
    assert!(format!("{job:?}").contains("fixture: true"), "{job:?}");
    let outcome = job
        .run(Arc::clone(&store), &index_url, &grant.name, "1.0")
        .await
        .unwrap();

    assert_eq!(outcome.pages, 2, "two markdown pages ingested");
    assert_eq!(outcome.skipped_pages, 1, "the HTML twin is skipped");
    assert!(outcome.chunks > 0);
    assert_eq!(outcome.license_status, LicenseStatus::ApprovedIndex);

    // The published content is searchable through the store.
    let hits = store
        .search_fts(&devdocs_store::build_match_query("cursors"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the advanced page is searchable");

    // Republishing replaces the generation without duplicating chunks.
    let outcome2 = job
        .run(Arc::clone(&store), &index_url, &grant.name, "1.0")
        .await
        .unwrap();
    assert_eq!(outcome2.chunks, outcome.chunks);
}

#[tokio::test]
async fn ingest_denies_a_source_without_a_registry_entry() {
    let dir = fixture_dir();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    // No [sources] entry for acme — the deny-by-default gate must fire
    // before any bytes move.
    let mut config = config();
    config.sources.clear();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let db = tempfile::tempdir().unwrap();
    let store = Arc::new(devdocs_testkit::open_migrated(&db.path().join("docs.db")).unwrap());

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let error = gate_fetch(&config, &index_url).unwrap_err();
    match error {
        Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            detail,
        } => assert!(detail.contains("pending_review"), "{detail}"),
        other => panic!("expected FetchBlocked, got {other:?}"),
    }

    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap_err();
    match error {
        devdocs_ingest::IngestError::License(Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            ..
        }) => {}
        other => panic!("expected License(FetchBlocked), got {other:?}"),
    }
}

#[tokio::test]
async fn ingest_fails_closed_on_a_missing_fixture_page() {
    let dir = fixture_dir();
    // Drop the advanced page from the manifest: the index links it, the
    // replay cannot serve it — the run must fail closed before publish.
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
    ]);
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_string(&responses).unwrap(),
    )
    .unwrap();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    let config = config();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let db = tempfile::tempdir().unwrap();
    let store = Arc::new(devdocs_testkit::open_migrated(&db.path().join("docs.db")).unwrap());

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            devdocs_ingest::IngestError::Guard(devdocs_ingest::GuardError::FixtureMiss(_))
        ),
        "expected FixtureMiss, got {error}"
    );

    // The store carries nothing: the failed run published no source and no
    // chunks — not even from the pages that fetched fine.
    let hits = store
        .search_fts(&devdocs_store::build_match_query("cursors"), 10)
        .unwrap();
    assert!(hits.is_empty(), "a failed run must publish nothing");
    assert!(
        store.list_sources().unwrap().is_empty(),
        "a failed run must publish no source row"
    );
    assert!(
        devdocs_testkit::read_chunks(&db.path().join("docs.db"))
            .unwrap()
            .is_empty(),
        "a failed run must publish no chunks"
    );
}

#[tokio::test]
async fn fixture_replay_rejects_a_recorded_error_status() {
    let dir = tempfile::tempdir().unwrap();
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 403, "headers": {}, "body": INDEX },
    ]);
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_string(&responses).unwrap(),
    )
    .unwrap();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    let config = config();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(
            Arc::new(
                devdocs_testkit::open_migrated(
                    &tempfile::tempdir().unwrap().path().join("docs.db"),
                )
                .unwrap(),
            ),
            &index_url,
            "acme",
            "1.0",
        )
        .await
        .unwrap_err();
    match error {
        devdocs_ingest::IngestError::Guard(devdocs_ingest::GuardError::Http { status, .. }) => {
            assert_eq!(status.as_u16(), 403);
        }
        other => panic!("expected Guard(Http(403)), got {other}"),
    }
}

#[tokio::test]
async fn fixture_replay_enforces_the_byte_cap() {
    let dir = tempfile::tempdir().unwrap();
    let big_body = "x".repeat(64 * 1024 + 1);
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": big_body },
    ]);
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_string(&responses).unwrap(),
    )
    .unwrap();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    let mut config = config();
    config.fetch.max_bytes = 64 * 1024;
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(
            Arc::new(
                devdocs_testkit::open_migrated(
                    &tempfile::tempdir().unwrap().path().join("docs.db"),
                )
                .unwrap(),
            ),
            &index_url,
            "acme",
            "1.0",
        )
        .await
        .unwrap_err();
    match error {
        devdocs_ingest::IngestError::Guard(devdocs_ingest::GuardError::BodyTooLarge {
            limit,
            ..
        }) => assert_eq!(limit, 64 * 1024),
        other => panic!("expected Guard(BodyTooLarge), got {other}"),
    }
}
