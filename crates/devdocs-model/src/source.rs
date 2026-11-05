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
/// progression from any point; a `denied` source can re-enter review
/// ([`LicenseStatus::can_transition_to`]), while `expired` and `revoked`
/// absorb every transition.
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

    /// Whether the status is a terminal refusal for retrieval: a source
    /// carrying it drops out of search and serves
    /// `not_available` (docs/implementation/09 §License gate enforcement
    /// points). Terminal for retrieval is not the same as absorbing for
    /// transitions — a `denied` source can re-enter review, while
    /// `expired` and `revoked` admit no move out
    /// ([`Self::can_transition_to`]).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Denied | Self::Expired | Self::Revoked)
    }

    /// Whether the license gate lets a source flip to `active`.
    ///
    /// The store refuses to publish a source below the index stage
    /// (docs/implementation/09 §License gate enforcement points).
    #[must_use]
    pub const fn allows_index(self) -> bool {
        self.rank() >= Self::ApprovedIndex.rank()
    }

    /// Whether the license gate lets the source be fetched at all.
    ///
    /// The ingest planner refuses sources below the fetch stage
    /// (docs/implementation/09 §License gate enforcement points).
    #[must_use]
    pub const fn allows_fetch(self) -> bool {
        self.rank() >= Self::ApprovedFetch.rank()
    }

    /// Whether the state machine admits the transition `self → next`
    /// (docs/implementation/07 §License gate transition table).
    ///
    /// The approval stages form a forward-only progression, and a terminal
    /// refusal (`denied`, `expired`, `revoked`) is reachable from any
    /// non-terminal state. `denied` is the one refusal a review can lift:
    /// `denied → pending_review` re-opens the review and is the sole exit
    /// from a refusal. `expired` and `revoked` are absorbing — no move
    /// leaves them, not even into another refusal. Every other move —
    /// backwards, sideways, or onto the same state — is refused.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use LicenseStatus::{
            ApprovedCache, ApprovedEmbed, ApprovedFetch, ApprovedIndex, ApprovedSnippet, Denied,
            Expired, PendingReview, Revoked,
        };
        matches!(
            (self, next),
            (PendingReview, ApprovedFetch)
                | (ApprovedFetch, ApprovedIndex)
                | (ApprovedIndex, ApprovedEmbed)
                | (ApprovedEmbed, ApprovedCache)
                | (ApprovedCache, ApprovedSnippet)
                | (Denied, PendingReview)
                | (
                    PendingReview
                        | ApprovedFetch
                        | ApprovedIndex
                        | ApprovedEmbed
                        | ApprovedCache
                        | ApprovedSnippet,
                    Denied | Expired | Revoked
                )
        )
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

    /// Only the three terminal refusals report terminal.
    #[test]
    fn only_terminal_refusals_report_terminal() {
        for status in [
            LicenseStatus::Denied,
            LicenseStatus::Expired,
            LicenseStatus::Revoked,
        ] {
            assert!(status.is_terminal(), "{status}");
        }
        for status in [
            LicenseStatus::PendingReview,
            LicenseStatus::ApprovedFetch,
            LicenseStatus::ApprovedIndex,
            LicenseStatus::ApprovedEmbed,
            LicenseStatus::ApprovedCache,
            LicenseStatus::ApprovedSnippet,
        ] {
            assert!(!status.is_terminal(), "{status}");
        }
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

    /// The exhaustive 9×9 transition table mirrors docs/07 §License gate
    /// exactly: the forward progression, `denied → pending_review` as the
    /// sole exit from a refusal, a terminal refusal from every
    /// non-terminal state, and nothing else — no self-moves, no
    /// backwards or sideways moves, no exit from `expired` or `revoked`.
    #[test]
    fn transition_table_matches_docs_07_exactly() {
        use LicenseStatus::{
            ApprovedCache, ApprovedEmbed, ApprovedFetch, ApprovedIndex, ApprovedSnippet, Denied,
            Expired, PendingReview, Revoked,
        };
        let all = [
            PendingReview,
            ApprovedFetch,
            ApprovedIndex,
            ApprovedEmbed,
            ApprovedCache,
            ApprovedSnippet,
            Denied,
            Expired,
            Revoked,
        ];
        let non_terminal = [
            PendingReview,
            ApprovedFetch,
            ApprovedIndex,
            ApprovedEmbed,
            ApprovedCache,
            ApprovedSnippet,
        ];

        let mut legal_count = 0;
        for from in all {
            for to in all {
                let forward = matches!(
                    (from, to),
                    (PendingReview, ApprovedFetch)
                        | (ApprovedFetch, ApprovedIndex)
                        | (ApprovedIndex, ApprovedEmbed)
                        | (ApprovedEmbed, ApprovedCache)
                        | (ApprovedCache, ApprovedSnippet)
                );
                let legal = forward
                    || (from, to) == (Denied, PendingReview)
                    || (matches!(to, Denied | Expired | Revoked) && non_terminal.contains(&from));
                assert_eq!(from.can_transition_to(to), legal, "{from:?} → {to:?}");
                if legal {
                    legal_count += 1;
                }
            }
        }
        // 5 forward steps + denied → pending_review + 6 × 3 refusals.
        assert_eq!(legal_count, 24, "the documented table has 24 legal moves");

        // `denied` stays terminal for retrieval while re-entering review,
        // and the two absorbing refusals admit no move out.
        assert!(Denied.is_terminal());
        assert!(Expired.is_terminal());
        assert!(Revoked.is_terminal());
    }

    /// Source ids render as plain integers for logs and envelopes.
    #[test]
    fn source_id_displays_as_an_integer() {
        assert_eq!(SourceId(41).to_string(), "41");
    }
}
