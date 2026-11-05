//! Errors the eval harness reports, and the exit codes they map to.
//!
//! Exit code 2 is not an error: it is the gate-failure outcome of a run
//! that completed (docs/implementation/14 §Harness flow). Invalid input
//! maps to 1; operational failures map to 3.

use std::path::PathBuf;

use devdocs_store::StoreError;

/// Everything that can abort a run before the gate table prints.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A suite file broke a cross-question rule, or a sidecar or baseline
    /// file is bad.
    #[error("invalid suite {suite}: {reason}")]
    InvalidSuite {
        /// The suite name.
        suite: String,
        /// What is wrong, in one line.
        reason: String,
    },
    /// A suite line is not a valid question.
    #[error("suite {suite} line {line}: {reason}")]
    SuiteParse {
        /// The suite name.
        suite: String,
        /// The 1-based file line the question sits on.
        line: usize,
        /// What is wrong, in one line.
        reason: String,
    },
    /// The database could not be opened, migrated, or read.
    #[error("store failure: {0}")]
    Store(
        #[from]
        #[source]
        StoreError,
    ),
    /// The `devdocs` binary for the subprocess pass is missing, or a spawn
    /// failed.
    #[error("subprocess failure: {0}")]
    Subprocess(
        /// What failed, in one line.
        String,
    ),
    /// The retrieval pipeline failed outside the store: a broken request
    /// shape or hash drift the harness's own inputs cannot cause.
    #[error("retrieval failure: {0}")]
    Retrieve(
        /// What failed, in one line.
        String,
    ),
    /// A file could not be read or written.
    #[error("cannot {action} {path}: {source}")]
    Io {
        /// What the harness tried to do, for example `read`.
        action: &'static str,
        /// The file the action touched.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// An artifact could not be serialized.
    #[error("cannot serialize {what}: {source}")]
    Serialize {
        /// Which artifact failed, for example `results.json`.
        what: &'static str,
        /// The underlying serialization failure.
        source: serde_json::Error,
    },
}

impl EvalError {
    /// The process exit code for this error (docs/implementation/14).
    ///
    /// `1` marks invalid input a caller can fix; `3` marks an operational
    /// failure. Gate failures are not errors: `main` exits `2` for them.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidSuite { .. } | Self::SuiteParse { .. } => 1,
            Self::Store(_)
            | Self::Subprocess(_)
            | Self::Retrieve(_)
            | Self::Io { .. }
            | Self::Serialize { .. } => 3,
        }
    }
}
