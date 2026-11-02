//! Migration runner.
//!
//! Contract: docs/implementation/06 §Migrations. Applied migrations never
//! change; a schema change is a new `NNNN_description.sql` file.

use rusqlite::Connection;
use rusqlite::TransactionBehavior;

use crate::error::StoreError;

/// One embedded migration file.
struct EmbeddedMigration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

/// The shipped migrations, in application order.
const MIGRATIONS: &[EmbeddedMigration] = &[EmbeddedMigration {
    version: 1,
    name: "0001_init",
    sql: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/migrations/0001_init.sql"
    )),
}];

/// A migration the runner applied during this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// The `NNNN` version the migration was filed under.
    pub version: u32,
    /// The migration name without the `.sql` suffix, e.g. `"0001_init"`.
    pub name: String,
}

/// Applies every pending migration to `conn` and returns them in order.
///
/// Re-running on an up-to-date database applies nothing and returns an empty
/// vector.
pub fn migrate(conn: &mut Connection) -> Result<Vec<AppliedMigration>, StoreError> {
    apply_all(conn, MIGRATIONS)
}

/// Applies `migrations` in order, skipping versions at or below the current
/// `user_version`. The unlocked read is a fast path for up-to-date
/// databases; [`apply_one`] re-checks the version under the write lock, so
/// a concurrent runner that applied the migration after the read skips it
/// instead of failing on the existing DDL.
fn apply_all(
    conn: &mut Connection,
    migrations: &[EmbeddedMigration],
) -> Result<Vec<AppliedMigration>, StoreError> {
    let current: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(StoreError::Sql)?;
    let mut applied = Vec::new();
    for migration in migrations {
        if i64::from(migration.version) <= current {
            continue;
        }
        if let Some(done) = apply_one(conn, migration)? {
            applied.push(done);
        }
    }
    Ok(applied)
}

/// Applies one migration inside a single transaction: SQL first, then the
/// `user_version` bump, so both land or both roll back.
///
/// Returns `None` when another connection applied this migration between
/// the unlocked read and the transaction: the `Immediate` transaction now
/// holds the write lock, so the re-read `user_version` is authoritative.
fn apply_one(
    conn: &mut Connection,
    migration: &EmbeddedMigration,
) -> Result<Option<AppliedMigration>, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored: i64 = tx
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(StoreError::Sql)?;
    if i64::from(migration.version) <= stored {
        return Ok(None);
    }
    let result = tx
        .execute_batch(migration.sql)
        .and_then(|()| tx.pragma_update(None, "user_version", i64::from(migration.version)));
    if let Err(source) = result {
        // Dropping `tx` rolls back; the DDL and the version bump vanish
        // together.
        return Err(StoreError::Migration {
            version: migration.version,
            source,
        });
    }
    tx.commit().map_err(StoreError::Sql)?;
    Ok(Some(AppliedMigration {
        version: migration.version,
        name: migration.name.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{EmbeddedMigration, apply_all, apply_one};
    use crate::error::StoreError;

    /// The shipped migrations form a contiguous run starting at 1.
    #[test]
    fn shipped_migrations_are_contiguous_from_one() {
        for (index, migration) in super::MIGRATIONS.iter().enumerate() {
            assert_eq!(Ok(migration.version), u32::try_from(index + 1));
        }
    }

    /// A failure inside a migration rolls back its DDL and leaves
    /// `user_version` unchanged.
    #[test]
    fn failed_migration_rolls_back_ddl_and_version() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let good = EmbeddedMigration {
            version: 1,
            name: "0001_good",
            sql: "CREATE TABLE keep_me (id INTEGER);",
        };
        let bad = EmbeddedMigration {
            version: 2,
            name: "0002_bad",
            sql: "CREATE TABLE broken (id INTEGER); INSERT INTO missing_table VALUES (1);",
        };

        let applied = apply_all(&mut conn, &[good]).unwrap();
        assert_eq!(applied.len(), 1);

        let error = apply_all(&mut conn, &[bad]).unwrap_err();
        match error {
            StoreError::Migration { version, .. } => assert_eq!(version, 2),
            other => panic!("expected StoreError::Migration, got {other:?}"),
        }

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1, "user_version must stay at the last good one");
        let broken_left: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'broken'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(broken_left, 0, "failed DDL must roll back");
    }

    /// A runner that read `user_version` before another connection applied
    /// the migration re-checks the version inside its transaction and skips
    /// instead of failing on the existing DDL.
    #[test]
    fn a_stale_version_read_skips_an_already_applied_migration() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = rusqlite::Connection::open(dir.path().join("docs.db")).unwrap();
        let mut second = rusqlite::Connection::open(dir.path().join("docs.db")).unwrap();

        // Another connection wins the race and applies the migration.
        let applied = apply_all(&mut first, super::MIGRATIONS).unwrap();
        assert_eq!(applied.len(), 1);

        // The loser's `apply_all` fast path would still see the migration
        // as pending if its read happened before the winner committed;
        // apply it directly to stand in for that stale state.
        let stale = EmbeddedMigration {
            version: 1,
            name: "0001_init",
            sql: super::MIGRATIONS[0].sql,
        };
        let loser = apply_one(&mut second, &stale).unwrap();
        assert!(loser.is_none(), "the re-check must skip, not re-apply");
    }
}
