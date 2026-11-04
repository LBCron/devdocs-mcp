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

/// The index body: one HTML page with a markdown twin, one HTML page
/// without a twin, and one markdown page.
const INDEX: &str = concat!(
    "# Acme\n\n> Acme widgets documentation.\n\n",
    "## Guides\n\n",
    "- [Intro](https://docs.acme.test/intro.html): Start here\n",
    "- [Faq](https://docs.acme.test/faq.html): No markdown twin\n",
    "- [Advanced](https://docs.acme.test/advanced.md): Deep dive\n"
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

/// The intro HTML page: its `Link` header names the markdown twin
/// (llmstxt.org v2: `rel="alternate" type="text/markdown"`).
const INTRO_HTML: &str = concat!(
    "<!doctype html><html><head>",
    "<title>Intro</title>",
    "</head><body><main><p>The Acme SDK ships a client for every runtime.</p></main></body></html>"
);

/// The faq HTML page: no markdown twin anywhere.
const FAQ_HTML: &str =
    "<!doctype html><html><body><p>Frequently asked questions about widgets.</p></body></html>";

/// The `Link` header of the intro HTML page.
const INTRO_TWIN_HEADER: &str =
    "<https://docs.acme.test/intro.md>; rel=\"alternate\"; type=\"text/markdown\"";

/// Builds the fixture directory with the index and every page it links.
fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": INTRO_TWIN_HEADER }, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
        { "url": "https://docs.acme.test/faq.html", "status": 200, "headers": {}, "body": FAQ_HTML },
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
async fn ingest_refuses_a_library_name_that_does_not_match_the_grant() {
    let dir = fixture_dir();
    let fixture = FixtureSet::load(dir.path()).unwrap();
    let config = config();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("docs.db");
    let store = Arc::new(devdocs_testkit::open_migrated(&db_path).unwrap());

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    gate_fetch(&config, &index_url).unwrap();

    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(Arc::clone(&store), &index_url, "not-acme", "1.0")
        .await
        .unwrap_err();
    match error {
        devdocs_ingest::IngestError::LicenseNameMismatch { expected, actual } => {
            assert_eq!(expected, "acme");
            assert_eq!(actual, "not-acme");
        }
        other => panic!("expected LicenseNameMismatch, got {other}"),
    }
    // A refused run publishes nothing.
    assert_eq!(
        store
            .search_fts(&devdocs_store::build_match_query("cursors"), 10)
            .unwrap(),
        Vec::new()
    );
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

    assert_eq!(outcome.pages, 2, "the twin and the markdown page ingested");
    assert_eq!(outcome.skipped_pages, 1, "the twin-less HTML page skips");
    assert!(outcome.chunks > 0);
    assert_eq!(outcome.license_status, LicenseStatus::ApprovedIndex);
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the intro page ingested through its markdown twin"
    );

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
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": INTRO_TWIN_HEADER }, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
        { "url": "https://docs.acme.test/faq.html", "status": 200, "headers": {}, "body": FAQ_HTML },
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

/// A markdown twin that serves an HTML document is unusable: its page falls
/// back to the HTML path and skips, and its markup never reaches the chunks
/// (docs/implementation/07 §Media split, §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_falls_back_when_a_markdown_twin_serves_html() {
    let dir = tempfile::tempdir().unwrap();
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": INTRO_TWIN_HEADER }, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": "<!DOCTYPE html><html><body><p>not markdown</p></body></html>" },
        { "url": "https://docs.acme.test/faq.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/advanced.md", "status": 200, "headers": {}, "body": ADVANCED },
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
    let outcome = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();

    assert_eq!(outcome.pages, 1, "only the markdown page is ingested");
    assert_eq!(
        outcome.skipped_pages, 2,
        "the twin-less page and the failed twin both skip"
    );
    assert_eq!(outcome.events.markdown_twins_used, 0);
    let hits = store
        .search_fts(&devdocs_store::build_match_query("not markdown"), 10)
        .unwrap();
    assert!(hits.is_empty(), "HTML markup must not be searchable");
}

/// Writes the manifest entries into `dir` and loads the fixture set.
fn write_fixture_set(dir: &std::path::Path, responses: &serde_json::Value) -> FixtureSet {
    std::fs::write(dir.join("manifest.json"), responses.to_string()).unwrap();
    FixtureSet::load(dir).unwrap()
}

/// The full manifest entries with every page carrying content.
fn content_responses() -> serde_json::Value {
    serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": INTRO_TWIN_HEADER }, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
        { "url": "https://docs.acme.test/faq.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/advanced.md", "status": 200, "headers": {}, "body": ADVANCED },
    ])
}

/// Subpath scoping: a deeper llms.txt linked from the root index governs
/// its subtree — the root defers a page the deeper index covers, the page
/// ingests once, and the subpath index is counted
/// (docs/implementation/07 §llms.txt adapter rule 4).
#[tokio::test]
async fn ingest_scopes_pages_to_the_most_specific_subpath_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Api](https://docs.acme.test/api/llms.txt): the api subtree index\n",
        "- [Overview](https://docs.acme.test/overview.md): root level\n",
        "- [Auth](https://docs.acme.test/api/auth.md): also listed at the root\n"
    );
    let api_index = concat!(
        "# Acme API\n\n> The api subtree.\n\n",
        "## Endpoints\n\n",
        "- [Auth](https://docs.acme.test/api/auth.md): the most specific listing\n"
    );
    let overview =
        "# Overview\n\nThe Acme widgets platform overview.\n\n## Tour\n\nWalk the tour.\n";
    let auth = "# Auth\n\nAuthenticate with an API key.\n\n## Scopes\n\nKeys carry scopes.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/api/llms.txt", "status": 200, "headers": {}, "body": api_index },
        { "url": "https://docs.acme.test/overview.md", "status": 200, "headers": {}, "body": overview },
        { "url": "https://docs.acme.test/api/auth.md", "status": 200, "headers": {}, "body": auth },
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
    let outcome = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();

    assert_eq!(outcome.pages, 2, "overview and auth ingest once each");
    assert_eq!(outcome.skipped_pages, 0);
    assert_eq!(
        outcome.events.subpath_indexes, 1,
        "the api subpath index scoped"
    );
    // The auth page is searchable: the deferred root listing did not block
    // the deeper scope from ingesting it.
    let hits = store
        .search_fts(&devdocs_store::build_match_query("scopes"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the api page is searchable");
}

/// A run whose only content page carries no content blocks segments to zero
/// chunks and fails closed: the active generation stays untouched
/// (docs/implementation/07 §Atomic publish).
#[tokio::test]
async fn ingest_rejects_a_run_that_segments_to_zero_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture_set(dir.path(), &content_responses());
    let config = config();
    let guard = FetchGuard::from_fetch_config(&config.fetch);

    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("docs.db");
    let store = Arc::new(devdocs_testkit::open_migrated(&db_path).unwrap());

    // Publish a good generation first, then a run whose pages carry no
    // content blocks: the zero-chunk refusal must keep the old generation
    // active.
    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let good = IngestJob::new(&config, &guard, Some(&fixture))
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap();
    assert!(good.chunks > 0);

    let heading_only = "# Intro\n\n## Install\n\n### Notes\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": INDEX },
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": { "link": INTRO_TWIN_HEADER }, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": heading_only },
        { "url": "https://docs.acme.test/faq.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/advanced.md", "status": 200, "headers": {}, "body": heading_only },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
        .await
        .unwrap_err();
    assert!(
        matches!(error, devdocs_ingest::IngestError::NoIngestablePages { .. }),
        "expected NoIngestablePages, got {error}"
    );

    let hits = store
        .search_fts(&devdocs_store::build_match_query("cursors"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the previous generation stays searchable");
    let sources = store.list_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].active, "the old generation stays active");
}
