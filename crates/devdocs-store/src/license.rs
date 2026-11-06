//! License state transitions, the `meta` audit trail, and the expiry
//! sweep (docs/implementation/07 §License gate).
//!
//! The store is authoritative for the mutable license state. Every
//! transition runs in one transaction — state check, state update, audit
//! line — so an illegal move leaves no trace and a legal one is never
//! half-recorded.

use rusqlite::params;
use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;

use devdocs_model::LicenseStatus;

use crate::error::StoreError;
use crate::store::Store;

/// The operator recorded on audit lines the sweep writes itself.
const AUTO_EXPIRY_OPERATOR: &str = "auto:expiry";

impl Store {
    /// Moves every source row of `library` to the license state `to` and
    /// appends one audit line to `meta`. One transaction: load the current
    /// states, check each against the transition table
    /// ([`LicenseStatus::can_transition_to`]), require a non-empty note
    /// exactly for `denied → pending_review`, update the rows, and write
    /// the audit line in the docs/implementation/07 format.
    ///
    /// An illegal move fails with [`StoreError::InvalidTransition`] and
    /// leaves the stored state unchanged with no audit line. Returns the
    /// number of rows moved. The library matches case-insensitively, and
    /// the audit line carries the canonical stored spelling of the
    /// library.
    ///
    /// # Errors
    /// [`StoreError::UnknownLibrary`] when no source row carries the
    /// library; [`StoreError::InvalidTransition`] when any current state
    /// cannot move to `to`; [`StoreError::MissingNote`] when the re-opening
    /// transition carries no note; [`StoreError::Sql`] on query failures.
    pub fn transition_license(
        &self,
        library: &str,
        to: LicenseStatus,
        operator: &str,
        note: Option<&str>,
    ) -> Result<usize, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let states = current_states(&tx, library)?;
        if states.is_empty() {
            return Err(StoreError::UnknownLibrary {
                library: library.to_owned(),
            });
        }
        for from in &states {
            if !from.can_transition_to(to) {
                return Err(StoreError::InvalidTransition { from: *from, to });
            }
        }
        let note = required_note(&states, to, note)?;

        let moved = tx
            .execute(
                "UPDATE sources SET license_status = ?1 WHERE library = ?2 COLLATE NOCASE",
                params![to.as_str(), library],
            )
            .map_err(StoreError::Sql)?;

        // The audit line carries the canonical stored spelling of the
        // library, not the caller's casing.
        let canonical: String = tx
            .query_row(
                "SELECT library FROM sources WHERE library = ?1 COLLATE NOCASE LIMIT 1",
                params![library],
                |row| row.get(0),
            )
            .map_err(StoreError::Sql)?;

        let from = joined_states(&states);
        append_audit(&tx, &canonical, &from, to.as_str(), operator, note)?;
        tx.commit().map_err(StoreError::Sql)?;
        Ok(moved)
    }

    /// Flips every source whose recorded expiry has passed to `expired`
    /// (docs/implementation/07 §License gate: the recorded expiry passes).
    /// One transaction: rows with a non-null `license_expires_at` at or
    /// before `now` (an RFC 3339 instant) that do not already sit in a
    /// terminal refusal move to `expired`, one audit line each with the
    /// `auto:expiry` operator. Rows already `expired`, `denied`, or
    /// `revoked` stay put, and a row without an expiry never flips.
    /// Returns the number of rows flipped.
    ///
    /// # Errors
    /// [`StoreError::Sql`] on query failures.
    pub fn sweep_expired(&self, now: &str) -> Result<usize, StoreError> {
        let mut conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let mut statement = tx.prepare(
            "SELECT source_id, library, license_status FROM sources \
             WHERE license_expires_at IS NOT NULL AND license_expires_at <= ?1 \
             AND license_status NOT IN ('expired', 'denied', 'revoked')",
        )?;
        let due: Vec<(i64, String, String)> = statement
            .query_map(params![now], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(StoreError::Sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Sql)?;
        drop(statement);

        for (source_id, library, from) in &due {
            let Some(from) = LicenseStatus::parse(from) else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    format!("unknown license status {from:?}").into(),
                )
                .into());
            };
            tx.execute(
                "UPDATE sources SET license_status = 'expired' WHERE source_id = ?1",
                params![source_id],
            )
            .map_err(StoreError::Sql)?;
            append_audit(
                &tx,
                library,
                from.as_str(),
                LicenseStatus::Expired.as_str(),
                AUTO_EXPIRY_OPERATOR,
                None,
            )?;
        }
        tx.commit().map_err(StoreError::Sql)?;
        Ok(due.len())
    }
}

impl Store {
    /// The distinct license states the library's rows currently carry,
    /// case-insensitive on the library, ordered by the storage spelling.
    /// An un-indexed library returns an empty vector.
    ///
    /// # Errors
    /// [`StoreError::Sql`] on query failures, or a conversion failure when
    /// a row carries an unknown status string.
    pub fn license_states(&self, library: &str) -> Result<Vec<LicenseStatus>, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut statement = conn.prepare(
            "SELECT DISTINCT license_status FROM sources \
             WHERE library = ?1 COLLATE NOCASE ORDER BY license_status",
        )?;
        let rows = statement
            .query_map(params![library], |row| row.get::<_, String>(0))
            .map_err(StoreError::Sql)?;
        let mut states = Vec::new();
        for row in rows {
            let raw = row.map_err(StoreError::Sql)?;
            let Some(state) = LicenseStatus::parse(&raw) else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown license status {raw:?}").into(),
                )
                .into());
            };
            states.push(state);
        }
        Ok(states)
    }

    /// Writes the expiry date onto every row of `library` — the
    /// `--expires` flag of `devdocs source approve`
    /// (docs/implementation/07 §License gate). `None` clears the recorded
    /// expiry. The next read-path sweep decides whether a written past
    /// date flips the rows to `expired`. Returns the row count.
    ///
    /// # Errors
    /// [`StoreError::Sql`] on the update failure.
    pub fn set_license_expires_at(
        &self,
        library: &str,
        expires: Option<&str>,
    ) -> Result<usize, StoreError> {
        let conn = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        conn.execute(
            "UPDATE sources SET license_expires_at = ?2 \
             WHERE library = ?1 COLLATE NOCASE",
            params![library, expires],
        )
        .map_err(StoreError::Sql)
    }
}

/// The distinct license states the library's rows currently carry.
fn current_states(
    tx: &rusqlite::Transaction<'_>,
    library: &str,
) -> Result<Vec<LicenseStatus>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT DISTINCT license_status FROM sources \
         WHERE library = ?1 COLLATE NOCASE ORDER BY license_status",
    )?;
    let rows = statement
        .query_map(params![library], |row| row.get::<_, String>(0))
        .map_err(StoreError::Sql)?;
    let mut states = Vec::new();
    for row in rows {
        let raw = row.map_err(StoreError::Sql)?;
        let Some(state) = LicenseStatus::parse(&raw) else {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown license status {raw:?}").into(),
            )
            .into());
        };
        states.push(state);
    }
    Ok(states)
}

/// Validates the note requirement: `denied → pending_review` needs a
/// non-empty note for the audit trail; every other transition needs none
/// but may carry one. Returns the note to record.
fn required_note<'a>(
    states: &'a [LicenseStatus],
    to: LicenseStatus,
    note: Option<&'a str>,
) -> Result<Option<&'a str>, StoreError> {
    let reopens = states.contains(&LicenseStatus::Denied) && to == LicenseStatus::PendingReview;
    let trimmed = note.map(str::trim).filter(|note| !note.is_empty());
    if reopens && trimmed.is_none() {
        return Err(StoreError::MissingNote);
    }
    Ok(trimmed)
}

/// Renders the `from` side of an audit line: one state, or the distinct
/// set joined with `|` when the library's rows disagree.
fn joined_states(states: &[LicenseStatus]) -> String {
    states
        .iter()
        .map(|state| state.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// Appends one audit line to `meta` at the current instant.
fn append_audit(
    tx: &rusqlite::Transaction<'_>,
    library: &str,
    from: &str,
    to: &str,
    operator: &str,
    note: Option<&str>,
) -> Result<(), StoreError> {
    let ts = OffsetDateTime::now_utc()
        .replace_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(StoreError::Timestamp)?;
    append_audit_at(tx, &ts, library, from, to, operator, note)
}

/// Appends one audit line to `meta`: the key is `audit:<rfc3339 nanosecond
/// timestamp>` — a same-instant collision falls back to a `-<n>` suffix —
/// and the value is the docs/implementation/07 format
/// `(audit:<ts>, library, from, to, operator[, note])`.
fn append_audit_at(
    tx: &rusqlite::Transaction<'_>,
    ts: &str,
    library: &str,
    from: &str,
    to: &str,
    operator: &str,
    note: Option<&str>,
) -> Result<(), StoreError> {
    let base_key = format!("audit:{ts}");
    let mut key = base_key.clone();
    let mut suffix = 0;
    loop {
        let taken: i64 = tx
            .query_row(
                "SELECT count(*) FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .map_err(StoreError::Sql)?;
        if taken == 0 {
            break;
        }
        suffix += 1;
        key = format!("{base_key}-{suffix}");
    }

    let mut value = format!("(audit:{ts}, {library}, {from}, {to}, {operator}");
    if let Some(note) = note {
        value.push_str(", ");
        value.push_str(note);
    }
    value.push(')');

    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(StoreError::Sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::BTreeSet;

    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    use super::{AUTO_EXPIRY_OPERATOR, Store};
    use crate::migrate::MigrateOptions;
    use crate::store::StoreOptions;
    use devdocs_model::LicenseStatus::{
        self, ApprovedCache, ApprovedEmbed, ApprovedFetch, ApprovedIndex, ApprovedSnippet, Denied,
        Expired, PendingReview, Revoked,
    };

    /// A migrated store plus its kept-alive temp directory.
    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_with(&dir.path().join("docs.db"), &StoreOptions::default()).unwrap();
        store.migrate().unwrap();
        (store, dir)
    }

    /// Inserts one source row straight into the store: the transition
    /// tests drive states the publish gate would refuse. Rows of the same
    /// library need distinct versions to satisfy the UNIQUE key.
    fn insert_source(
        store: &Store,
        library: &str,
        version: &str,
        status: LicenseStatus,
        expires: Option<&str>,
    ) {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sources(library, version, kind, origin_url, license_status, \
             license_expires_at, fetched_at, active) \
             VALUES (?1, ?2, 'llms_txt', 'https://docs.test/llms.txt', ?3, ?4, \
             '2026-01-01T00:00:00Z', 0)",
            rusqlite::params![library, version, status.as_str(), expires],
        )
        .unwrap();
    }

    /// The stored license state of the library's single row.
    fn state_of(store: &Store, library: &str) -> LicenseStatus {
        let conn = store.conn.lock().unwrap();
        let raw: String = conn
            .query_row(
                "SELECT license_status FROM sources WHERE library = ?1",
                [library],
                |row| row.get(0),
            )
            .unwrap();
        LicenseStatus::parse(&raw).unwrap()
    }

    /// The audit values in `meta`, in key order.
    fn audit_lines(store: &Store) -> Vec<String> {
        let conn = store.conn.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT value FROM meta WHERE key LIKE 'audit:%' ORDER BY key")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    /// Migration 0002 applies once: `user_version` lands on 2 and a
    /// re-run applies nothing (docs/implementation/06 §Migrations).
    #[test]
    fn migration_0002_applies_once() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_with(&dir.path().join("docs.db"), &StoreOptions::default()).unwrap();
        assert_eq!(store.user_version().unwrap(), 0);

        let applied = store.migrate().unwrap();
        let names: Vec<_> = applied.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"0002_license_gate"), "names: {names:?}");
        assert_eq!(store.user_version().unwrap(), 2);

        let reapplied = store
            .migrate_with_options(&MigrateOptions::default())
            .unwrap();
        assert!(reapplied.is_empty(), "re-run must be a no-op");
    }

    /// Every legal transition moves the row and appends exactly one audit
    /// line carrying the docs/07 fields: timestamp, library, from, to,
    /// operator.
    #[test]
    fn every_legal_transition_moves_and_audits() {
        let non_terminal = [
            PendingReview,
            ApprovedFetch,
            ApprovedIndex,
            ApprovedEmbed,
            ApprovedCache,
            ApprovedSnippet,
        ];
        let mut legal: Vec<(LicenseStatus, LicenseStatus)> = Vec::new();
        for from in non_terminal {
            for to in [Denied, Expired, Revoked] {
                legal.push((from, to));
            }
        }
        legal.extend([
            (PendingReview, ApprovedFetch),
            (ApprovedFetch, ApprovedIndex),
            (ApprovedIndex, ApprovedEmbed),
            (ApprovedEmbed, ApprovedCache),
            (ApprovedCache, ApprovedSnippet),
            // The re-opening move needs its note.
            (Denied, PendingReview),
        ]);
        assert_eq!(legal.len(), 24);

        for (from, to) in legal {
            let (store, _dir) = store();
            insert_source(&store, "acme", "1.0", from, None);
            let note = if (from, to) == (Denied, PendingReview) {
                Some("robots allowlist re-checked")
            } else {
                None
            };
            let moved = store
                .transition_license("acme", to, "op", note)
                .unwrap_or_else(|e| panic!("{from:?} → {to:?}: {e}"));
            assert_eq!(moved, 1, "{from:?} → {to:?}");
            assert_eq!(state_of(&store, "acme"), to, "{from:?} → {to:?}");

            let lines = audit_lines(&store);
            assert_eq!(lines.len(), 1, "{from:?} → {to:?}");
            let line = &lines[0];
            assert!(line.starts_with("(audit:"), "line: {line}");
            assert!(line.contains(", acme, "), "line: {line}");
            assert!(
                line.contains(&format!(", {}, {}, ", from.as_str(), to.as_str())),
                "line: {line}"
            );
            assert!(line.contains(", op"), "line: {line}");
            assert!(line.ends_with(')'), "line: {line}");
            // The re-opening move is the only one that arrives with a note
            // here; every other audit line ends at the operator.
            assert_eq!(
                line.contains(", robots allowlist re-checked"),
                (from, to) == (Denied, PendingReview),
                "line: {line}"
            );
        }
    }

    /// Every illegal transition errors with the typed refusal, leaves the
    /// stored state unchanged, and writes no audit line (AC2).
    #[test]
    fn every_illegal_transition_refuses_without_a_trace() {
        let all = [
            PendingReview,
            ApprovedFetch,
            ApprovedIndex,
            ApprovedEmbed,
            ApprovedCache,
            ApprovedSnippet,
            Denied,
            Expired,
            Revoked,
        ];
        let non_terminal = [
            PendingReview,
            ApprovedFetch,
            ApprovedIndex,
            ApprovedEmbed,
            ApprovedCache,
            ApprovedSnippet,
        ];
        for from in all {
            for to in all {
                let legal = (from, to) == (Denied, PendingReview)
                    || (matches!(to, Denied | Expired | Revoked) && non_terminal.contains(&from))
                    || matches!(
                        (from, to),
                        (PendingReview, ApprovedFetch)
                            | (ApprovedFetch, ApprovedIndex)
                            | (ApprovedIndex, ApprovedEmbed)
                            | (ApprovedEmbed, ApprovedCache)
                            | (ApprovedCache, ApprovedSnippet)
                    );
                if legal {
                    continue;
                }
                let (store, _dir) = store();
                insert_source(&store, "acme", "1.0", from, None);
                let error = store
                    .transition_license("acme", to, "op", Some("note"))
                    .unwrap_err();
                match error {
                    crate::error::StoreError::InvalidTransition {
                        from: error_from,
                        to: error_to,
                    } => {
                        assert_eq!(error_from, from, "{from:?} → {to:?}");
                        assert_eq!(error_to, to);
                    }
                    other => panic!("{from:?} → {to:?}: expected InvalidTransition, got {other:?}"),
                }
                assert_eq!(state_of(&store, "acme"), from, "{from:?} → {to:?}");
                assert!(
                    audit_lines(&store).is_empty(),
                    "{from:?} → {to:?} must write no audit line"
                );
            }
        }
    }

    /// `denied → pending_review` without a note errors and changes
    /// nothing; with a note the note lands in the audit line.
    #[test]
    fn reopening_a_denied_source_requires_a_note() {
        let (store, _dir) = store();
        insert_source(&store, "acme", "1.0", Denied, None);

        let error = store
            .transition_license("acme", PendingReview, "op", None)
            .unwrap_err();
        assert!(
            matches!(error, crate::error::StoreError::MissingNote),
            "error: {error:?}"
        );
        assert_eq!(state_of(&store, "acme"), Denied);
        assert_eq!(audit_lines(&store), Vec::<String>::new());

        let error = store
            .transition_license("acme", PendingReview, "op", Some("   "))
            .unwrap_err();
        assert!(matches!(error, crate::error::StoreError::MissingNote));

        store
            .transition_license("acme", PendingReview, "op", Some("ToS clarified"))
            .unwrap();
        assert_eq!(state_of(&store, "acme"), PendingReview);
        let lines = audit_lines(&store);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(", ToS clarified)"), "line: {}", lines[0]);
    }

    /// A transition naming an unindexed library errors and writes nothing.
    #[test]
    fn unknown_library_refuses() {
        let (store, _dir) = store();
        let error = store
            .transition_license("ghost", PendingReview, "op", Some("note"))
            .unwrap_err();
        assert!(
            matches!(error, crate::error::StoreError::UnknownLibrary { .. }),
            "error: {error:?}"
        );
        assert_eq!(audit_lines(&store), Vec::<String>::new());
    }

    /// A transition carries an optional note on any legal move.
    #[test]
    fn any_transition_may_carry_a_note() {
        let (store, _dir) = store();
        insert_source(&store, "acme", "1.0", PendingReview, None);
        store
            .transition_license("acme", Denied, "op", Some("robots disallow"))
            .unwrap();
        let lines = audit_lines(&store);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(", robots disallow)"),
            "line: {}",
            lines[0]
        );
    }

    /// `set_license_expires_at` writes the date onto every row of the
    /// library (case-insensitive), `None` clears it, and an unknown
    /// library writes nothing.
    #[test]
    fn set_license_expires_at_updates_every_row_of_the_library() {
        let (store, _dir) = store();
        insert_source(&store, "acme", "1.0", ApprovedIndex, None);
        insert_source(&store, "acme", "2.0", ApprovedIndex, None);
        assert_eq!(
            store
                .set_license_expires_at("ACME", Some("2999-01-01"))
                .unwrap(),
            2
        );
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE license_expires_at = '2999-01-01'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(count, 2);
        assert_eq!(store.set_license_expires_at("acme", None).unwrap(), 2);
        assert_eq!(
            store
                .set_license_expires_at("ghost", Some("2999-01-01"))
                .unwrap(),
            0
        );
    }

    /// The sweep flips a past-expiry row to `expired` with the
    /// `auto:expiry` audit line, leaves future and null expiries alone,
    /// and skips rows already in a terminal refusal (AC3, AC6 store half).
    #[test]
    fn sweep_flips_only_passed_non_terminal_expiries() {
        let (store, _dir) = store();
        insert_source(&store, "past", "1.0", ApprovedIndex, Some("2026-01-01"));
        insert_source(&store, "future", "1.0", ApprovedIndex, Some("2999-01-01"));
        insert_source(&store, "no-expiry", "1.0", ApprovedIndex, None);
        insert_source(
            &store,
            "already-expired",
            "1.0",
            ApprovedIndex,
            Some("2026-01-01"),
        );
        store
            .transition_license("already-expired", Expired, "op", None)
            .unwrap();
        insert_source(
            &store,
            "already-denied",
            "1.0",
            ApprovedIndex,
            Some("2026-01-01"),
        );
        store
            .transition_license("already-denied", Denied, "op", None)
            .unwrap();
        insert_source(
            &store,
            "already-revoked",
            "1.0",
            ApprovedIndex,
            Some("2026-01-01"),
        );
        store
            .transition_license("already-revoked", Revoked, "op", None)
            .unwrap();

        let flipped = store.sweep_expired("2026-11-06T12:00:00Z").unwrap();
        assert_eq!(flipped, 1, "only the past-expiry row flips");
        assert_eq!(state_of(&store, "past"), Expired);
        assert_eq!(state_of(&store, "future"), ApprovedIndex);
        assert_eq!(state_of(&store, "no-expiry"), ApprovedIndex);
        assert_eq!(state_of(&store, "already-expired"), Expired);
        assert_eq!(state_of(&store, "already-denied"), Denied);
        assert_eq!(state_of(&store, "already-revoked"), Revoked);

        let lines = audit_lines(&store);
        // One operator transition line per already-terminal setup (3) plus
        // exactly one auto:expiry line.
        let auto: Vec<_> = lines
            .iter()
            .filter(|l| l.contains(AUTO_EXPIRY_OPERATOR))
            .collect();
        assert_eq!(auto.len(), 1, "lines: {lines:?}");
        assert!(
            auto[0].contains("(audit:") && auto[0].contains(", past, approved_index, expired, "),
            "line: {}",
            auto[0]
        );

        // The sweep is idempotent: the second run flips nothing.
        assert_eq!(store.sweep_expired("2026-11-06T12:00:00Z").unwrap(), 0);
        let auto: BTreeSet<_> = audit_lines(&store)
            .into_iter()
            .filter(|l| l.contains(AUTO_EXPIRY_OPERATOR))
            .collect();
        assert_eq!(auto.len(), 1, "no duplicate auto:expiry line");
    }

    /// `license_states` reports the distinct states of a library's rows
    /// case-insensitively and returns empty for an un-indexed library.
    #[test]
    fn license_states_reports_distinct_states() {
        let (store, _dir) = store();
        assert_eq!(
            store.license_states("acme").unwrap(),
            Vec::<LicenseStatus>::new()
        );
        insert_source(&store, "acme", "1.0", ApprovedIndex, None);
        insert_source(&store, "acme", "2.0", ApprovedSnippet, None);
        assert_eq!(
            store.license_states("ACME").unwrap(),
            vec![ApprovedIndex, ApprovedSnippet]
        );
    }

    /// Same-instant audit lines collide on the key: the `-<n>` suffix
    /// keeps both rows (driven through `append_audit_at` with a fixed
    /// instant; the wall clock moves between real transitions).
    #[test]
    fn same_instant_audit_lines_get_a_suffix() {
        let (store, _dir) = store();
        insert_source(&store, "one", "1.0", ApprovedIndex, None);
        insert_source(&store, "two", "1.0", ApprovedIndex, None);
        {
            let mut conn = store.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            super::append_audit_at(
                &tx,
                "2026-11-06T12:00:00.1Z",
                "one",
                "approved_index",
                "expired",
                "auto:expiry",
                None,
            )
            .unwrap();
            super::append_audit_at(
                &tx,
                "2026-11-06T12:00:00.1Z",
                "two",
                "approved_index",
                "expired",
                "auto:expiry",
                None,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let conn = store.conn.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT key FROM meta WHERE key LIKE 'audit:%' ORDER BY key")
            .unwrap();
        let keys: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(keys.len(), 2, "keys: {keys:?}");
        assert!(
            keys[0].starts_with("audit:") && keys[1].starts_with(&format!("{}-", keys[0])),
            "the second key must suffix the first: {keys:?}"
        );
    }

    /// The expiry boundary is 00:00 UTC of the recorded date: the
    /// date-only stored value compares string-wise against the full
    /// RFC 3339 instant, so a row flips on the recorded date itself —
    /// expiry = today flips, expiry = tomorrow does not, expiry =
    /// yesterday flips (docs/implementation/07 §Expiry sweep).
    #[test]
    fn expiry_boundary_flips_at_0000_utc_of_the_recorded_date() {
        let today = OffsetDateTime::now_utc().date();
        let yesterday = today.previous_day().unwrap();
        let tomorrow = today.next_day().unwrap();
        let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();

        let (store, _dir) = store();
        insert_source(
            &store,
            "today",
            "1.0",
            ApprovedIndex,
            Some(&today.to_string()),
        );
        insert_source(
            &store,
            "yesterday",
            "1.0",
            ApprovedIndex,
            Some(&yesterday.to_string()),
        );
        insert_source(
            &store,
            "tomorrow",
            "1.0",
            ApprovedIndex,
            Some(&tomorrow.to_string()),
        );

        let flipped = store.sweep_expired(&now).unwrap();
        assert_eq!(flipped, 2, "today and yesterday flip; tomorrow does not");
        assert_eq!(state_of(&store, "today"), Expired);
        assert_eq!(state_of(&store, "yesterday"), Expired);
        assert_eq!(state_of(&store, "tomorrow"), ApprovedIndex);
    }

    /// The license path matches the library case-insensitively: a
    /// transition naming `ACME` moves the store row `acme`, and the audit
    /// line carries the canonical stored spelling
    /// (docs/implementation/07 §License gate).
    #[test]
    fn transition_license_matches_the_library_case_insensitively() {
        let (store, _dir) = store();
        insert_source(&store, "acme", "1.0", ApprovedIndex, None);

        let moved = store
            .transition_license("ACME", ApprovedEmbed, "op", None)
            .unwrap();
        assert_eq!(moved, 1);
        assert_eq!(state_of(&store, "acme"), ApprovedEmbed);

        let lines = audit_lines(&store);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(", acme, approved_index, approved_embed, "),
            "canonical spelling in the line: {}",
            lines[0]
        );
        assert!(
            !lines[0].contains(", ACME, "),
            "the caller casing must not appear: {}",
            lines[0]
        );
    }
}
