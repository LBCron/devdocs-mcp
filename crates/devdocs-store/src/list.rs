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
            "SELECT library, version, kind, license_status, active, commit_sha \
             FROM sources ORDER BY library, version",
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
            let status_raw: String = row.get(3)?;
            let Some(status) = devdocs_model::LicenseStatus::parse(&status_raw) else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    format!("unknown license status {status_raw:?}").into(),
                ));
            };
            let active: i64 = row.get(4)?;
            Ok(SourceSummary {
                library: row.get(0)?,
                version: row.get(1)?,
                kind: kind.as_str().to_owned(),
                license_status: status.as_str().to_owned(),
                active: active != 0,
                commit_sha: row.get(5)?,
            })
        })?;
        let mut sources = Vec::new();
        for row in rows {
            sources.push(row?);
        }
        Ok(sources)
    }
}
