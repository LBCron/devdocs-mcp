//! The `versions` discovery surface (docs/implementation/08 §CLI
//! contract): only active sources list, and an unknown library misses
//! with `not_indexed`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::MissReason;
use devdocs_retrieve::RetrieveError;
use devdocs_retrieve::serve::versions;
use devdocs_store::Store;
use rusqlite::Connection;
use tempfile::tempdir;

/// Seeds one active and one inactive source of `acme`, plus one active
/// source of another library.
fn seeded() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    devdocs_testkit::seed_source(&conn, "acme", true).unwrap();
    conn.execute(
        "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
         VALUES ('acme', '0.9.0', 'llms_txt', 'https://example.test/acme-0', 'approved_index', \
         '2026-01-01T00:00:00Z', 0)",
        [],
    )
    .unwrap();
    devdocs_testkit::seed_source(&conn, "other", true).unwrap();
    (dir, store)
}

/// Only active sources list; a withdrawn generation is not an available
/// version.
#[test]
fn versions_list_only_active_sources() {
    let (_dir, store) = seeded();
    let envelope = versions(&store, "acme").unwrap();
    let listed: Vec<&str> = envelope
        .results
        .iter()
        .map(|s| s.version.as_str())
        .collect();
    assert_eq!(listed, vec!["1.0.0"]);
}

/// A library with no active sources misses with `not_indexed`.
#[test]
fn an_unknown_library_misses() {
    let (_dir, store) = seeded();
    let error = versions(&store, "missing").unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "unknown library must miss, got {error}"
    );
}
