//! WAL concurrency behavior on the schema 0001 file.
//!
//! The store wraps one connection; these tests use two raw connections on
//! the same file to exercise the WAL properties the store relies on
//! (docs/implementation/05 §Concurrency model, sqlite.org/wal.html):
//! readers see a consistent snapshot while a writer holds the lock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use rusqlite::Connection;
use tempfile::tempdir;

/// Opens a second raw connection on `path` in WAL mode with a short busy
/// timeout so busy outcomes surface fast and deterministically.
fn raw_wal_connection(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.busy_timeout(Duration::from_millis(50)).unwrap();
    conn
}

/// Sets up schema 0001 plus one source and one committed chunk.
fn seeded(path: &std::path::Path) -> Connection {
    let mut conn = raw_wal_connection(path);
    devdocs_store::migrate(&mut conn).unwrap();
    let source_id = devdocs_testkit::seed_source(&conn, "w", true).unwrap();
    devdocs_testkit::seed_chunk(
        &conn,
        source_id,
        0,
        "[]",
        None,
        "first generation chunk about routing",
    )
    .unwrap();
    conn
}

/// While a writer holds the write transaction, a reader that started before
/// the write keeps seeing its snapshot across the writer's commit, and a
/// fresh read after the snapshot ends sees the committed write.
#[test]
fn reader_sees_a_consistent_snapshot_across_a_commit() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("docs.db");
    let writer = seeded(&path);
    let reader = raw_wal_connection(&path);

    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO chunks(source_id, content_hash, section_path, text, token_count, ordinal) VALUES (1, x'00', '[]', 'second generation chunk', 1, 1); COMMIT;")
        .unwrap();

    // Reader pins its snapshot before the writer's next change lands.
    reader.execute_batch("BEGIN;").unwrap();
    let before: i64 = reader
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, 2);

    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO chunks(source_id, content_hash, section_path, text, token_count, ordinal) VALUES (1, x'00', '[]', 'third generation chunk', 1, 2); COMMIT;")
        .unwrap();

    // The pinned snapshot still reports the pre-commit state.
    let pinned: i64 = reader
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(pinned, 2, "snapshot must not move under the reader");
    reader.execute_batch("COMMIT;").unwrap();

    let after: i64 = reader
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after, 3, "a fresh read sees the committed write");
}

/// A plain read does not block behind a writer's uncommitted transaction:
/// WAL lets readers and the writer proceed side by side.
#[test]
fn a_writer_does_not_block_readers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("docs.db");
    let writer = seeded(&path);
    let reader = raw_wal_connection(&path);

    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO chunks(source_id, content_hash, section_path, text, token_count, ordinal) VALUES (1, x'00', '[]', 'in flight chunk', 1, 1);")
        .unwrap(); // no COMMIT: the write lock is held

    // A reader outside any transaction still gets its snapshot.
    let seen: i64 = reader
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(seen, 1, "the in-flight write stays invisible");

    writer.execute_batch("COMMIT;").unwrap();
}

/// Two writers serialize: the second BEGIN IMMEDIATE reports busy while the
/// first holds the write lock, then succeeds once it commits.
#[test]
fn a_second_writer_reports_busy() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("docs.db");
    let first = seeded(&path);
    let second = raw_wal_connection(&path);

    first.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let busy = second
        .execute_batch("BEGIN IMMEDIATE;")
        .expect_err("the write lock is held by the first writer");
    assert!(
        matches!(busy, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DatabaseBusy),
        "expected DatabaseBusy, got {busy:?}"
    );

    first.execute_batch("COMMIT;").unwrap();
    second.execute_batch("BEGIN IMMEDIATE; COMMIT;").unwrap();
}

/// Two stores can open the same file at once and both migrate: the first
/// applies schema 0001, the second applies nothing.
#[test]
fn concurrent_open_and_migrate_succeed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("docs.db");

    let store_a = devdocs_store::Store::open(&path).unwrap();
    let store_b = devdocs_store::Store::open(&path).unwrap();

    let applied_a = store_a.migrate().unwrap();
    assert_eq!(applied_a.len(), 1, "the first migrator applies schema 0001");
    let applied_b = store_b.migrate().unwrap();
    assert!(applied_b.is_empty(), "the second migrator must be a no-op");
}
