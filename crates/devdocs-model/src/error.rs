//! The top error enum, its stable code strings, and its exit-code mapping.
//!
//! `Error` is the top of the error chain (docs/implementation/05 §Error
//! model). Crate errors convert into it with `#[from]`. The CLI edge maps it
//! to the documented exit codes 0–4 and prints the JSON error envelope on
//! stderr. The code strings are stable `UPPER_SNAKE` values
//! (docs/implementation/08 §Error codes); a new code is a minor release.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Why a valid query served nothing (exit code 2).
///
/// The values are the `miss_reason` strings from
/// docs/implementation/08 §Error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissReason {
    /// The library is absent from the index.
    NotIndexed,
    /// The library is indexed, but the requested version is not.
    VersionAbsent,
    /// The license gate refused the source.
    LicenseBlocked,
    /// Only quarantined chunks matched.
    QuarantinedOnly,
    /// Matches existed, but none cleared the confidence floor.
    LowConfidence,
    /// The symbol lookup resolved its target, but no symbol matched.
    SymbolAbsent,
}

impl fmt::Display for MissReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::NotIndexed => "not_indexed",
            Self::VersionAbsent => "version_absent",
            Self::LicenseBlocked => "license_blocked",
            Self::QuarantinedOnly => "quarantined_only",
            Self::LowConfidence => "low_confidence",
            Self::SymbolAbsent => "symbol_absent",
        };
        f.write_str(text)
    }
}

/// Which upstream failure an [`Error::Upstream`] carries (exit code 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamCode {
    /// The fetch guard or the license gate refused the request.
    FetchBlocked,
    /// A network, git, or parse failure.
    UpstreamError,
    /// The stored content hash no longer matches the re-derived one.
    HashMismatch,
}

impl fmt::Display for UpstreamCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::FetchBlocked => "fetch_blocked",
            Self::UpstreamError => "upstream_error",
            Self::HashMismatch => "hash_mismatch",
        };
        f.write_str(text)
    }
}

/// The top error enum for every devdocs crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Bad flags or a bad command shape. Exit 1.
    #[error("usage: {0}")]
    Usage(String),
    /// A config file failed validation. Exit 1.
    #[error("invalid config at `{key_path}`: {reason}")]
    ConfigInvalid {
        /// The dotted key path that failed, for example `fetch.concurrency`.
        key_path: String,
        /// Why the key path failed.
        reason: String,
    },
    /// A valid query served nothing. Exit 2.
    #[error("no result: {reason:?}")]
    NoResult {
        /// Why nothing was served.
        reason: MissReason,
    },
    /// A fetch, network, git, or parse failure. Exit 3.
    #[error("upstream ({code}): {detail}")]
    Upstream {
        /// Which upstream failure happened.
        code: UpstreamCode,
        /// Context about the failure: a URL, a size, a count.
        detail: String,
    },
    /// The token budget was consumed with no deliverable content. Exit 4.
    #[error("budget exceeded")]
    BudgetExceeded,
}

impl Error {
    /// The exit code this error maps to at the CLI edge
    /// (docs/implementation/05 §Error model).
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::ConfigInvalid { .. } => 1,
            Self::NoResult { .. } => 2,
            Self::Upstream { .. } => 3,
            Self::BudgetExceeded => 4,
        }
    }

    /// The stable `UPPER_SNAKE` code for the JSON error envelope
    /// (docs/implementation/08 §Error codes). A cold miss — the library
    /// or the requested version absent — is `PKG_NOT_INDEXED`; every other
    /// valid-query miss is `NO_RESULT`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Usage(_) => "USAGE",
            Self::ConfigInvalid { .. } => "CONFIG_INVALID",
            Self::NoResult { reason } => match reason {
                MissReason::NotIndexed | MissReason::VersionAbsent => "PKG_NOT_INDEXED",
                MissReason::LicenseBlocked
                | MissReason::QuarantinedOnly
                | MissReason::LowConfidence
                | MissReason::SymbolAbsent => "NO_RESULT",
            },
            Self::Upstream { code, .. } => match code {
                UpstreamCode::FetchBlocked => "FETCH_BLOCKED",
                UpstreamCode::UpstreamError => "UPSTREAM_ERROR",
                UpstreamCode::HashMismatch => "HASH_MISMATCH",
            },
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
        }
    }

    /// A short next-step hint for the JSON error envelope.
    #[must_use]
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Usage(_) => Some("run: devdocs --help"),
            Self::ConfigInvalid { .. } => Some("run: devdocs config validate"),
            Self::NoResult { reason } => match reason {
                MissReason::NotIndexed | MissReason::VersionAbsent => {
                    Some("run: devdocs add <source> --version <V>")
                }
                MissReason::LicenseBlocked => Some("run: devdocs source list"),
                MissReason::QuarantinedOnly | MissReason::LowConfidence => {
                    Some("run: devdocs sync [library]")
                }
                MissReason::SymbolAbsent => Some("run: devdocs search <symbol>"),
            },
            Self::Upstream { .. } => Some("run: devdocs sync [library]"),
            Self::BudgetExceeded => Some("raise --max-tokens"),
        }
    }

    /// The JSON error envelope for stderr
    /// (docs/implementation/05 §Error model: `{"error", "hint", "detail"}`).
    #[must_use]
    pub fn to_envelope(&self) -> ErrorEnvelope {
        let detail = match self {
            Self::Usage(detail)
            | Self::ConfigInvalid { reason: detail, .. }
            | Self::Upstream { detail, .. } => Some(detail.clone()),
            Self::NoResult { .. } | Self::BudgetExceeded => None,
        };
        ErrorEnvelope {
            error: self.code().to_owned(),
            hint: self.hint().map(str::to_owned),
            detail,
            miss_reason: match self {
                Self::NoResult { reason } => Some(*reason),
                _ => None,
            },
            key_path: match self {
                Self::ConfigInvalid { key_path, .. } => Some(key_path.clone()),
                _ => None,
            },
        }
    }
}

/// The JSON error envelope printed on stderr (docs/implementation/05
/// §Error model). Exit 2 additionally carries `miss_reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// The stable `UPPER_SNAKE` error code.
    pub error: String,
    /// A short next-step hint.
    pub hint: Option<String>,
    /// Extra context, when the failure carries any.
    pub detail: Option<String>,
    /// Why a valid query served nothing. Present only on `NO_RESULT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miss_reason: Option<MissReason>,
    /// The dotted config key that failed validation. Present only on
    /// `CONFIG_INVALID`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
}

impl ErrorEnvelope {
    /// Serialize to the single-line JSON string the CLI prints on stderr.
    ///
    /// # Errors
    /// Serialization of a field-only struct cannot fail, but `serde_json`
    /// forces this shape on us.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Error, ErrorEnvelope, MissReason, UpstreamCode};

    /// The exit-code table test: every variant maps to 0/1/2/3/4
    /// (docs/implementation/05 §Error model).
    #[test]
    fn error_maps_to_documented_exit_codes() {
        let cases: [(Error, i32); 5] = [
            (Error::Usage("bad flag".to_owned()), 1),
            (
                Error::ConfigInvalid {
                    key_path: "fetch.concurrency".to_owned(),
                    reason: "expected a number".to_owned(),
                },
                1,
            ),
            (
                Error::NoResult {
                    reason: MissReason::NotIndexed,
                },
                2,
            ),
            (
                Error::Upstream {
                    code: UpstreamCode::UpstreamError,
                    detail: "connection refused".to_owned(),
                },
                3,
            ),
            (Error::BudgetExceeded, 4),
        ];
        for (error, expected) in cases {
            assert_eq!(error.exit_code(), expected, "exit code for {error:?}");
        }
    }

    /// Every variant maps to a stable `UPPER_SNAKE` code; a cold miss —
    /// the library or the requested version absent — is
    /// `PKG_NOT_INDEXED` (docs/implementation/08 §Error codes).
    #[test]
    fn error_maps_to_stable_codes() {
        let codes = [
            Error::Usage(String::new()),
            Error::ConfigInvalid {
                key_path: String::new(),
                reason: String::new(),
            },
            Error::NoResult {
                reason: MissReason::NotIndexed,
            },
            Error::NoResult {
                reason: MissReason::VersionAbsent,
            },
            Error::NoResult {
                reason: MissReason::LowConfidence,
            },
            Error::NoResult {
                reason: MissReason::SymbolAbsent,
            },
            Error::Upstream {
                code: UpstreamCode::FetchBlocked,
                detail: String::new(),
            },
            Error::Upstream {
                code: UpstreamCode::UpstreamError,
                detail: String::new(),
            },
            Error::Upstream {
                code: UpstreamCode::HashMismatch,
                detail: String::new(),
            },
            Error::BudgetExceeded,
        ];
        let expected = [
            "USAGE",
            "CONFIG_INVALID",
            "PKG_NOT_INDEXED",
            "PKG_NOT_INDEXED",
            "NO_RESULT",
            "NO_RESULT",
            "FETCH_BLOCKED",
            "UPSTREAM_ERROR",
            "HASH_MISMATCH",
            "BUDGET_EXCEEDED",
        ];
        for (error, code) in codes.iter().zip(expected) {
            assert_eq!(error.code(), code);
        }
        assert_eq!(codes.iter().map(Error::code).collect::<Vec<_>>().len(), 10);
    }

    /// The exit-2 envelope carries `miss_reason`; the others do not. A
    /// cold miss carries `PKG_NOT_INDEXED`.
    #[test]
    fn envelope_carries_miss_reason_only_for_no_result() {
        let miss = Error::NoResult {
            reason: MissReason::VersionAbsent,
        };
        let envelope: ErrorEnvelope = miss.to_envelope();
        assert_eq!(envelope.error, "PKG_NOT_INDEXED");
        assert_eq!(envelope.miss_reason, Some(MissReason::VersionAbsent));
        assert!(envelope.hint.is_some());

        let usage = Error::Usage("bad flag".to_owned()).to_envelope();
        assert_eq!(usage.miss_reason, None);
        assert_eq!(usage.detail.as_deref(), Some("bad flag"));
    }

    /// The envelope round-trips as the single-line JSON shape from doc 05;
    /// a cold miss carries `PKG_NOT_INDEXED` and the add hint.
    #[test]
    fn envelope_serializes_to_one_line_json() {
        let json = Error::NoResult {
            reason: MissReason::NotIndexed,
        }
        .to_envelope()
        .to_json_string()
        .unwrap();
        assert_eq!(
            json,
            r#"{"error":"PKG_NOT_INDEXED","hint":"run: devdocs add <source> --version <V>","detail":null,"miss_reason":"not_indexed"}"#
        );
    }

    /// A symbol miss is a `NO_RESULT` carrying `symbol_absent` and the
    /// search hint — the target resolved, the identifier did not.
    #[test]
    fn symbol_absent_maps_to_no_result_with_the_search_hint() {
        let error = Error::NoResult {
            reason: MissReason::SymbolAbsent,
        };
        assert_eq!(error.exit_code(), 2);
        assert_eq!(error.code(), "NO_RESULT");
        let json = error.to_envelope().to_json_string().unwrap();
        assert_eq!(
            json,
            r#"{"error":"NO_RESULT","hint":"run: devdocs search <symbol>","detail":null,"miss_reason":"symbol_absent"}"#
        );
    }

    /// The `CONFIG_INVALID` envelope carries the failing key path; the
    /// other envelopes omit it.
    #[test]
    fn envelope_carries_key_path_only_for_config_invalid() {
        let envelope = Error::ConfigInvalid {
            key_path: "fetch.concurrency".to_owned(),
            reason: "expected a number".to_owned(),
        }
        .to_envelope();
        assert_eq!(envelope.key_path.as_deref(), Some("fetch.concurrency"));
        let json = envelope.to_json_string().unwrap();
        assert!(
            json.contains(r#""key_path":"fetch.concurrency""#),
            "json: {json}"
        );

        let usage = Error::Usage("bad flag".to_owned()).to_envelope();
        assert_eq!(usage.key_path, None);
    }
}
