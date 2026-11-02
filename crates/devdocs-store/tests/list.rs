//! Tests for the source listing (docs/08 §CLI contract: `devdocs list`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::source::{LicenseStatus, SourceKind, SourceRecord};
use devdocs_store::NewChunk;
use url::Url;

/// A source record for the tests.
fn record(library: &str, version: &str, url: &str, status: LicenseStatus) -> SourceRecord {
    SourceRecord {
        id: devdocs_model::SourceId(0),
        library: library.to_owned(),
        version: version.to_owned(),
        kind: SourceKind::LlmsTxt,
        origin_url: Url::parse(url).unwrap(),
        commit_sha: None,
        license: Some(String::from("MIT")),
        license_status: status,
        fetched_at: time::OffsetDateTime::now_utc(),
    }
}

/// An empty store lists nothing.
#[test]
fn empty_store_lists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    assert_eq!(store.list_sources().unwrap().len(), 0);
}

/// A published source lists with its kind, stage, and active flag; a
/// second source sorts after the first.
#[test]
fn published_sources_list_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();

    let chunks = [NewChunk {
        section_path: vec![String::from("Guide")],
        symbol: None,
        text: String::from("[a 1] hello"),
        token_count: 4,
        content_hash: devdocs_store::content_hash("hello"),
        ordinal: 0,
    }];
    store
        .publish_generation(
            &record(
                "acme",
                "1.0",
                "https://a.test/llms.txt",
                LicenseStatus::ApprovedIndex,
            ),
            &chunks,
        )
        .unwrap();
    store
        .publish_generation(
            &record(
                "beta",
                "2.0",
                "https://b.test/llms.txt",
                LicenseStatus::ApprovedIndex,
            ),
            &chunks,
        )
        .unwrap();

    let sources = store.list_sources().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].library, "acme");
    assert_eq!(sources[0].version, "1.0");
    assert_eq!(sources[0].kind, "llms_txt");
    assert_eq!(sources[0].license_status, "approved_index");
    assert!(sources[0].active);
    assert_eq!(sources[1].library, "beta");
    assert!(sources[1].active);
}
