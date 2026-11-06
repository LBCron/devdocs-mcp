//! The unified result envelope (docs/implementation/08 §CLI contract).
//!
//! The CLI prints this envelope on stdout under `--json`; the MCP server
//! reuses the shape for structured output. The per-result fields follow the
//! docs/08 example exactly; placeholder commands print the empty envelope.

use serde::{Deserialize, Serialize};
use std::fmt;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The version status of a served result
/// (docs/implementation/08 §CLI contract: `verified`, `alias`, `fallback`,
/// `absent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// The content hash was re-derived and matched.
    Verified,
    /// The version served through a documented alias.
    Alias,
    /// The version served through a fallback rule.
    Fallback,
    /// The library is absent from the index.
    Absent,
}

impl fmt::Display for VersionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Verified => "verified",
            Self::Alias => "alias",
            Self::Fallback => "fallback",
            Self::Absent => "absent",
        };
        f.write_str(text)
    }
}

/// One search/open/symbol result in the docs/08 result envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultItem {
    /// The document ref, for example `docs://next.js/v16.2.9/proxy#rewrite`.
    #[serde(rename = "ref")]
    pub doc_ref: String,
    /// The section title.
    pub title: String,
    /// The section path, for example `Routing > Proxy`.
    pub section: String,
    /// The matched text.
    pub snippet: String,
    /// Code examples in the section.
    pub examples: Vec<Example>,
    /// The upstream page the section came from.
    pub source_url: String,
    /// The upstream commit the content was captured at.
    pub source_commit: String,
    /// The version the query carries, when any.
    pub requested_version: Option<String>,
    /// The version that actually served the query.
    pub served_version: String,
    /// The version status of the served result.
    pub version_status: VersionStatus,
    /// The hex sha256 of the section content.
    pub content_hash: String,
    /// The license status of the source.
    pub license_status: String,
    /// The attribution line the source's license requires, when one is
    /// recorded (docs/implementation/07 §License gate). Absent from the
    /// JSON when no attribution applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// The fusion confidence score.
    pub confidence: f64,
    /// The token count of the snippet.
    pub token_count: u64,
}

/// A code example in a result item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Example {
    /// The example language, for example `tsx`.
    pub language: String,
    /// The example code.
    pub code: String,
}

/// The result envelope printed on stdout (docs/08 top-level fields).
///
/// The item type varies per command: search/open/symbol serve
/// [`ResultItem`]s, `list` serves [`SourceSummary`]s. The top-level shape is
/// the contract and stays identical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultEnvelope<T = ResultItem> {
    /// The results, in fusion order.
    pub results: Vec<T>,
    /// Whether the budget cut the result list short.
    pub truncated: bool,
    /// The cursor for the next page, when results were truncated.
    pub next_cursor: Option<String>,
    /// The RFC 3339 time the envelope was served at.
    pub served_at: String,
}

/// One `devdocs list` item: the discovery surface names every indexed
/// source and version (docs/implementation/08 §CLI contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSummary {
    /// The library name, for example `next.js`.
    pub library: String,
    /// The exact version, for example `16.2.9`.
    pub version: String,
    /// The origin adapter that produced the content, for example
    /// `llms_txt`.
    pub kind: String,
    /// The SPDX license id the source records, when one is known.
    pub license: Option<String>,
    /// The license approval stage of the source.
    pub license_status: String,
    /// Whether the source is active in search.
    pub active: bool,
    /// When the content was fetched, RFC 3339 UTC.
    pub fetched_at: String,
    /// The date the license approval lapses, when one is recorded
    /// (docs/implementation/07 §License gate).
    pub license_expires_at: Option<String>,
    /// The commit the content was captured at, when the adapter records
    /// one (git sources only).
    pub commit_sha: Option<String>,
}

/// One `devdocs source list` entry: the declarative `[sources.<name>]`
/// config entry joined with the authoritative store state
/// (docs/implementation/07 §License gate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceListEntry {
    /// The registry entry name.
    pub name: String,
    /// The source URL from the config entry.
    pub url: Option<String>,
    /// The source kind from the config entry.
    pub kind: Option<String>,
    /// The SPDX license id from the config entry.
    pub license: Option<String>,
    /// The attribution line from the config entry.
    pub attribution: Option<String>,
    /// The config expiry date, `YYYY-MM-DD`.
    pub expires: Option<String>,
    /// The least privileged stored state of the library's rows, or the
    /// deny-by-default `pending_review` when nothing is indexed yet.
    pub status: String,
    /// The indexed versions of the library, from the store.
    pub versions: Vec<SourceListVersion>,
}

/// The `devdocs source list --json` envelope: the config entries joined
/// with store state plus the serve time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceListEnvelope {
    /// One entry per configured source.
    pub results: Vec<SourceListEntry>,
    /// The RFC 3339 time the envelope was served at.
    pub served_at: String,
}

/// One version row of a [`SourceListEntry`], straight from the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceListVersion {
    /// The exact version.
    pub version: String,
    /// The license approval stage of the row.
    pub status: String,
    /// When the content was fetched, RFC 3339 UTC.
    pub fetched_at: String,
    /// Whether the source is active in search.
    pub active: bool,
}

/// The RFC 3339 UTC timestamp envelopes carry as `served_at`; the fallback
/// keeps the envelope contract when the clock is unavailable.
#[must_use]
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Build the empty result envelope that placeholder commands print.
#[must_use]
pub fn empty_envelope() -> ResultEnvelope {
    ResultEnvelope {
        results: Vec::new(),
        truncated: false,
        next_cursor: None,
        served_at: now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{SourceSummary, VersionStatus, empty_envelope, now_rfc3339};

    /// The `Display` text of each status is the exact `snake_case` string the
    /// envelope contract names (docs/implementation/08 §CLI contract).
    #[test]
    fn version_status_displays_the_contract_strings() {
        assert_eq!(VersionStatus::Verified.to_string(), "verified");
        assert_eq!(VersionStatus::Alias.to_string(), "alias");
        assert_eq!(VersionStatus::Fallback.to_string(), "fallback");
        assert_eq!(VersionStatus::Absent.to_string(), "absent");
    }

    /// A source summary round-trips through the envelope JSON, so the
    /// `list` output parses on the consumer side; the license and expiry
    /// fields ride along.
    #[test]
    fn source_summary_round_trips_through_json() {
        let summary = SourceSummary {
            library: String::from("acme"),
            version: String::from("1.0"),
            kind: String::from("llms_txt"),
            license: Some(String::from("MIT")),
            license_status: String::from("approved_index"),
            active: true,
            fetched_at: String::from("2026-11-06T00:00:00Z"),
            license_expires_at: Some(String::from("2027-11-06")),
            commit_sha: None,
        };
        let text = serde_json::to_string(&summary).unwrap();
        let back: SourceSummary = serde_json::from_str(&text).unwrap();
        assert_eq!(back, summary);
    }

    /// The result item carries the attribution line only when the source's
    /// license requires one: the field is skipped in the JSON when absent
    /// and present when set (docs/implementation/07 §License gate).
    #[test]
    fn result_item_serializes_attribution_only_when_set() {
        use super::{Example, ResultItem, VersionStatus};

        let base = ResultItem {
            doc_ref: String::from("docs://acme/v1.0/intro"),
            title: String::from("Intro"),
            section: String::from("Intro"),
            snippet: String::from("text"),
            examples: Vec::<Example>::new(),
            source_url: String::from("https://docs.acme.test/intro.md"),
            source_commit: String::from("abc"),
            requested_version: None,
            served_version: String::from("1.0"),
            version_status: VersionStatus::Verified,
            content_hash: String::from("deadbeef"),
            license_status: String::from("approved_snippet"),
            attribution: None,
            confidence: 1.0,
            token_count: 4,
        };

        let without = serde_json::to_string(&base).unwrap();
        assert!(
            !without.contains("attribution"),
            "absent attribution must be skipped: {without}"
        );

        let with = ResultItem {
            attribution: Some(String::from("CC-BY-4.0 — (c) Acme Corp")),
            ..base.clone()
        };
        let text = serde_json::to_string(&with).unwrap();
        assert!(
            text.contains(r#""attribution":"CC-BY-4.0 — (c) Acme Corp""#),
            "set attribution must serialize: {text}"
        );
        let back: ResultItem = serde_json::from_str(&text).unwrap();
        assert_eq!(back, with);
    }

    /// The empty envelope carries the documented top-level shape and an
    /// RFC 3339 `served_at`.
    #[test]
    fn empty_envelope_carries_the_contract_shape() {
        let envelope = empty_envelope();
        assert_eq!(envelope.results, Vec::new());
        assert!(!envelope.truncated);
        assert_eq!(envelope.next_cursor, None);
        assert!(envelope.served_at.contains('T'), "{:?}", envelope.served_at);
        assert!(now_rfc3339().ends_with('Z'));
    }
}
