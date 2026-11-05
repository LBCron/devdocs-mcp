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
    /// The caller-supplied library does not match the grant's registry
    /// name; approved content must publish under the registry identity
    /// (docs/implementation/07 §License gate).
    #[error("LICENSE_MISMATCH: library `{actual}` does not match the registry entry `{expected}`")]
    LicenseNameMismatch {
        /// The registry name carried by the license grant.
        expected: String,
        /// The library name the caller passed.
        actual: String,
    },
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
    /// The source produced no ingestable pages: an index with no markdown
    /// links, or a git tree with no allowlisted file.
    #[error("no ingestable pages for {index}")]
    NoIngestablePages {
        /// The index URL.
        index: String,
    },
    /// The `git` executable is not on `PATH`; git sources cannot ingest
    /// (docs/implementation/07 §Git adapter).
    #[error("GIT_BINARY_MISSING: the git executable was not found on PATH")]
    GitBinaryMissing,
    /// A git pin starts with `-`, which would read as a git option on the
    /// argv vector (docs/implementation/09 §Git subprocess safety).
    #[error("GIT_INVALID_PIN: pin `{pin}` starts with `-`")]
    GitInvalidPin {
        /// The rejected pin.
        pin: String,
    },
    /// The URL at fetch differs from the planned URL; a changed URL aborts
    /// the run (docs/implementation/09 §Git subprocess safety).
    #[error("GIT_URL_MISMATCH: fetch URL `{actual}` does not match the planned URL `{planned}`")]
    GitUrlMismatch {
        /// The URL the plan pinned.
        planned: String,
        /// The URL the caller passed to fetch.
        actual: String,
    },
    /// A git invocation exited non-zero; the error carries a bounded
    /// stderr tail, never a full dump (docs/implementation/07 §Git
    /// adapter).
    #[error("GIT_COMMAND_FAILED: `{command}` failed: {stderr}")]
    GitCommand {
        /// The argv of the failed invocation (without the config args).
        command: String,
        /// The bounded stderr tail.
        stderr: String,
    },
    /// Spawning git failed for a reason other than a missing binary.
    #[error("GIT_SPAWN_FAILED: {message}")]
    GitSpawn {
        /// What failed.
        message: String,
    },
    /// The private temp clone directory could not be created or does not
    /// carry mode 0700 (docs/implementation/09 §Local storage controls).
    #[error("GIT_TEMP_DIR: {message}")]
    GitTempDir {
        /// What failed.
        message: String,
    },
    /// The docs walk over the clone failed.
    #[error("GIT_WALK_FAILED: {message}")]
    GitWalk {
        /// What failed.
        message: String,
    },
    /// `git rev-parse HEAD` did not return a 40-hex commit SHA.
    #[error("GIT_COMMIT_SHA_INVALID: rev-parse returned `{0}`")]
    GitCommitShaInvalid(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::IngestError;

    /// Every git error kind renders its documented `GIT_*` code prefix, so
    /// the CLI edge cannot surface a bare message
    /// (docs/implementation/08 §Error codes).
    #[test]
    fn git_errors_carry_their_code_prefix() {
        let message = "boom".to_owned();
        assert_eq!(
            IngestError::GitSpawn {
                message: message.clone()
            }
            .to_string(),
            "GIT_SPAWN_FAILED: boom"
        );
        assert_eq!(
            IngestError::GitTempDir {
                message: message.clone()
            }
            .to_string(),
            "GIT_TEMP_DIR: boom"
        );
        assert_eq!(
            IngestError::GitWalk { message }.to_string(),
            "GIT_WALK_FAILED: boom"
        );
    }
}
