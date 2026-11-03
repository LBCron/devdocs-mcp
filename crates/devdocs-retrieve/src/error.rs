//! The retrieval error type.

/// The retrieval pipeline's error: everything it can hit is a store
/// failure; the CLI edge maps it onto [`devdocs_model::Error`].
#[derive(Debug, thiserror::Error)]
pub enum RetrieveError {
    /// A store query failed.
    #[error(transparent)]
    Store(#[from] devdocs_store::StoreError),
}
