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
    /// The tokenizer could not load or the limits are inconsistent.
    #[error("tokenizer: {message}")]
    Tokenizer {
        /// What failed.
        message: String,
    },
}
