//! The retrieval error type.

use devdocs_model::MissReason;

/// The retrieval pipeline's error. The CLI edge maps each variant onto
/// [`devdocs_model::Error`].
#[derive(Debug, thiserror::Error)]
pub enum RetrieveError {
    /// A store query failed (exit 3).
    #[error(transparent)]
    Store(#[from] devdocs_store::StoreError),
    /// A valid request served nothing (exit 2).
    #[error("no result: {0}")]
    Miss(MissReason),
    /// A broken request shape: a malformed ref or cursor (exit 1).
    #[error("usage: {0}")]
    Usage(#[from] UsageError),
    /// The stored content hash no longer matches the re-derived one
    /// (exit 3, docs/implementation/08 §Error codes).
    #[error("hash mismatch: {0}")]
    HashMismatch(String),
}

/// A broken request shape the serve layer detected.
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    /// A `docs://` ref that does not parse.
    #[error("malformed ref {0:?}: expected docs://<library>/v<version>/<path>[#<fragment>]")]
    BadRef(String),
    /// A `--cursor` that does not parse.
    #[error("malformed cursor {0:?}")]
    BadCursor(String),
    /// A cursor naming a chunk the query does not serve.
    #[error("cursor {0} is not in this query's fusion order")]
    UnknownCursor(String),
}

impl From<MissReason> for RetrieveError {
    fn from(reason: MissReason) -> Self {
        Self::Miss(reason)
    }
}
