//! Shared test helpers and fixtures for devdocs.
//!
//! Fixtures return the underlying error: the lint rules that carry no
//! panic allowance apply to this library root like any other, and the
//! calling test files unwrap into their own local allowance.

pub mod fixtures;
pub mod git_repo;

use std::path::Path;

use rusqlite::Connection;

use devdocs_store::{Store, StoreError};

/// The shared base test config: the fetch, index, search, and ingest
/// defaults every pipeline test starts from, with no sources — the
/// caller registers its own `[sources.<name>]` entry, so no test writes
/// a `Config` literal inline.
#[must_use]
pub fn base_config() -> devdocs_model::config::Config {
    use devdocs_model::config::{
        Config, DEFAULT_GIT_EXTENSIONS, FetchConfig, IndexConfig, IngestConfig, SearchConfig,
    };

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
        ingest: IngestConfig {
            git_extensions: DEFAULT_GIT_EXTENSIONS
                .iter()
                .map(|ext| (*ext).to_owned())
                .collect(),
        },
        sources: std::collections::BTreeMap::new(),
    }
}

/// Opens a store at `path` and applies every pending migration.
pub fn open_migrated(path: &Path) -> Result<Store, StoreError> {
    let store = Store::open(path)?;
    store.migrate()?;
    Ok(store)
}

/// Opens a migrated store at `path` and seeds one source row
/// (`llms_txt`, origin `https://docs.test/<library>/llms.txt`)
/// with an explicit version, license state, and optional expiry date. The
/// license-gate and transition tests drive states and shapes the publish
/// gate would refuse, so the row lands behind a raw insert. Returns the
/// store.
pub fn seed_source_status(
    path: &Path,
    library: &str,
    version: &str,
    status: &str,
    expires: Option<&str>,
) -> Result<Store, StoreError> {
    let store = open_migrated(path)?;
    let conn = Connection::open(path).map_err(StoreError::Sql)?;
    conn.execute(
        "INSERT INTO sources(library, version, kind, origin_url, license_status, \
         license_expires_at, fetched_at, active) \
         VALUES (?1, ?2, 'llms_txt', ?3, ?4, ?5, '2026-01-01T00:00:00Z', 0)",
        rusqlite::params![
            library,
            version,
            format!("https://docs.test/{library}/llms.txt"),
            status,
            expires,
        ],
    )
    .map_err(StoreError::Sql)?;
    drop(conn);
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

/// The shared test fixture for fusion, budget, and item tests: a minimal
/// deterministic hit with the identity fields filled and every optional
/// payload empty. Tests that need different values mutate the returned
/// struct.
#[must_use]
pub fn sample_hit(chunk_id: i64, token_count: u32) -> devdocs_store::Hit {
    devdocs_store::Hit {
        chunk_id,
        source_id: chunk_id,
        library: "a".to_owned(),
        version: "1.0.0".to_owned(),
        section_path: Vec::new(),
        symbol: None,
        symbol_kind: None,
        snippet: None,
        text: String::new(),
        token_count,
        rank: None,
        content_hash: [0u8; 32],
        source_url: String::new(),
        source_commit: None,
        license_status: "approved_index".to_owned(),
    }
}

/// Seeds one active source named `library` with one chunk (ordinal 0,
/// `section_path_json`, `text`) into a migrated store at `path`, and
/// returns the store with the source id. The seeding connection is
/// dropped before the store is handed back, so tests never import
/// rusqlite.
pub fn seeded_store(
    path: &Path,
    library: &str,
    section_path_json: &str,
    text: &str,
) -> Result<(Store, i64), StoreError> {
    let store = open_migrated(path)?;
    let conn = Connection::open(path).map_err(StoreError::Sql)?;
    let source_id = seed_source(&conn, library, true).map_err(StoreError::Sql)?;
    seed_chunk(&conn, source_id, 0, section_path_json, None, text).map_err(StoreError::Sql)?;
    drop(conn);
    Ok((store, source_id))
}

/// Appends drift to one chunk's stored text behind the store's back,
/// returning the number of drifted rows. The `HASH_MISMATCH` drift test
/// drives this instead of opening a raw connection.
pub fn corrupt_one_chunk_text(path: &Path) -> Result<usize, StoreError> {
    let conn = Connection::open(path).map_err(StoreError::Sql)?;
    let drifted = conn
        .execute(
            "UPDATE chunks SET text = text || ' drifted' \
             WHERE chunk_id IN (SELECT chunk_id FROM chunks LIMIT 1)",
            [],
        )
        .map_err(StoreError::Sql)?;
    Ok(drifted)
}

/// Opens a migrated store at `path`, runs `seed` against a temporary
/// connection to the same file, drops that connection, and returns the
/// store. For seed shapes the dedicated helpers do not cover, such as
/// inactive sources.
pub fn open_migrated_and(
    path: &Path,
    seed: impl FnOnce(&Connection) -> Result<(), rusqlite::Error>,
) -> Result<Store, StoreError> {
    let store = open_migrated(path)?;
    let conn = Connection::open(path).map_err(StoreError::Sql)?;
    seed(&conn).map_err(StoreError::Sql)?;
    drop(conn);
    Ok(store)
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
