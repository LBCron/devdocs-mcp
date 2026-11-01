//! Source identity, origin kind, and license state shared across layers.

use std::fmt;

use time::OffsetDateTime;
use url::Url;

/// Stable identity of one row in `sources.source_id`.
///
/// The store assigns it on the first publish of a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub i64);

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where a source's content comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// An `llms.txt` style index plus its linked documents.
    LlmsTxt,
    /// A git repository clone.
    Git,
    /// An `OpenAPI` document.
    OpenApi,
    /// Plain HTML documentation pages.
    Html,
}

impl SourceKind {
    /// The storage spelling used by the `sources.kind` CHECK constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LlmsTxt => "llms_txt",
            Self::Git => "git",
            Self::OpenApi => "openapi",
            Self::Html => "html",
        }
    }

    /// Parses the storage spelling produced by [`SourceKind::as_str`].
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "llms_txt" => Some(Self::LlmsTxt),
            "git" => Some(Self::Git),
            "openapi" => Some(Self::OpenApi),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// License approval stage of a source.
///
/// The stages [`LicenseStatus::ApprovedFetch`] through
/// [`LicenseStatus::ApprovedSnippet`] form a progression: each later stage
/// implies the earlier ones. [`LicenseStatus::Denied`],
/// [`LicenseStatus::Expired`], and [`LicenseStatus::Revoked`] end the
/// progression from any point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LicenseStatus {
    /// Default state; nothing is allowed beyond a review.
    PendingReview,
    /// Content may be fetched and held.
    ApprovedFetch,
    /// Content may be indexed and served from the index.
    ApprovedIndex,
    /// Content may be embedded for the vector table.
    ApprovedEmbed,
    /// Content may be cached on disk.
    ApprovedCache,
    /// Content may be returned verbatim as snippets.
    ApprovedSnippet,
    /// Indexing and serving are refused.
    Denied,
    /// A previously granted approval lapsed.
    Expired,
    /// A previously granted approval was withdrawn.
    Revoked,
}

impl LicenseStatus {
    /// The storage spelling used by the `sources.license_status` CHECK
    /// constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PendingReview => "pending_review",
            Self::ApprovedFetch => "approved_fetch",
            Self::ApprovedIndex => "approved_index",
            Self::ApprovedEmbed => "approved_embed",
            Self::ApprovedCache => "approved_cache",
            Self::ApprovedSnippet => "approved_snippet",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    /// Parses the storage spelling produced by [`LicenseStatus::as_str`].
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending_review" => Some(Self::PendingReview),
            "approved_fetch" => Some(Self::ApprovedFetch),
            "approved_index" => Some(Self::ApprovedIndex),
            "approved_embed" => Some(Self::ApprovedEmbed),
            "approved_cache" => Some(Self::ApprovedCache),
            "approved_snippet" => Some(Self::ApprovedSnippet),
            "denied" => Some(Self::Denied),
            "expired" => Some(Self::Expired),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }

    /// Position on the approval progression; terminal refusals rank zero.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::PendingReview | Self::Denied | Self::Expired | Self::Revoked => 0,
            Self::ApprovedFetch => 1,
            Self::ApprovedIndex => 2,
            Self::ApprovedEmbed => 3,
            Self::ApprovedCache => 4,
            Self::ApprovedSnippet => 5,
        }
    }

    /// Whether the license gate lets a source flip to `active`.
    ///
    /// The store refuses to publish a source below the index stage
    /// (docs/implementation/09 §License gate enforcement points).
    #[must_use]
    pub const fn allows_index(self) -> bool {
        self.rank() >= Self::ApprovedIndex.rank()
    }
}

impl fmt::Display for LicenseStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One indexed source: a library at a version from one origin.
///
/// Field names follow docs/implementation/05 §Core types.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRecord {
    /// Store-assigned identity; zero until the first publish.
    pub id: SourceId,
    /// Normalized name, e.g. `"next.js"`.
    pub library: String,
    /// Exact version, e.g. `"16.2.9"`.
    pub version: String,
    /// Origin adapter that produced the content.
    pub kind: SourceKind,
    /// Where the content was fetched from.
    pub origin_url: Url,
    /// Commit the content was pinned to, when the origin pins one.
    pub commit_sha: Option<String>,
    /// SPDX id, or `None` when the origin names no license.
    pub license: Option<String>,
    /// Current license approval stage.
    pub license_status: LicenseStatus,
    /// When the content was fetched, RFC 3339 UTC.
    pub fetched_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{LicenseStatus, SourceId, SourceKind};

    /// Every kind round-trips through its storage spelling.
    #[test]
    fn source_kind_round_trips() {
        for kind in [
            SourceKind::LlmsTxt,
            SourceKind::Git,
            SourceKind::OpenApi,
            SourceKind::Html,
        ] {
            assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SourceKind::parse("llmstxt"), None);
    }

    /// Every license stage round-trips through its storage spelling.
    #[test]
    fn license_status_round_trips() {
        for status in [
            LicenseStatus::PendingReview,
            LicenseStatus::ApprovedFetch,
            LicenseStatus::ApprovedIndex,
            LicenseStatus::ApprovedEmbed,
            LicenseStatus::ApprovedCache,
            LicenseStatus::ApprovedSnippet,
            LicenseStatus::Denied,
            LicenseStatus::Expired,
            LicenseStatus::Revoked,
        ] {
            assert_eq!(LicenseStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(LicenseStatus::parse("approved"), None);
    }

    /// The index gate admits the index stage and every later grant, and
    /// refuses everything below it including terminal refusals.
    #[test]
    fn index_gate_follows_the_progression() {
        assert!(!LicenseStatus::PendingReview.allows_index());
        assert!(!LicenseStatus::ApprovedFetch.allows_index());
        assert!(LicenseStatus::ApprovedIndex.allows_index());
        assert!(LicenseStatus::ApprovedEmbed.allows_index());
        assert!(LicenseStatus::ApprovedCache.allows_index());
        assert!(LicenseStatus::ApprovedSnippet.allows_index());
        assert!(!LicenseStatus::Denied.allows_index());
        assert!(!LicenseStatus::Expired.allows_index());
        assert!(!LicenseStatus::Revoked.allows_index());
    }

    /// Source ids render as plain integers for logs and envelopes.
    #[test]
    fn source_id_displays_as_an_integer() {
        assert_eq!(SourceId(41).to_string(), "41");
    }
}
