//! Shared test helpers and fixtures for devdocs.
//!
//! Fixtures return the underlying error: the lint rules that carry no
//! panic allowance apply to this library root like any other, and the
//! calling test files unwrap into their own local allowance.

pub mod fixtures;

use std::path::Path;

use rusqlite::Connection;

use devdocs_store::{Store, StoreError};

/// Opens a store at `path` and applies every pending migration.
pub fn open_migrated(path: &Path) -> Result<Store, StoreError> {
    let store = Store::open(path)?;
    store.migrate()?;
    Ok(store)
}

/// Inserts one source row (`llms_txt`, `approved_index`) and returns its id.
pub fn seed_source(conn: &Connection, library: &str, active: bool) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
         VALUES (?1, '1.0.0', 'llms_txt', ?2, 'approved_index', '2026-01-01T00:00:00Z', ?3)",
        rusqlite::params![
            library,
            format!("https://example.test/{library}"),
            i64::from(active)
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Inserts one chunk row with a fixed hash and returns its id.
pub fn seed_chunk(
    conn: &Connection,
    source_id: i64,
    ordinal: i64,
    section_path: &str,
    symbol: Option<&str>,
    text: &str,
) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO chunks(source_id, content_hash, section_path, symbol, text, token_count, ordinal) \
         VALUES (?1, ?2, ?3, ?4, ?5, 8, ?6)",
        rusqlite::params![
            source_id,
            &[7u8; 32][..],
            section_path,
            symbol,
            text,
            ordinal
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// One chunk row read back from a store file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChunk {
    /// The source the chunk belongs to.
    pub source_id: i64,
    /// The library and version of that source.
    pub library: String,
    /// The version of that source.
    pub version: String,
    /// The raw sha256 bytes of the chunk body.
    pub content_hash: Vec<u8>,
    /// The breadcrumb JSON array.
    pub section_path: String,
    /// The stored text with the contextual header.
    pub text: String,
    /// The stored token count.
    pub token_count: i64,
    /// The chunk position inside the source.
    pub ordinal: i64,
}

/// Reads every chunk row of every active source, joined with its source
/// row, ordered by library, version, and ordinal.
///
/// The store-level assertions in the CLI tests read through this helper so
/// no test touches rusqlite directly.
pub fn read_chunks(path: &Path) -> Result<Vec<StoredChunk>, rusqlite::Error> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT c.source_id, s.library, s.version, c.content_hash, c.section_path, c.text, \
                c.token_count, c.ordinal \
         FROM chunks c JOIN sources s ON s.source_id = c.source_id \
         WHERE s.active = 1 \
         ORDER BY s.library, s.version, c.ordinal",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredChunk {
            source_id: row.get(0)?,
            library: row.get(1)?,
            version: row.get(2)?,
            content_hash: row.get(3)?,
            section_path: row.get(4)?,
            text: row.get(5)?,
            token_count: row.get(6)?,
            ordinal: row.get(7)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use rusqlite::Connection;

    use super::{StoredChunk, open_migrated, read_chunks};

    /// Seeded rows read back in order, and a deactivated source drops out.
    #[test]
    fn read_chunks_returns_active_rows_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("docs.db");
        {
            let _store = open_migrated(&db).unwrap();
        }
        let conn = Connection::open(&db).unwrap();
        let first = super::seed_source(&conn, "acme", true).unwrap();
        let second = super::seed_source(&conn, "beta", true).unwrap();
        super::seed_chunk(&conn, first, 0, "Guide", None, "acme chunk one").unwrap();
        super::seed_chunk(&conn, second, 0, "Guide", None, "beta chunk one").unwrap();
        drop(conn);

        let chunks = read_chunks(&db).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].library, "acme");
        assert_eq!(chunks[1].library, "beta");
        assert_eq!(chunks[0].token_count, 8);
        assert!(chunks.iter().all(|chunk: &StoredChunk| chunk.ordinal == 0));

        // Deactivating the source removes its chunks from the read.
        {
            let store = open_migrated(&db).unwrap();
            store
                .withdraw_source_by_origin_url(
                    "https://example.test/acme",
                    devdocs_model::LicenseStatus::Denied,
                )
                .unwrap();
        }
        let chunks = read_chunks(&db).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].library, "beta");
    }
}
