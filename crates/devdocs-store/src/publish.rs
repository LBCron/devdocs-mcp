//! Atomic generation publish.
//!
//! Contract: docs/implementation/07 §Atomic publish. On any error the
//! transaction rolls back and the previous generation stays queryable.

use rusqlite::TransactionBehavior;
use rusqlite::params;
use time::format_description::well_known::Rfc3339;

use crate::error::StoreError;
use crate::store::Store;

/// One chunk queued for publish.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChunk {
    /// Breadcrumb of section titles.
    pub section_path: Vec<String>,
    /// Every symbol declared in the chunk, stored in the `symbols` table
    /// for the exact-symbol lookup ladder (docs/implementation/06
    /// §Metadata mapping). Duplicates collapse on the `(symbol, chunk_id)`
    /// primary key.
    pub symbols: Vec<Symbol>,
    /// The exact symbol this chunk documents, stored in `chunks.symbol`:
    /// the declared symbol whose name the section header names, else the
    /// first declared one.
    pub symbol: Option<Symbol>,
    /// Body with the contextual header prepended.
    pub text: String,
    /// Token count of `text`.
    pub token_count: u32,
    /// SHA-256 of the canonical body without the contextual header
    /// (docs/implementation/06 §Hashing policy); the caller strips the
    /// header before hashing.
    pub content_hash: [u8; 32],
    /// Position of the chunk inside its source, zero-based.
    pub ordinal: u32,
}

/// A documented symbol and its kind, stored in `chunks.symbol` and
/// `symbols.kind` together.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    /// The exact symbol, e.g. `use_router`.
    pub name: String,
    /// The `symbols.kind` value, e.g. `function`.
    pub kind: String,
}

impl Store {
    /// Replaces one source's chunk set atomically.
    ///
    /// The source row is upserted by its unique key, the old chunk set is
    /// removed, the new chunks and symbols are inserted with the FTS
    /// triggers firing, and the source flips to `active` — all inside one
    /// transaction. The license gate refuses sources below the index
    /// approval stage before any write happens.
    ///
    /// A terminal license state (`denied`, `expired`, `revoked`) additionally
    /// withdraws a previously indexed source: the stored status and the
    /// `active` flip land before the gate error returns, so the source leaves
    /// search results at this enforcement point and not only at retrieve-time
    /// filtering (docs/implementation/09 §License gate enforcement points).
    pub fn publish_generation(
        &self,
        src: &devdocs_model::SourceRecord,
        chunks: &[NewChunk],
    ) -> Result<(), StoreError> {
        if !src.license_status.allows_index() {
            if matches!(
                src.license_status,
                devdocs_model::LicenseStatus::Denied
                    | devdocs_model::LicenseStatus::Expired
                    | devdocs_model::LicenseStatus::Revoked
            ) {
                self.withdraw_source(src)?;
            }
            return Err(StoreError::LicenseNotApproved {
                status: src.license_status,
            });
        }

        let mut conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = publish_inside(&tx, src, chunks);
        match result {
            Ok(()) => tx.commit().map_err(StoreError::Sql),
            Err(error) => {
                // Dropping `tx` rolls back; the old generation stays active
                // and searchable.
                Err(error)
            }
        }
    }

    /// Records the terminal license state and deactivates every source
    /// published from `origin_url`, whatever its library or version. The
    /// add path calls this when the gate refuses a previously indexed
    /// source: the refusal fires before any publish, so the publish-time
    /// withdrawal cannot run. A source that was never published matches no
    /// row. Returns the number of rows withdrawn.
    pub fn withdraw_source_by_origin_url(
        &self,
        origin_url: &str,
        status: devdocs_model::LicenseStatus,
    ) -> Result<usize, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let n = conn
            .execute(
                "UPDATE sources SET license_status = ?1, active = 0 WHERE origin_url = ?2",
                params![status.as_str(), origin_url],
            )
            .map_err(StoreError::Sql)?;
        Ok(n)
    }

    /// Records the terminal license state and deactivates the matching
    /// source row. A source that was never published matches no row, and
    /// the single UPDATE is its own transaction.
    fn withdraw_source(&self, src: &devdocs_model::SourceRecord) -> Result<(), StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        conn.execute(
            "UPDATE sources SET license_status = ?1, active = 0 \
             WHERE library = ?2 AND version = ?3 AND kind = ?4 AND origin_url = ?5",
            params![
                src.license_status.as_str(),
                src.library,
                src.version,
                src.kind.as_str(),
                src.origin_url.as_str(),
            ],
        )
        .map_err(StoreError::Sql)?;
        Ok(())
    }
}

/// Runs the whole publish sequence inside `tx`.
fn publish_inside(
    tx: &rusqlite::Transaction<'_>,
    src: &devdocs_model::SourceRecord,
    chunks: &[NewChunk],
) -> Result<(), StoreError> {
    let source_id: i64 = tx
        .query_row(
            "INSERT INTO sources(library, version, kind, origin_url, commit_sha, license, \
             license_status, fetched_at, active) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0) \
             ON CONFLICT(library, version, kind, origin_url) DO UPDATE SET \
             commit_sha = excluded.commit_sha, license = excluded.license, \
             license_status = excluded.license_status, fetched_at = excluded.fetched_at, \
             active = 0 \
             RETURNING source_id",
            params![
                src.library,
                src.version,
                src.kind.as_str(),
                src.origin_url.as_str(),
                src.commit_sha,
                src.license,
                src.license_status.as_str(),
                rfc3339_utc(src.fetched_at)?,
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::Sql)?;

    // Deactivate the old generation: the chunk delete fires the FTS delete
    // trigger, and symbols plus session_trajectory cascade.
    tx.execute(
        "DELETE FROM chunks WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(StoreError::Sql)?;

    insert_chunks(tx, source_id, chunks)?;

    tx.execute(
        "UPDATE sources SET active = 1 WHERE source_id = ?1",
        params![source_id],
    )
    .map_err(StoreError::Sql)?;
    Ok(())
}

/// Inserts the new chunk set and their symbol rows.
fn insert_chunks(
    tx: &rusqlite::Transaction<'_>,
    source_id: i64,
    chunks: &[NewChunk],
) -> Result<(), StoreError> {
    let mut insert = tx
        .prepare(
            "INSERT INTO chunks(source_id, content_hash, section_path, symbol, text, \
             token_count, ordinal) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(StoreError::Sql)?;
    for chunk in chunks {
        let section_path = serde_json::to_string(&chunk.section_path)?;
        insert
            .execute(params![
                source_id,
                &chunk.content_hash[..],
                section_path,
                chunk.symbol.as_ref().map(|sym| sym.name.as_str()),
                chunk.text,
                i64::from(chunk.token_count),
                i64::from(chunk.ordinal),
            ])
            .map_err(StoreError::Sql)?;
        let chunk_id = tx.last_insert_rowid();
        // `OR IGNORE` on the (symbol, chunk_id) primary key keeps a repeat
        // insert of the same generation a no-op.
        for sym in &chunk.symbols {
            tx.execute(
                "INSERT OR IGNORE INTO symbols(symbol, chunk_id, kind) VALUES (?1, ?2, ?3)",
                params![sym.name, chunk_id, sym.kind],
            )
            .map_err(StoreError::Sql)?;
        }
    }
    Ok(())
}

/// Formats a fetched-at instant as RFC 3339 UTC.
fn rfc3339_utc(instant: time::OffsetDateTime) -> Result<String, StoreError> {
    instant
        .replace_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(StoreError::Timestamp)
}
