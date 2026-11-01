//! Migration runner behavior on a temp database file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_store::migrate;
use rusqlite::Connection;

/// Names of every schema object schema 0001 must create.
const SCHEMA_OBJECTS: &[(&str, &str)] = &[
    ("table", "meta"),
    ("table", "sources"),
    ("table", "chunks"),
    ("table", "symbols"),
    ("table", "session_trajectory"),
    ("table", "chunks_fts"),
    ("view", "chunks_content"),
    ("trigger", "chunks_ai"),
    ("trigger", "chunks_ad"),
    ("index", "idx_sources_library"),
    ("index", "idx_sources_active"),
    ("index", "idx_chunks_symbol"),
    ("index", "idx_chunks_hash"),
    ("index", "idx_symbols_chunk"),
];

/// Applies schema 0001 to a fresh temp file and checks every object.
#[test]
fn fresh_database_reaches_version_one_with_all_objects() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("docs.db")).unwrap();

    let applied = migrate(&mut conn).unwrap();
    assert_eq!(
        applied,
        vec![devdocs_store::AppliedMigration {
            version: 1,
            name: "0001_init".to_owned(),
        }]
    );

    for (kind, name) in SCHEMA_OBJECTS {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                rusqlite::params![kind, name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "{kind} {name} must exist after migration");
    }

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

/// A second run applies nothing, leaves `user_version` alone, and keeps the
/// schema bookkeeping row from the first run.
#[test]
fn re_migrating_an_up_to_date_database_applies_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("docs.db")).unwrap();
    migrate(&mut conn).unwrap();
    let created_at: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_created_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let second = migrate(&mut conn).unwrap();
    assert_eq!(
        second,
        Vec::<devdocs_store::AppliedMigration>::new(),
        "an up-to-date database must get no migrations"
    );

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let created_after: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_created_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_after, created_at, "schema 0001 must not re-run");
}
