//! Ingest errors (docs/implementation/07 §Adapter trait `IngestError`).
//!
//! The CLI edge maps these onto the documented exit codes; refusals
//! (license gate, guard policy) become `FETCH_BLOCKED`, failures become
//! `UPSTREAM_ERROR` (docs/implementation/08 §Error codes).

use thiserror::Error as ThisError;

use crate::guard::GuardError;

/// Why an ingest run failed.
#[derive(Debug, ThisError)]
pub enum IngestError {
    /// The index carries zero links; the picker falls to the next rung
    /// (docs/implementation/07 §llms.txt adapter rule 5).
    #[error("LLMS_TXT_EMPTY: no link lines in {url}")]
    LlmsTxtEmpty {
        /// The index URL.
        url: String,
    },
    /// The index does not follow the llms.txt v2 grammar
    /// (docs/implementation/07 §llms.txt adapter rules 1 and 2).
    #[error("LLMS_TXT_INVALID: {reason} ({url})")]
    LlmsTxtInvalid {
        /// The index URL.
        url: String,
        /// What failed in the grammar.
        reason: String,
    },
    /// A fetch-guard refusal or failure (docs/implementation/09).
    #[error("fetch guard: {0}")]
    Guard(#[from] GuardError),
    /// The license gate or another model-layer policy refused the run.
    #[error("{0}")]
    License(#[from] devdocs_model::Error),
    /// The tokenizer could not load or the limits are inconsistent.
    #[error("tokenizer: {message}")]
    Tokenizer {
        /// What failed.
        message: String,
    },
    /// The fixture manifest was unreadable or invalid.
    #[error("fixture {path}: {message}")]
    Fixture {
        /// The manifest path.
        path: String,
        /// What failed.
        message: String,
    },
    /// The store refused or failed the publish.
    #[error("store: {0}")]
    Store(#[from] devdocs_store::StoreError),
    /// The ingest run exceeded the 600 s total wall-time cap.
    #[error("the ingest run exceeded the {seconds}s total cap")]
    TotalTimeout {
        /// The configured cap in seconds.
        seconds: u64,
    },
    /// A blocking task failed to join.
    #[error("blocking task failed: {0}")]
    Join(String),
    /// The index carried links but none was an ingestable markdown page.
    #[error("no ingestable markdown pages linked from {index}")]
    NoIngestablePages {
        /// The index URL.
        index: String,
    },
}
