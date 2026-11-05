//! FTS5 search and symbol lookup over schema 0001.
//!
//! Ranking uses the documented per-column weights
//! `bm25(chunks_fts, 5.0, 1.0)` — the title column (breadcrumb plus symbol)
//! outranks the body 5 to 1 (docs/implementation/06 §FTS5 design decisions).

use rusqlite::params;

use crate::error::StoreError;
use crate::store::Store;

/// The placeholder the serving queries carry for the terminal-license
/// filter; [`fts_sql`] and [`plain_sql`] substitute
/// [`TERMINAL_LICENSE_CLAUSE`] for it when they compose the final SQL.
const LICENSE_PLACEHOLDER: &str = "{license_clause}";

/// The WHERE clause that drops sources with a terminal license refusal out
/// of retrieval at query time (docs/implementation/09 §License gate
/// enforcement points, point 3). One string, shared by every serving
/// query through the [`LICENSE_PLACEHOLDER`] in the composed SQL.
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

/// The chunk and source column list every serving query shares, in the
/// fixed order the row readers ([`base_columns`]) index: `chunk_id`,
/// `source_id`, `library`, `version`, `section_path`, `symbol`, `text`,
/// `token_count`, `content_hash`, `origin_url`, `commit_sha`,
/// `license_status` at indices 0..12. Each query appends its
/// query-specific suffix columns behind them.
const BASE_COLUMNS: &str = "c.chunk_id, c.source_id, s.library, s.version, c.section_path, \
     c.symbol, c.text, c.token_count, c.content_hash, s.origin_url, s.commit_sha, \
     s.license_status";

/// The FTS suffix columns behind [`BASE_COLUMNS`]: the `snippet()` fragment
/// at index 12 and the bm25 rank at index 13, with the documented weights.
const FTS_COLUMNS: &str = "snippet(chunks_fts, 1, '<mark>', '</mark>', '…', 16), \
     bm25(chunks_fts, 5.0, 1.0)";

/// The FROM/JOIN of the FTS queries: the fts table driving the chunk and
/// source joins.
const FTS_FROM: &str = "chunks_fts \
     JOIN chunks c ON c.chunk_id = chunks_fts.rowid \
     JOIN sources s ON s.source_id = c.source_id";

/// The WHERE clause shared by the three FTS shapes, before the scoping
/// predicate: the FTS match plus the active-source filter, then the
/// `{license_clause}` placeholder ([`TERMINAL_LICENSE_CLAUSE`]) that drops
/// terminal-license sources at query time
/// (docs/implementation/09 §License gate enforcement points, point 3).
const FTS_MATCH_CLAUSE: &str = "chunks_fts MATCH ?1 AND s.active = 1 {license_clause}";

/// The library-scoped predicate, bound to `?3`, appended to
/// [`FTS_MATCH_CLAUSE`] before the limit — so the limit counts
/// same-library hits only. `COLLATE NOCASE` matches the retrieval-side
/// filter it replaces.
const FTS_LIBRARY_PREDICATE: &str = " AND s.library = ?3 COLLATE NOCASE";

/// The version-scoped predicate, bound to `?3`, appended to
/// [`FTS_MATCH_CLAUSE`] before the limit — the version-only fallback
/// path, where the request names a version but no library
/// (docs/implementation/08 §Version resolution, step 4). The comparison
/// is exact, like [`SOURCE_SQL`].
const FTS_VERSION_PREDICATE: &str = " AND s.version = ?3";

/// The source-scoped predicate, bound to `?3`, appended to
/// [`FTS_MATCH_CLAUSE`] before the limit — the exact-version serving path.
const FTS_SOURCE_PREDICATE: &str = " AND s.source_id = ?3";

/// The FROM/JOIN of the symbol lookup: the symbol table driving the chunk
/// and source joins.
const SYMBOL_FROM: &str = "symbols sym \
     JOIN chunks c ON c.chunk_id = sym.chunk_id \
     JOIN sources s ON s.source_id = c.source_id";

/// The symbol-lookup WHERE clause: the exact symbol of one source, active
/// only, with the terminal-license placeholder.
const SYMBOL_CLAUSE: &str =
    "sym.symbol = ?1 AND c.source_id = ?2 AND s.active = 1 {license_clause}";

/// How a symbol query matches the stored `symbols.symbol` values, from the
/// strongest shape down (docs/implementation/08 §Retrieval ladder, the
/// symbol exact step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolMatch {
    /// The whole identifier, byte-exact.
    Exact,
    /// The identifier starts with the query, case-sensitively — the
    /// completion shape (`useR` does not match `useRouter`).
    CaseSensitivePrefix,
    /// The identifier equals the query or starts with it,
    /// case-insensitively; exact matches rank before prefixes.
    CaseInsensitive,
}

/// The source narrowing of a symbol query, the
/// [`Store::search_fts_library`]/[`Store::search_fts_source`] shapes
/// carried as one argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolScope {
    /// Every active source.
    All,
    /// Active sources of one library, case-insensitive on the name.
    Library(String),
    /// One exact source id.
    Source(i64),
}

/// Escapes `prefix` for a SQLite `GLOB` pattern head, so the identifier
/// characters `[`, `*`, and `?` match themselves.
fn glob_escape(prefix: &str) -> String {
    prefix
        .replace('[', "[[]")
        .replace('*', "[*]")
        .replace('?', "[?]")
}

/// Escapes `prefix` for a SQLite `LIKE` pattern head under the `\` escape,
/// so `%` and `_` in an identifier match themselves.
fn like_escape(prefix: &str) -> String {
    prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Store {
    /// The symbol-lookup ladder over active sources: matches `symbol` per
    /// `mode`, narrowed to `scope`, best match quality first with ties in
    /// the lower chunk id. Returns at most `limit` hits, and the
    /// terminal-license clause drops refused sources at query time
    /// (docs/implementation/09 §License gate enforcement points, point 3).
    pub fn search_symbols(
        &self,
        symbol: &str,
        mode: SymbolMatch,
        scope: SymbolScope,
        limit: u32,
    ) -> Result<Vec<Hit>, StoreError> {
        // `?1` is always the raw identifier (equality and match quality),
        // `?2` the limit; the escaped match pattern sits at `?3` in the
        // prefix modes, and the scope value takes the next index.
        let (predicate, order, uses_pattern) = match mode {
            SymbolMatch::Exact => (String::from("sym.symbol = ?1"), String::new(), false),
            SymbolMatch::CaseSensitivePrefix => (
                String::from("sym.symbol GLOB (?3 || '*')"),
                String::from("(sym.symbol = ?1) DESC, "),
                true,
            ),
            SymbolMatch::CaseInsensitive => (
                String::from(
                    "sym.symbol = ?1 COLLATE NOCASE \
                     OR sym.symbol LIKE (?3 || '%') ESCAPE '\\'",
                ),
                String::from("(sym.symbol = ?1 COLLATE NOCASE) DESC, "),
                true,
            ),
        };
        let mut bind = vec![
            rusqlite::types::Value::from(symbol.to_owned()),
            rusqlite::types::Value::from(i64::from(limit)),
        ];
        if uses_pattern {
            let pattern = match mode {
                SymbolMatch::CaseSensitivePrefix => glob_escape(symbol),
                SymbolMatch::CaseInsensitive => like_escape(symbol),
                SymbolMatch::Exact => unreachable!("uses_pattern is false for Exact"),
            };
            bind.push(rusqlite::types::Value::from(pattern));
        }
        let scope_idx = if uses_pattern { 4 } else { 3 };
        // Deterministic in the mode and scope only, so `prepare_cached`
        // keeps one entry per mode and scope variant.
        let (scope_clause, scope_value) = match scope {
            SymbolScope::All => (String::new(), None),
            SymbolScope::Library(library) => (
                format!(" AND s.library = ?{scope_idx} COLLATE NOCASE"),
                Some(rusqlite::types::Value::from(library)),
            ),
            SymbolScope::Source(source_id) => (
                format!(" AND s.source_id = ?{scope_idx}"),
                Some(rusqlite::types::Value::from(source_id)),
            ),
        };
        let sql = format!(
            "SELECT {BASE_COLUMNS}, sym.kind \
             FROM {SYMBOL_FROM} \
             WHERE ({predicate}) AND s.active = 1 {LICENSE_PLACEHOLDER}{scope_clause} \
             ORDER BY {order}c.chunk_id \
             LIMIT ?2"
        )
        .replace(LICENSE_PLACEHOLDER, TERMINAL_LICENSE_CLAUSE);
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(&sql)?;
        if let Some(value) = scope_value {
            bind.push(value);
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), plain_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }
}

/// The FROM/JOIN of the full-chunk read: chunk and source, no symbol table.
const SOURCE_CHUNKS_FROM: &str = "chunks c \
     JOIN sources s ON s.source_id = c.source_id";

/// The full-chunk-read WHERE clause: every chunk of one active source,
/// with the terminal-license placeholder.
const SOURCE_CHUNKS_CLAUSE: &str = "c.source_id = ?1 AND s.active = 1 {license_clause}";

/// Composes one FTS query from a WHERE clause: the shared projection, the
/// FTS join, the clause, and the bm25 ordering with the limit. Deterministic
/// in `where_clause`, so the same shape yields the same SQL string on every
/// call and `prepare_cached` stays effective.
fn fts_sql(where_clause: &str) -> String {
    format!(
        "SELECT {BASE_COLUMNS}, {FTS_COLUMNS} \
         FROM {FTS_FROM} \
         WHERE {where_clause} \
         ORDER BY bm25(chunks_fts, 5.0, 1.0) \
         LIMIT ?2"
    )
    .replace(LICENSE_PLACEHOLDER, TERMINAL_LICENSE_CLAUSE)
}

/// Composes the pinned-source-set clause: [`FTS_MATCH_CLAUSE`] plus
/// `s.source_id IN (…)` with one placeholder per source id behind `?3`.
/// Deterministic per set size, so the same size yields the same SQL string
/// on every call and `prepare_cached` stays effective — one cache entry
/// per set size.
fn fts_source_set_clause(count: usize) -> String {
    // Explicit indices, like [`FTS_LIBRARY_PREDICATE`]: the IN list sits
    // before the `LIMIT ?2` tail, so bare `?` placeholders would take
    // indices 2 and up and collide with the limit's.
    let placeholders = (3..)
        .take(count)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{FTS_MATCH_CLAUSE} AND s.source_id IN ({placeholders})")
}

/// Composes one ordinal-ordered query (symbol lookups, full-chunk reads):
/// the shared projection, one query-specific suffix column at index 12,
/// the FROM/JOIN, and the clause. Deterministic in its arguments, so the
/// same shape yields the same SQL string on every call and
/// `prepare_cached` stays effective.
fn plain_sql(from_clause: &str, where_clause: &str, suffix_column: &str) -> String {
    format!(
        "SELECT {BASE_COLUMNS}, {suffix_column} \
         FROM {from_clause} \
         WHERE {where_clause} \
         ORDER BY c.ordinal"
    )
    .replace(LICENSE_PLACEHOLDER, TERMINAL_LICENSE_CLAUSE)
}

/// The source lookup SQL: the lowest active source id for one exact
/// `(library, version)` pair, case-insensitive on the library. The
/// `open`/`verify` ref paths resolve through it so one ref always serves
/// the same source.
const SOURCE_SQL: &str = "SELECT source_id FROM sources \
     WHERE library = ?1 COLLATE NOCASE AND version = ?2 AND active = 1 \
     ORDER BY source_id \
     LIMIT 1";

/// The source-set lookup SQL: every active source id for one exact
/// `(library, version)` pair, case-insensitive on the library, lowest
/// first. Several active origins per pair are representable
/// (docs/implementation/06-data-model.md, §Schema).
const SOURCES_SQL: &str = "SELECT source_id FROM sources \
     WHERE library = ?1 COLLATE NOCASE AND version = ?2 AND active = 1 \
     ORDER BY source_id";

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
        let sql = fts_sql(FTS_MATCH_CLAUSE);
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
        let sql = fts_sql(&format!("{FTS_MATCH_CLAUSE}{FTS_LIBRARY_PREDICATE}"));
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
        let sql = plain_sql(SYMBOL_FROM, SYMBOL_CLAUSE, "sym.kind");
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
        let sql = fts_sql(&format!("{FTS_MATCH_CLAUSE}{FTS_SOURCE_PREDICATE}"));
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit, source_id], fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Full-text search scoped to one exact version across every active
    /// source that serves it — the version-only fallback path, where the
    /// request names a version but no library. Same weights and ordering
    /// as [`Store::search_fts`].
    pub fn search_fts_version(
        &self,
        match_expr: &str,
        version: &str,
        limit: u32,
    ) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = fts_sql(&format!("{FTS_MATCH_CLAUSE}{FTS_VERSION_PREDICATE}"));
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit, version], fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Full-text search scoped to the given source ids — the
    /// pinned-version serving path over every origin that serves the
    /// `(library, version)` pair. Same weights and ordering as
    /// [`Store::search_fts`]. An empty set serves nothing.
    pub fn search_fts_sources(
        &self,
        match_expr: &str,
        source_ids: &[i64],
        limit: u32,
    ) -> Result<Vec<Hit>, StoreError> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = fts_sql(&fts_source_set_clause(source_ids.len()));
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut bind = Vec::with_capacity(source_ids.len() + 2);
        bind.push(rusqlite::types::Value::from(match_expr.to_owned()));
        bind.push(rusqlite::types::Value::from(i64::from(limit)));
        bind.extend(
            source_ids
                .iter()
                .map(|id| rusqlite::types::Value::from(*id)),
        );
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), fts_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// Every chunk of one source in ordinal order — the `open` read path.
    pub fn source_chunks(&self, source_id: i64) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let sql = plain_sql(SOURCE_CHUNKS_FROM, SOURCE_CHUNKS_CLAUSE, "NULL");
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![source_id], plain_hit)?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }

    /// The active source id for one exact `(library, version)` pair,
    /// case-insensitive on the library; `None` when no active source
    /// serves that pair. When several active origins serve the pair
    /// (docs/implementation/06-data-model.md, §Schema), the lowest
    /// source id returns, so one ref always serves one source.
    pub fn find_source(&self, library: &str, version: &str) -> Result<Option<i64>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(SOURCE_SQL)?;
        let mut rows = stmt.query(params![library, version])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Every active source id for one exact `(library, version)` pair,
    /// case-insensitive on the library, lowest first. The schema's UNIQUE
    /// (library, version, kind, `origin_url`) key allows several active
    /// origins per pair, and the pinned search scope serves them all.
    pub fn find_sources(&self, library: &str, version: &str) -> Result<Vec<i64>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(SOURCES_SQL)?;
        let rows = stmt.query_map(params![library, version], |row| row.get(0))?;
        rows.collect::<Result<Vec<i64>, _>>()
            .map_err(StoreError::Sql)
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
