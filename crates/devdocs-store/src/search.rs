//! FTS5 search and symbol lookup over schema 0001.
//!
//! Ranking uses the documented per-column weights
//! `bm25(chunks_fts, 5.0, 1.0)` — the title column (breadcrumb plus symbol)
//! outranks the body 5 to 1 (docs/implementation/06 §FTS5 design decisions).

use rusqlite::params;

use crate::error::StoreError;
use crate::store::Store;

/// The WHERE clause that drops sources with a terminal license refusal out
/// of retrieval at query time (docs/implementation/09 §License gate
/// enforcement points, point 3). One string, shared by every serving
/// query through the `{license_clause}` placeholder in the SQL consts.
const TERMINAL_LICENSE_CLAUSE: &str =
    "AND s.license_status NOT IN ('denied', 'expired', 'revoked')";

/// One search or lookup result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The chunk row (`chunks.chunk_id`).
    pub chunk_id: i64,
    /// The source the chunk belongs to.
    pub source_id: i64,
    /// Normalized library name.
    pub library: String,
    /// Exact version served.
    pub version: String,
    /// Breadcrumb of section titles.
    pub section_path: Vec<String>,
    /// Exact symbol this chunk documents, when one applies.
    pub symbol: Option<String>,
    /// The `symbols.kind` of the matched symbol entry, when the hit came
    /// through the symbol table.
    pub symbol_kind: Option<String>,
    /// `snippet()` FTS fragment; `None` for symbol and full-chunk reads.
    pub snippet: Option<String>,
    /// Full chunk text.
    pub text: String,
    /// Token count of `text`.
    pub token_count: u32,
    /// bm25 rank (lower is better); `None` for non-FTS reads.
    pub rank: Option<f64>,
    /// The stored sha256 of the chunk body (`chunks.content_hash`).
    pub content_hash: [u8; 32],
    /// The source's upstream origin URL.
    pub source_url: String,
    /// The upstream commit the source was captured at, when known.
    pub source_commit: Option<String>,
    /// The source's license approval stage.
    pub license_status: String,
}

/// Reads a `chunks.section_path` JSON string array out of a row column.
fn section_path(
    row: &rusqlite::Row<'_>,
    idx: usize,
    chunk_id: i64,
) -> rusqlite::Result<Vec<String>> {
    let raw: String = row.get(idx)?;
    serde_json::from_str(&raw).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "chunk {chunk_id}: section_path is not a JSON string array: {source}"
            ))),
        )
    })
}

/// The chunk and source columns both queries share, in a fixed order:
/// `chunk_id, source_id, library, version, section_path, symbol, text,
/// token_count, content_hash, origin_url, commit_sha, license_status` at
/// indices 0..12.
struct BaseColumns {
    chunk_id: i64,
    source_id: i64,
    library: String,
    version: String,
    section_path: Vec<String>,
    symbol: Option<String>,
    text: String,
    token_count: u32,
    content_hash: [u8; 32],
    source_url: String,
    source_commit: Option<String>,
    license_status: String,
}

/// Reads [`BaseColumns`] from a row laid out at the shared column order.
fn base_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<BaseColumns> {
    let chunk_id: i64 = row.get(0)?;
    Ok(BaseColumns {
        chunk_id,
        source_id: row.get(1)?,
        library: row.get(2)?,
        version: row.get(3)?,
        section_path: section_path(row, 4, chunk_id)?,
        symbol: row.get(5)?,
        text: row.get(6)?,
        token_count: row.get(7)?,
        content_hash: hash_bytes(row, 8)?,
        source_url: row.get(9)?,
        source_commit: row.get(10)?,
        license_status: row.get(11)?,
    })
}

/// Reads a `chunks.content_hash` BLOB into the fixed-size sha256 array.
fn hash_bytes(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<[u8; 32]> {
    let raw: Vec<u8> = row.get(idx)?;
    let bytes: [u8; 32] = raw.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::other("content_hash is not 32 bytes")),
        )
    })?;
    Ok(bytes)
}

/// Maps an FTS row laid out at the shared column order to a [`Hit`] with
/// its `snippet()` fragment and bm25 rank (indices 12, 13).
fn fts_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let base = base_columns(row)?;
    Ok(Hit {
        chunk_id: base.chunk_id,
        source_id: base.source_id,
        library: base.library,
        version: base.version,
        section_path: base.section_path,
        symbol: base.symbol,
        symbol_kind: None,
        snippet: Some(row.get(12)?),
        text: base.text,
        token_count: base.token_count,
        rank: Some(row.get(13)?),
        content_hash: base.content_hash,
        source_url: base.source_url,
        source_commit: base.source_commit,
        license_status: base.license_status,
    })
}

/// Maps a non-FTS row (symbol lookups, full-chunk reads) laid out at the
/// shared column order to a [`Hit`] with no snippet and no rank.
fn plain_hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let base = base_columns(row)?;
    Ok(Hit {
        chunk_id: base.chunk_id,
        source_id: base.source_id,
        library: base.library,
        version: base.version,
        section_path: base.section_path,
        symbol: base.symbol,
        symbol_kind: row.get(12)?,
        snippet: None,
        text: base.text,
        token_count: base.token_count,
        rank: None,
        content_hash: base.content_hash,
        source_url: base.source_url,
        source_commit: base.source_commit,
        license_status: base.license_status,
    })
}

/// The unscoped search SQL: the chunk/source join with the documented
/// bm25 weights, ordered and limited. The `{license_clause}` placeholder
/// takes the shared terminal-license filter
/// ([`TERMINAL_LICENSE_CLAUSE`]) so sources whose license state turned
/// terminal drop out at query time
/// (docs/implementation/09 §License gate enforcement points, point 3).
const FTS_SQL: &str = "SELECT c.chunk_id, c.source_id, s.library, s.version, c.section_path, \
     c.symbol, \
     c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, s.license_status, \
     snippet(chunks_fts, 1, '<mark>', '</mark>', '…', 16), \
     bm25(chunks_fts, 5.0, 1.0) \
     FROM chunks_fts \
     JOIN chunks c ON c.chunk_id = chunks_fts.rowid \
     JOIN sources s ON s.source_id = c.source_id \
     WHERE chunks_fts MATCH ?1 AND s.active = 1 \
     {license_clause} \
     ORDER BY bm25(chunks_fts, 5.0, 1.0) \
     LIMIT ?2";

/// The library-scoped search SQL: the same projection with the library
/// predicate before the limit. The `{license_clause}` placeholder takes
/// the shared terminal-license filter
/// ([`TERMINAL_LICENSE_CLAUSE`]) — the same drop the unscoped query
/// applies.
const FTS_LIBRARY_SQL: &str = "SELECT c.chunk_id, c.source_id, s.library, s.version, \
     c.section_path, \
     c.symbol, \
     c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, s.license_status, \
     snippet(chunks_fts, 1, '<mark>', '</mark>', '…', 16), \
     bm25(chunks_fts, 5.0, 1.0) \
     FROM chunks_fts \
     JOIN chunks c ON c.chunk_id = chunks_fts.rowid \
     JOIN sources s ON s.source_id = c.source_id \
     WHERE chunks_fts MATCH ?1 AND s.active = 1 \
     {license_clause} \
     AND s.library = ?3 COLLATE NOCASE \
     ORDER BY bm25(chunks_fts, 5.0, 1.0) \
     LIMIT ?2";

/// The source-scoped search SQL: the same projection with the source-id
/// predicate before the limit — the exact-version serving path.
const FTS_SOURCE_SQL: &str = "SELECT c.chunk_id, c.source_id, s.library, s.version, \
     c.section_path, \
     c.symbol, \
     c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, s.license_status, \
     snippet(chunks_fts, 1, '<mark>', '</mark>', '…', 16), \
     bm25(chunks_fts, 5.0, 1.0) \
     FROM chunks_fts \
     JOIN chunks c ON c.chunk_id = chunks_fts.rowid \
     JOIN sources s ON s.source_id = c.source_id \
     WHERE chunks_fts MATCH ?1 AND s.active = 1 \
     {license_clause} \
     AND s.source_id = ?3 \
     ORDER BY bm25(chunks_fts, 5.0, 1.0) \
     LIMIT ?2";

/// The exact-symbol lookup SQL: the symbol join with the same
/// terminal-license clause, ordered by chunk position.
const SYMBOL_SQL: &str = "SELECT c.chunk_id, c.source_id, s.library, s.version, \
     c.section_path, \
     c.symbol, \
     c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, s.license_status, \
     sym.kind \
     FROM symbols sym \
     JOIN chunks c ON c.chunk_id = sym.chunk_id \
     JOIN sources s ON s.source_id = c.source_id \
     WHERE sym.symbol = ?1 AND c.source_id = ?2 AND s.active = 1 \
     {license_clause} \
     ORDER BY c.ordinal";

/// The full-chunk read SQL: every chunk of one source in ordinal order,
/// for the `open` path. No FTS match, so no snippet and no rank.
const SOURCE_CHUNKS_SQL: &str = "SELECT c.chunk_id, c.source_id, s.library, s.version, \
     c.section_path, \
     c.symbol, \
     c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, s.license_status, \
     NULL \
     FROM chunks c \
     JOIN sources s ON s.source_id = c.source_id \
     WHERE c.source_id = ?1 AND s.active = 1 \
     {license_clause} \
     ORDER BY c.ordinal";

/// The source lookup SQL: the active source id for one exact
/// `(library, version)` pair, case-insensitive on the library.
const SOURCE_SQL: &str = "SELECT source_id FROM sources \
     WHERE library = ?1 COLLATE NOCASE AND version = ?2 AND active = 1 \
     ORDER BY source_id \
     LIMIT 1";

/// The version listing SQL: every active source of one library, newest
/// publication first.
const LIBRARY_SQL: &str = "SELECT DISTINCT s.version FROM sources s \
     WHERE s.library = ?1 COLLATE NOCASE AND s.active = 1 \
     ORDER BY s.version DESC";

impl Store {
    /// Full-text search over active sources with the documented bm25
    /// weights. A source whose license state turned terminal drops out at
    /// query time (docs/implementation/09 §License gate enforcement
    /// points, point 3), even when its withdrawal never ran.
    ///
    /// `match_expr` should come from the query builder — it is bound as a
    /// parameter, never interpolated.
    pub fn search_fts(&self, match_expr: &str, limit: u32) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = FTS_SQL.replace("{license_clause}", TERMINAL_LICENSE_CLAUSE);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit], fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Full-text search scoped to one library, with the same bm25 weights
    /// and ordering as [`Store::search_fts`].
    ///
    /// The library predicate applies before the limit, so the limit counts
    /// same-library hits only. The comparison is case-insensitive
    /// (`COLLATE NOCASE`), matching the retrieval-side filter it replaces.
    pub fn search_fts_library(
        &self,
        match_expr: &str,
        library: &str,
        limit: u32,
    ) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = FTS_LIBRARY_SQL.replace("{license_clause}", TERMINAL_LICENSE_CLAUSE);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit, library], fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Exact-symbol lookup over active sources.
    pub fn lookup_symbol(
        &self,
        symbol: &str,
        src: devdocs_model::SourceId,
    ) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = SYMBOL_SQL.replace("{license_clause}", TERMINAL_LICENSE_CLAUSE);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![symbol, src.0], plain_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Full-text search scoped to one source id — the exact-version
    /// serving path. Same weights and ordering as [`Store::search_fts`].
    pub fn search_fts_source(
        &self,
        match_expr: &str,
        source_id: i64,
        limit: u32,
    ) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = FTS_SOURCE_SQL.replace("{license_clause}", TERMINAL_LICENSE_CLAUSE);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit, source_id], fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Every chunk of one source in ordinal order — the `open` read path.
    pub fn source_chunks(&self, source_id: i64) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = SOURCE_CHUNKS_SQL.replace("{license_clause}", TERMINAL_LICENSE_CLAUSE);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![source_id], plain_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// The active source id for one exact `(library, version)` pair,
    /// case-insensitive on the library; `None` when no active source
    /// serves that pair.
    pub fn find_source(&self, library: &str, version: &str) -> Result<Option<i64>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(SOURCE_SQL)?;
        let mut rows = stmt.query(params![library, version])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// The exact versions one active library serves, in descending version
    /// order.
    pub fn library_versions(&self, library: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(LIBRARY_SQL)?;
        let rows = stmt.query_map(params![library], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(StoreError::Sql)
    }
}
