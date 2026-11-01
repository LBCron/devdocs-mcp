//! Store error type.

use std::path::PathBuf;

use devdocs_model::LicenseStatus;

/// Everything that can fail inside the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A SQLite call failed.
    #[error("sqlite call failed: {0}")]
    Sql(#[from] rusqlite::Error),

    /// A filesystem call during open failed.
    #[error("filesystem check for {path}: {source}")]
    Io {
        /// The path the failing call inspected.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// Migration `version` failed; the transaction rolled back, so
    /// `user_version` still names the previous schema.
    #[error("migration {version} failed and rolled back: {source}")]
    Migration {
        /// Version of the migration that failed.
        version: u32,
        /// The underlying SQLite error.
        source: rusqlite::Error,
    },

    /// The database path sits on a network filesystem, where SQLite locking
    /// is unreliable (docs/implementation/09 §Local storage controls).
    #[error("database path is on a network filesystem: {path}")]
    NetworkFsPath {
        /// The refused path.
        path: PathBuf,
    },

    /// The database path is a symlink that resolves to a file the current
    /// user does not own (docs/implementation/09 §Local storage controls).
    #[error("database path is a symlink into another user's territory: {path}")]
    ForeignSymlink {
        /// The refused path.
        path: PathBuf,
    },

    /// The connection could not switch to WAL journal mode; readback got
    /// `mode` instead.
    #[error("journal_mode readback was {mode}, not wal")]
    WalUnavailable {
        /// The journal mode the connection reported after the switch.
        mode: String,
    },

    /// The license gate refused a publish because the source sits below the
    /// index approval stage (docs/implementation/09 §License gate).
    #[error("license status {status} does not allow indexing")]
    LicenseNotApproved {
        /// The refusing status.
        status: LicenseStatus,
    },

    /// A chunk in a publish batch could not be serialized for storage.
    #[error("chunk serialization failed: {0}")]
    InvalidChunk(#[from] serde_json::Error),

    /// The fetched-at instant could not be formatted as RFC 3339.
    #[error("timestamp formatting failed: {0}")]
    Timestamp(#[from] time::error::Format),

    /// The store mutex was poisoned by a panic in an earlier operation.
    #[error("store mutex poisoned by an earlier panic")]
    LockPoisoned,
}
