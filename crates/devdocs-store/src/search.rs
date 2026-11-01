//! FTS5 search and symbol lookup over schema 0001.
//!
//! Ranking uses the documented per-column weights
//! `bm25(chunks_fts, 5.0, 1.0)` — the title column (breadcrumb plus symbol)
//! outranks the body 5 to 1 (docs/implementation/06 §FTS5 design decisions).

use rusqlite::params;

use crate::error::StoreError;
use crate::store::Store;

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
    /// `snippet()` fragment for FTS hits; `None` for symbol lookups.
    pub snippet: Option<String>,
    /// Full chunk text.
    pub text: String,
    /// Token count of `text`.
    pub token_count: u32,
    /// bm25 rank (lower is better); `None` for symbol lookups.
    pub rank: Option<f64>,
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
/// token_count` at indices 0..8.
struct BaseColumns {
    chunk_id: i64,
    source_id: i64,
    library: String,
    version: String,
    section_path: Vec<String>,
    symbol: Option<String>,
    text: String,
    token_count: u32,
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
    })
}

impl Store {
    /// Full-text search over active sources with the documented bm25
    /// weights.
    ///
    /// `match_expr` should come from the query builder — it is bound as a
    /// parameter, never interpolated.
    pub fn search_fts(&self, match_expr: &str, limit: u32) -> Result<Vec<Hit>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut stmt = conn.prepare_cached(
            "SELECT c.chunk_id, c.source_id, s.library, s.version, c.section_path, c.symbol, \
             c.text, c.token_count, \
             snippet(chunks_fts, 1, '<mark>', '</mark>', '…', 16), \
             bm25(chunks_fts, 5.0, 1.0) \
             FROM chunks_fts \
             JOIN chunks c ON c.chunk_id = chunks_fts.rowid \
             JOIN sources s ON s.source_id = c.source_id \
             WHERE chunks_fts MATCH ?1 AND s.active = 1 \
             ORDER BY bm25(chunks_fts, 5.0, 1.0) \
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_expr, limit], |row| {
            let base = base_columns(row)?;
            Ok(Hit {
                chunk_id: base.chunk_id,
                source_id: base.source_id,
                library: base.library,
                version: base.version,
                section_path: base.section_path,
                symbol: base.symbol,
                symbol_kind: None,
                snippet: Some(row.get(8)?),
                text: base.text,
                token_count: base.token_count,
                rank: Some(row.get(9)?),
            })
        })?;
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
        let mut stmt = conn.prepare_cached(
            "SELECT c.chunk_id, c.source_id, s.library, s.version, c.section_path, c.symbol, \
             c.text, c.token_count, sym.kind \
             FROM symbols sym \
             JOIN chunks c ON c.chunk_id = sym.chunk_id \
             JOIN sources s ON s.source_id = c.source_id \
             WHERE sym.symbol = ?1 AND c.source_id = ?2 AND s.active = 1 \
             ORDER BY c.ordinal",
        )?;
        let rows = stmt.query_map(params![symbol, src.0], |row| {
            let base = base_columns(row)?;
            Ok(Hit {
                chunk_id: base.chunk_id,
                source_id: base.source_id,
                library: base.library,
                version: base.version,
                section_path: base.section_path,
                symbol: base.symbol,
                symbol_kind: Some(row.get(8)?),
                snippet: None,
                text: base.text,
                token_count: base.token_count,
                rank: None,
            })
        })?;
        rows.collect::<Result<Vec<Hit>, _>>()
            .map_err(StoreError::Sql)
    }
}
