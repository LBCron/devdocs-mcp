//! Source listings: the `devdocs list` discovery surface
//! (docs/implementation/08 §CLI contract).

use devdocs_model::SourceSummary;

use crate::error::StoreError;
use crate::store::Store;

impl Store {
    /// Lists every source row: library, version, kind, license stage, the
    /// active flag, and the capture commit when the adapter records one,
    /// ordered by library then version.
    ///
    /// # Errors
    /// [`StoreError::Sql`] when the query fails.
    pub fn list_sources(&self) -> Result<Vec<SourceSummary>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut statement = conn.prepare(
            "SELECT library, version, kind, license, license_status, active, fetched_at, \
             license_expires_at, commit_sha FROM sources ORDER BY library, version",
        )?;
        let rows = statement.query_map([], |row| {
            let kind_raw: String = row.get(2)?;
            let Some(kind) = devdocs_model::SourceKind::parse(&kind_raw) else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    format!("unknown source kind {kind_raw:?}").into(),
                ));
            };
            let status_raw: String = row.get(4)?;
            let Some(status) = devdocs_model::LicenseStatus::parse(&status_raw) else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    format!("unknown license status {status_raw:?}").into(),
                ));
            };
            let active: i64 = row.get(5)?;
            Ok(SourceSummary {
                library: row.get(0)?,
                version: row.get(1)?,
                kind: kind.as_str().to_owned(),
                license: row.get(3)?,
                license_status: status.as_str().to_owned(),
                active: active != 0,
                fetched_at: row.get(6)?,
                license_expires_at: row.get(7)?,
                commit_sha: row.get(8)?,
            })
        })?;
        let mut sources = Vec::new();
        for row in rows {
            sources.push(row?);
        }
        Ok(sources)
    }
}
