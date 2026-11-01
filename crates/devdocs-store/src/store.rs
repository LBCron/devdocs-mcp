//! The store handle: one SQLite connection behind a mutex.
//!
//! Contracts: docs/implementation/06 §Connection pragmas,
//! docs/implementation/09 §Local storage controls, and
//! docs/implementation/05 §Concurrency model (rusqlite lives only inside
//! this crate).

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::OpenFlags;

use crate::error::StoreError;
use crate::fschecks;
use crate::migrate::{self, AppliedMigration};

/// Options for [`Store::open_with`].
#[derive(Debug, Clone, Copy, Default)]
pub struct StoreOptions {
    /// Admits a database path on a network filesystem. The symlink-ownership
    /// refusal has no override (docs/implementation/09).
    pub allow_network_fs: bool,
}

/// One SQLite connection behind a mutex.
pub struct Store {
    pub(crate) conn: Mutex<rusqlite::Connection>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) the database at `path` with the documented
    /// pragmas and path checks.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        Self::open_with(path, &StoreOptions::default())
    }

    /// Opens the database with explicit options.
    pub fn open_with(path: &Path, options: &StoreOptions) -> Result<Store, StoreError> {
        fschecks::check_parent_exists(path)?;
        fschecks::check_symlink_owner(path)?;
        if !options.allow_network_fs {
            fschecks::check_network_mount(path)?;
        }
        fschecks::ensure_private_file(path)?;

        let conn = rusqlite::Connection::open_with_flags(
            path,
            // Single-threaded use per connection: the store mutex serializes
            // all access, so SQLite's own locking layer is redundant.
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(StoreError::Sql)?;

        // Set the busy timeout before any locking-sensitive statement, so
        // concurrent opens and migrations wait instead of failing fast.
        conn.busy_timeout(Duration::from_secs(5))?;
        apply_pragmas(&conn)?;

        fschecks::warn_on_mode_drift(path);
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// Applies every pending migration to the store's connection.
    pub fn migrate(&self) -> Result<Vec<AppliedMigration>, StoreError> {
        let mut guard = self.conn.lock().map_err(|_| StoreError::LockPoisoned)?;
        migrate::migrate(&mut guard)
    }
}

/// Sets the five connection pragmas and verifies the WAL switch by readback.
fn apply_pragmas(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    // PRAGMA journal_mode returns the resulting mode, so the switch is
    // verified by readback; a filesystem without WAL support gets the old
    // mode back and open fails loudly.
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable { mode });
    }

    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 128 * 1024 * 1024)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::Store;

    /// `Store::open` sets the five pragmas from docs/implementation/06 and
    /// the WAL switch survives readback.
    #[test]
    fn open_sets_the_five_pragmas() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("docs.db")).unwrap();
        let conn = store.conn.lock().unwrap();

        let journal: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");

        let synchronous: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 1, "synchronous = NORMAL");

        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let temp_store: i64 = conn
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .unwrap();
        assert_eq!(temp_store, 2, "temp_store = MEMORY");

        let mmap_size: i64 = conn
            .pragma_query_value(None, "mmap_size", |row| row.get(0))
            .unwrap();
        assert_eq!(mmap_size, 128 * 1024 * 1024);
    }
}
