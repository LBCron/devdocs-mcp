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
