//! End-to-end ingest pipeline tests over recorded fixtures: the whole
//! vertical slice runs offline — index → pages → chunks → published store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use devdocs_ingest::fixture::FixtureSet;
use devdocs_ingest::guard::FetchGuard;
use devdocs_ingest::{IngestJob, gate_fetch};
use devdocs_model::config::{Config, SourceConfig};
use devdocs_model::error::{Error, UpstreamCode};
use devdocs_model::source::LicenseStatus;
use devdocs_testkit::base_config;
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
    let mut config = base_config();
    config.sources.insert(
        String::from("acme"),
        SourceConfig {
            kind: Some(String::from("llms_txt")),
            url: Some(String::from("https://docs.acme.test/llms.txt")),
            license: Some(String::from("MIT")),
            status: Some(String::from("approved_index")),
            refresh_days: None,
        },
    );
    config
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

/// The deeper index overrides the root for its subtree: when both indexes
/// list the same page with different relation hints, the deeper listing is
/// what ingests. The root's twin hint names a URL no fixture serves, so a
/// run that used the root listing would fail closed on the missing twin
/// (docs/implementation/07 §llms.txt adapter rule 4).
#[tokio::test]
async fn ingest_uses_the_deeper_listings_relation_hints() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Api](https://docs.acme.test/api/llms.txt): the api subtree index\n",
        "- [Auth](https://docs.acme.test/api/auth.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/api/auth-twin-broken.md): the root listing\n"
    );
    let api_index = concat!(
        "# Acme API\n\n> The api subtree.\n\n",
        "## Endpoints\n\n",
        "- [Auth](https://docs.acme.test/api/auth.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/api/auth.md): the deeper listing\n"
    );
    let auth_html = "<!doctype html><html><body>authentication docs</body></html>";
    let auth_md = "# Auth\n\nAuthenticate with an API key.\n\n## Scopes\n\nKeys carry scopes.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/api/llms.txt", "status": 200, "headers": {}, "body": api_index },
        { "url": "https://docs.acme.test/api/auth.html", "status": 200, "headers": {}, "body": auth_html },
        { "url": "https://docs.acme.test/api/auth.md", "status": 200, "headers": {}, "body": auth_md },
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

    assert_eq!(
        outcome.pages, 1,
        "the page ingests once, via the deeper twin"
    );
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the deeper listing's twin hint is the one that ran"
    );
    let hits = store
        .search_fts(&devdocs_store::build_match_query("scopes"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the twin body is searchable");
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

/// The llms-full probe: when the export exists at the index's directory it
/// becomes one pre-built document, segmented like any other markdown body;
/// when absent, the run proceeds without one
/// (docs/implementation/07 §llms.txt adapter rule 3).
#[tokio::test]
async fn ingest_probes_llms_full_and_ingests_it_as_one_export() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/intro.md): Start here\n"
    );
    let intro = concat!(
        "# Intro\n\nThe Acme SDK ships a client for every runtime.\n\n",
        "## Install\n\nRun the installer.\n"
    );
    let export = concat!(
        "# Intro\n\nThe Acme SDK ships a client for every runtime.\n\n",
        "## Install\n\nRun the installer.\n\n",
        "# Advanced\n\nPagination follows cursors across pages of widgets.\n\n",
        "## Cursors\n\nPass the cursor to walk the whole list.\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": intro },
        { "url": "https://docs.acme.test/llms-full.txt", "status": 200, "headers": {}, "body": export },
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

    assert_eq!(outcome.events.full_exports, 1, "the probe found the export");
    assert_eq!(outcome.pages, 2, "the page and the export both ingest");
    // The export's content is searchable: it was segmented like any other
    // markdown body.
    let hits = store
        .search_fts(&devdocs_store::build_match_query("cursors"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the export content is searchable");
}

/// A recorded llms-full export holds to the same replay contracts as any
/// fetch: a body over the byte cap fails the run instead of ingesting
/// (docs/implementation/09 invariant 5).
#[tokio::test]
async fn probe_rejects_an_oversized_recorded_export() {
    let dir = tempfile::tempdir().unwrap();
    let index = "# Acme\n\n> Acme widgets documentation.\n\n- [Intro](https://docs.acme.test/intro.md): Start here\n";
    let big_body = "x".repeat(64 * 1024 + 1);
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": "# Intro\n\nContent.\n" },
        { "url": "https://docs.acme.test/llms-full.txt", "status": 200, "headers": {}, "body": big_body },
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

    let db = tempfile::tempdir().unwrap();
    let store = Arc::new(devdocs_testkit::open_migrated(&db.path().join("docs.db")).unwrap());

    let index_url = Url::parse("https://docs.acme.test/llms.txt").unwrap();
    let job = IngestJob::new(&config, &guard, Some(&fixture));
    let error = job
        .run(Arc::clone(&store), &index_url, "acme", "1.0")
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

/// The describedby fixture: two hint-carrying HTML pages, each naming the
/// index that covers it, and two markdown pages under those indexes.
/// Runs the ingest over the fixture and returns the published store plus
/// the run outcome (docs/implementation/07 §llms.txt adapter rule 2).
async fn run_describedby_ingest() -> (Arc<devdocs_store::Store>, devdocs_ingest::IngestOutcome) {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Api](https://docs.acme.test/api/intro.html) (rel=\"describedby\" /api/llms.txt): covered by the api index\n",
        "- [Guide](https://docs.acme.test/guide/intro.html) (rel=\"describedby\" /guide/llms.txt): covered by the guide index\n"
    );
    let api_index = concat!(
        "# Acme API\n\n> The api subtree.\n\n",
        "## Endpoints\n\n",
        "- [Intro](https://docs.acme.test/api/intro.md): the api page\n"
    );
    let guide_index = concat!(
        "# Acme Guide\n\n> The guide subtree.\n\n",
        "## Walkthrough\n\n",
        "- [Intro](https://docs.acme.test/guide/intro.md): the guide page\n"
    );
    let api_md =
        "# Api intro\n\nAuthenticate with an API key.\n\n## Scopes\n\nKeys carry scopes.\n";
    let guide_md =
        "# Guide intro\n\nFollow the guided tour.\n\n## Steps\n\nEvery step is listed.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/api/llms.txt", "status": 200, "headers": {}, "body": api_index },
        { "url": "https://docs.acme.test/guide/llms.txt", "status": 200, "headers": {}, "body": guide_index },
        { "url": "https://docs.acme.test/api/intro.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/guide/intro.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/api/intro.md", "status": 200, "headers": {}, "body": api_md },
        { "url": "https://docs.acme.test/guide/intro.md", "status": 200, "headers": {}, "body": guide_md },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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
    (store, outcome)
}

/// A `rel="describedby"` hint queues the index it names: each hint counts
/// as a subpath index, each queued index ingests its page, and the
/// hint-carrying HTML pages themselves skip
/// (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_queues_the_index_a_describedby_hint_names() {
    let (_store, outcome) = run_describedby_ingest().await;
    assert_eq!(
        outcome.events.subpath_indexes, 2,
        "each describedby hint queued its index"
    );
    assert_eq!(outcome.pages, 2, "each queued index ingested its page");
    assert_eq!(
        outcome.skipped_pages, 2,
        "the hint-carrying HTML pages themselves skip"
    );
}

/// The pages the describedby-queued indexes govern reach the store and
/// stay searchable (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn describedby_queued_indexes_publish_searchable_pages() {
    let (store, _outcome) = run_describedby_ingest().await;
    let hits = store
        .search_fts(&devdocs_store::build_match_query("scopes"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the api page is searchable");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("tour"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the guide page is searchable");
}

/// A link the seen set already holds re-fetches and replaces its buffered
/// document in place: the two listings produce the same body, so the corpus
/// holds the page exactly once and the page count does not grow
/// (docs/implementation/07 §llms.txt adapter rule 4).
#[tokio::test]
async fn ingest_holds_a_link_listed_twice_once() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/intro.md): Start here\n",
        "- [Intro again](https://docs.acme.test/intro.md): the same page, listed twice\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(outcome.pages, 1, "the duplicate listing ingested nothing");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("installer"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the page is searchable exactly once");
}

/// A markdown twin hint whose URL cannot join against the page is invalid:
/// the run fails with `LLMS_TXT_INVALID` instead of fetching
/// (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_fails_when_a_markdown_twin_does_not_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/intro.html) (rel=\"alternate\" type=\"text/markdown\" http://[): broken twin hint\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.html", "status": 200, "headers": {}, "body": INTRO_HTML },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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
    match error {
        devdocs_ingest::IngestError::LlmsTxtInvalid { url, reason } => {
            assert_eq!(url, "https://docs.acme.test/intro.html");
            assert!(reason.contains("does not resolve"), "{reason}");
        }
        other => panic!("expected LlmsTxtInvalid, got {other}"),
    }
}

/// A twin the seen set already holds is skipped without a fetch: the first
/// HTML page's twin reached the corpus, the second page pointing at the
/// same twin neither fetches it again nor counts as skipped
/// (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_skips_a_twin_the_seen_set_already_holds() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Alpha](https://docs.acme.test/a.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/t.md): first listing\n",
        "- [Beta](https://docs.acme.test/b.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/t.md): same twin\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/a.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/b.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/t.md", "status": 200, "headers": {}, "body": ADVANCED },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(outcome.pages, 1, "the twin ingested once");
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the twin ran through the first listing only"
    );
    assert_eq!(
        outcome.skipped_pages, 0,
        "a shared twin is not a skipped page"
    );
}

/// A fixture that records a status outside the HTTP status range fails the
/// replay the same way an invalid fetch would: the recorded status cannot
/// be read as a success, so the run fails closed
/// (docs/implementation/04 §Fixtures, docs/implementation/09 invariant 5).
#[tokio::test]
async fn fixture_replay_rejects_an_unparsable_recorded_status() {
    let dir = tempfile::tempdir().unwrap();
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 9999, "headers": {}, "body": INDEX },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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
        devdocs_ingest::IngestError::LlmsTxtInvalid { reason, .. } => {
            assert!(reason.contains("fixture records status 9999"), "{reason}");
        }
        other => panic!("expected LlmsTxtInvalid, got {other}"),
    }
}

/// A non-page link in an index — an asset such as an image — is not a page:
/// the walker counts it as a skipped page and carries on
/// (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_counts_a_non_page_link_as_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/intro.md): Start here\n",
        "- [Logo](https://docs.acme.test/logo.png): an asset, not a page\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(outcome.pages, 1, "only the markdown page ingested");
    assert_eq!(outcome.skipped_pages, 1, "the asset link counts as skipped");
}

/// An HTML page the root listing covers without a markdown twin skips, but
/// it does not stay in the seen set: a deeper scope its `rel="describedby"`
/// hint queued can list the same page again with a twin hint, and that
/// listing ingests it (docs/implementation/07 §llms.txt adapter rules 2, 4).
#[tokio::test]
async fn ingest_reingests_a_skipped_html_page_a_deeper_scope_lists_with_a_twin() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/api/intro.html) (rel=\"describedby\" /api/llms.txt): no twin at the root\n"
    );
    let api_index = concat!(
        "# Acme API\n\n> The api subtree.\n\n",
        "## Endpoints\n\n",
        "- [Intro](https://docs.acme.test/api/intro.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/api/intro.md): the deeper listing has the twin\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/api/llms.txt", "status": 200, "headers": {}, "body": api_index },
        { "url": "https://docs.acme.test/api/intro.html", "status": 200, "headers": {}, "body": INTRO_HTML },
        { "url": "https://docs.acme.test/api/intro.md", "status": 200, "headers": {}, "body": INTRO },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(
        outcome.events.subpath_indexes, 1,
        "the describedby hint queued the api index"
    );
    assert_eq!(outcome.pages, 1, "the page ingested through its twin");
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the deeper listing's twin hint ran"
    );
    assert_eq!(outcome.skipped_pages, 1, "only the root skip counts");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("installer"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the twin body is searchable");
}

/// An HTML document recorded at the `llms-full.txt` probe URL reads as
/// absence: no export is counted, and its markup never reaches the chunks
/// (docs/implementation/07 §llms.txt adapter rule 3).
#[tokio::test]
async fn ingest_treats_html_at_the_llms_full_probe_url_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    let index = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Intro](https://docs.acme.test/intro.md): Start here\n"
    );
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": index },
        { "url": "https://docs.acme.test/intro.md", "status": 200, "headers": {}, "body": INTRO },
        { "url": "https://docs.acme.test/llms-full.txt", "status": 200, "headers": {}, "body": FAQ_HTML },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(
        outcome.events.full_exports, 0,
        "HTML at the probe URL counts as absence"
    );
    assert_eq!(outcome.pages, 1, "only the markdown page ingested");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("questions"), 10)
        .unwrap();
    assert!(hits.is_empty(), "the probe HTML must not be searchable");
}

/// A duplicate listing keeps its `rel="describedby"` hint: the page reached
/// the corpus through its first listing, but the second listing still names
/// the index that covers it — that index queues and ingests the deeper page
/// it lists (docs/implementation/07 §llms.txt adapter rules 2, 4).
#[tokio::test]
async fn ingest_returns_the_describedby_hint_of_a_duplicate_listing() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Page](https://docs.acme.test/sub/page.md): listed plainly at the root\n",
        "- [Other](https://docs.acme.test/other.html) (rel=\"describedby\" /sub/llms.txt): queues the sub index after the page\n"
    );
    let sub_index = concat!(
        "# Acme Sub\n\n> The sub subtree.\n\n",
        "## Pages\n\n",
        "- [Page](https://docs.acme.test/sub/page.md) (rel=\"describedby\" /deep/llms.txt): the same page, listed again with a hint\n"
    );
    let deep_index = concat!(
        "# Acme Deep\n\n> The deep subtree.\n\n",
        "## Pages\n\n",
        "- [Extra](https://docs.acme.test/deep/extra.md): the deeper page\n"
    );
    let page =
        "# Page\n\nThe page body carries the installer steps.\n\n## Steps\n\nRun the steps.\n";
    let extra =
        "# Extra\n\nThe deep page body carries the cursor rules.\n\n## Cursors\n\nUse cursors.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/sub/llms.txt", "status": 200, "headers": {}, "body": sub_index },
        { "url": "https://docs.acme.test/deep/llms.txt", "status": 200, "headers": {}, "body": deep_index },
        { "url": "https://docs.acme.test/sub/page.md", "status": 200, "headers": {}, "body": page },
        { "url": "https://docs.acme.test/other.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/deep/extra.md", "status": 200, "headers": {}, "body": extra },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(
        outcome.events.subpath_indexes, 2,
        "the describedby hint queued the sub index and the duplicate's hint queued the deep index"
    );
    assert_eq!(
        outcome.pages, 2,
        "the duplicate page buffered once, the deep page joined"
    );
    assert_eq!(
        outcome.skipped_pages, 1,
        "only the twin-less hint page skips"
    );
    let hits = store
        .search_fts(&devdocs_store::build_match_query("installer"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the duplicate page is searchable once");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("cursors"), 10)
        .unwrap();
    assert!(
        !hits.is_empty(),
        "the deep page the hint unlocked is searchable"
    );
}

/// An HTML page whose markdown twin names the page itself has no markdown
/// body anywhere: the self-referential twin skips and releases the page, so
/// a later deeper listing can still ingest it through a real twin
/// (docs/implementation/07 §llms.txt adapter rule 2).
#[tokio::test]
async fn ingest_releases_a_page_whose_markdown_twin_names_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Self](https://docs.acme.test/deep/self.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/deep/self.html): the twin names the page itself\n",
        "- [Other](https://docs.acme.test/other.html) (rel=\"describedby\" /deep/llms.txt): queues the deep index after the page\n"
    );
    let deep_index = concat!(
        "# Acme Deep\n\n> The deep subtree.\n\n",
        "## Pages\n\n",
        "- [Self](https://docs.acme.test/deep/self.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/deep/self.md): the deeper listing carries a real twin\n"
    );
    let self_md =
        "# Self\n\nThe self page body carries the key handling.\n\n## Keys\n\nKeys rotate.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/deep/llms.txt", "status": 200, "headers": {}, "body": deep_index },
        { "url": "https://docs.acme.test/deep/self.html", "status": 200, "headers": {}, "body": FAQ_HTML },
        { "url": "https://docs.acme.test/deep/self.md", "status": 200, "headers": {}, "body": self_md },
        { "url": "https://docs.acme.test/other.html", "status": 200, "headers": {}, "body": FAQ_HTML },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(
        outcome.skipped_pages, 2,
        "the self-referential twin skips and the twin-less hint page skips"
    );
    assert_eq!(
        outcome.pages, 1,
        "nothing buffers for the self-twin page itself"
    );
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the deeper listing's real twin ran after the release"
    );
    let hits = store
        .search_fts(&devdocs_store::build_match_query("rotate"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the deeper twin body is searchable");
}

/// A page buffered through a shallower listing is replaced when a deeper
/// scope lists it again: the deeper listing's markdown twin re-fetches the
/// page and overwrites the buffered document in place, so the deeper twin's
/// body is what remains, the shallow twin's body is gone, and the page
/// count did not grow (docs/implementation/07 §llms.txt adapter rule 4).
#[tokio::test]
async fn ingest_replaces_a_buffered_page_when_a_deeper_listing_relists_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = concat!(
        "# Acme\n\n> Acme widgets documentation.\n\n",
        "## Guides\n\n",
        "- [Page](https://docs.acme.test/deep/page.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/root-twin.md rel=\"describedby\" /deep/llms.txt): the root listing, ingested through its twin\n"
    );
    let deep_index = concat!(
        "# Acme Deep\n\n> The deep subtree.\n\n",
        "## Pages\n\n",
        "- [Page](https://docs.acme.test/deep/page.html) (rel=\"alternate\" type=\"text/markdown\" https://docs.acme.test/deep-twin.md): the deeper listing carries a different twin\n"
    );
    let page_html = "<!doctype html><html><body>the page</body></html>";
    let shallow =
        "# Page\n\nThe shallow twin body carries the alpha marker.\n\n## Alpha\n\nAlpha steps.\n";
    let deep =
        "# Page\n\nThe deep twin body carries the omega marker.\n\n## Omega\n\nOmega steps.\n";
    let responses = serde_json::json!([
        { "url": "https://docs.acme.test/llms.txt", "status": 200, "headers": {}, "body": root },
        { "url": "https://docs.acme.test/deep/llms.txt", "status": 200, "headers": {}, "body": deep_index },
        { "url": "https://docs.acme.test/deep/page.html", "status": 200, "headers": {}, "body": page_html },
        { "url": "https://docs.acme.test/root-twin.md", "status": 200, "headers": {}, "body": shallow },
        { "url": "https://docs.acme.test/deep-twin.md", "status": 200, "headers": {}, "body": deep },
    ]);
    let fixture = write_fixture_set(dir.path(), &responses);
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

    assert_eq!(outcome.pages, 1, "the page buffered once, not twice");
    assert_eq!(
        outcome.events.subpath_indexes, 1,
        "the describedby hint queued the deep index"
    );
    assert_eq!(
        outcome.events.markdown_twins_used, 1,
        "the replacement does not double-count the twin"
    );
    assert_eq!(outcome.skipped_pages, 0);
    let hits = store
        .search_fts(&devdocs_store::build_match_query("omega"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "the deeper twin's body is what remains");
    let hits = store
        .search_fts(&devdocs_store::build_match_query("alpha"), 10)
        .unwrap();
    assert!(hits.is_empty(), "the shallow twin's body is replaced");
}
