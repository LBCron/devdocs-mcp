//! The M3a ingest pipeline: license gate → fetch → parse → segment →
//! publish (docs/implementation/07 §Pipeline stages).
//!
//! The slice ingests an llms.txt index plus its linked markdown pages
//! (`.md` paths); non-markdown links are skipped with a warning and a
//! count. Any guard error fails the whole run closed before publish. The
//! total ingest wall time caps at 600 s (docs/implementation/09 invariant
//! 7) and bounds the fetch-and-segment work only: the publish runs after
//! the cap, so a timed-out run reports the publish's own result and the
//! store state never diverges from the reported outcome. A run that never
//! reaches publish leaves the store untouched.

use std::time::Duration;

use devdocs_model::config::Config;
use devdocs_model::source::{LicenseStatus, SourceKind, SourceRecord};
use devdocs_store::{NewChunk, Store};
use time::OffsetDateTime;
use url::Url;

use crate::error::IngestError;
use crate::events::IngestEvents;
use crate::fixture::FixtureSet;
use crate::guard::FetchGuard;
use crate::llms_txt::parse_llms_txt;
use crate::segment::Segmenter;

/// The total ingest wall-time cap (docs/implementation/09 invariant 7: "by
/// config"; the config key lands with the M6 network work, the cap holds
/// from M3a).
pub const TOTAL_INGEST_CAP: Duration = Duration::from_secs(600);

/// What one ingest run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestOutcome {
    /// The library name from the registry entry.
    pub library: String,
    /// The version the run pinned.
    pub version: String,
    /// The index URL.
    pub origin_url: String,
    /// The license stage the run published under.
    pub license_status: LicenseStatus,
    /// Markdown pages ingested.
    pub pages: usize,
    /// Links skipped because they were not markdown pages.
    pub skipped_pages: usize,
    /// Chunks published for the source.
    pub chunks: usize,
    /// The ingest event report: what the run skipped, dropped, and detoured
    /// (docs/implementation/07 §Normalization pipeline).
    pub events: IngestEvents,
}

/// One ingest run: the resolved config, the guard, and the optional fixture
/// replay set.
#[derive(Clone)]
pub struct IngestJob<'a> {
    config: &'a Config,
    guard: &'a FetchGuard,
    fixture: Option<&'a FixtureSet>,
}

impl std::fmt::Debug for IngestJob<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestJob")
            .field("fixture", &self.fixture.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> IngestJob<'a> {
    /// Assembles one run.
    #[must_use]
    pub fn new(config: &'a Config, guard: &'a FetchGuard, fixture: Option<&'a FixtureSet>) -> Self {
        Self {
            config,
            guard,
            fixture,
        }
    }

    /// Runs the pipeline: gate, fetch index, parse, fetch pages, segment,
    /// publish. The license gate was already applied by the caller
    /// ([`crate::license::gate_fetch`]) before the store opened; the run
    /// re-checks nothing and trusts the grant it was handed.
    ///
    /// The total cap bounds the fetch-and-segment work only. The publish
    /// runs after the cap: once the transaction starts, the run reports the
    /// publish's own result, so the store state and the reported outcome
    /// never diverge (docs/implementation/09 invariant 7).
    ///
    /// # Errors
    /// [`IngestError`] when any stage fails; the store stays untouched on
    /// any failure.
    pub async fn run(
        &self,
        store: std::sync::Arc<Store>,
        index_url: &Url,
        library: &str,
        version: &str,
    ) -> Result<IngestOutcome, IngestError> {
        let grant = crate::license::gate_fetch(self.config, index_url)?;
        // The published library identity is the registry entry: a caller
        // that passes another name would publish approved content under an
        // arbitrary name, so the run refuses instead (docs/implementation/07
        // §License gate).
        if library != grant.name {
            return Err(IngestError::LicenseNameMismatch {
                expected: grant.name.clone(),
                actual: library.to_owned(),
            });
        }
        let prepared = tokio::time::timeout(
            TOTAL_INGEST_CAP,
            self.prepare(index_url, library, version, &grant),
        )
        .await
        .map_err(|_| IngestError::TotalTimeout {
            seconds: TOTAL_INGEST_CAP.as_secs(),
        })??;
        self.publish(store, index_url, library, version, &grant, prepared)
            .await
    }

    /// The fetch-and-segment phase, inside the total-cap timeout: fetch and
    /// parse the index, fetch every markdown page, segment, and buffer the
    /// chunks. Publishes nothing.
    async fn prepare(
        &self,
        index_url: &Url,
        library: &str,
        version: &str,
        grant: &crate::license::LicenseGrant,
    ) -> Result<Prepared, IngestError> {
        // Relative links resolve against the URL the index actually came
        // from: a redirect can move it to another directory, and the
        // pre-redirect URL would join every link to the wrong base.
        let (index_body, index_base) = self.fetch(index_url).await?;
        let index = parse_llms_txt(&index_body, index_url.as_str())?;
        let segmenter = Segmenter::new()?;
        let mut run = RunState::default();
        let ctx = LinkContext {
            index_url,
            index_base: &index_base,
            library,
            version,
        };
        for link in index.links() {
            self.ingest_link(&ctx, link, &segmenter, &mut run).await?;
        }

        // A run that segments to zero chunks — no markdown links at all, or
        // only heading-only pages — must not publish an empty generation
        // over the active one (docs/implementation/07 §Atomic publish).
        if run.new_chunks.is_empty() {
            return Err(IngestError::NoIngestablePages {
                index: index_url.to_string(),
            });
        }

        let record = SourceRecord {
            id: devdocs_model::SourceId(0),
            library: library.to_owned(),
            version: version.to_owned(),
            kind: SourceKind::LlmsTxt,
            origin_url: index_url.clone(),
            commit_sha: None,
            license: grant.license.clone(),
            license_status: grant.status,
            fetched_at: OffsetDateTime::now_utc(),
        };
        Ok(Prepared {
            record,
            new_chunks: run.new_chunks,
            events: run.events,
        })
    }

    /// Ingests one index link: resolve, skip non-markdown URLs and HTML
    /// bodies, segment, and buffer the chunks into `run`.
    async fn ingest_link(
        &self,
        ctx: &LinkContext<'_>,
        link: &crate::llms_txt::LlmsLink,
        segmenter: &Segmenter,
        run: &mut RunState,
    ) -> Result<(), IngestError> {
        let page_url =
            ctx.index_base
                .join(link.url())
                .map_err(|e| IngestError::LlmsTxtInvalid {
                    url: ctx.index_url.to_string(),
                    reason: format!("link {:?} does not resolve: {e}", link.url()),
                })?;
        if !is_markdown_page(&page_url) {
            tracing::warn!("skipping non-markdown link {}", page_url);
            run.events.skipped_pages += 1;
            return Ok(());
        }
        let (body, _) = self.fetch(&page_url).await?;
        if is_html_body(&body) {
            tracing::warn!("markdown link {} returned an HTML body; skipping", page_url);
            run.events.skipped_pages += 1;
            return Ok(());
        }
        let (chunks, stats) = segment_page(segmenter, ctx.library, ctx.version, body).await?;
        run.events.skipped_blocks += stats.skipped_blocks;
        run.events.unknown_fence_languages += stats.unknown_fence_languages;
        for chunk in chunks {
            run.new_chunks.push(NewChunk {
                section_path: chunk.section_path,
                symbol: None,
                text: chunk.text,
                token_count: chunk.token_count,
                content_hash: chunk.content_hash,
                ordinal: u32::try_from(run.new_chunks.len()).unwrap_or(u32::MAX),
            });
        }
        run.events.pages += 1;
        Ok(())
    }

    /// Publishes the prepared run outside the total-cap timeout and reports
    /// the publish's own result.
    async fn publish(
        &self,
        store: std::sync::Arc<Store>,
        index_url: &Url,
        library: &str,
        version: &str,
        grant: &crate::license::LicenseGrant,
        prepared: Prepared,
    ) -> Result<IngestOutcome, IngestError> {
        let Prepared {
            record,
            new_chunks,
            events,
        } = prepared;
        let chunks_len = new_chunks.len();
        let publish_result =
            tokio::task::spawn_blocking(move || store.publish_generation(&record, &new_chunks))
                .await
                .map_err(|join| IngestError::Join(join.to_string()))?;
        publish_result?;

        Ok(IngestOutcome {
            library: library.to_owned(),
            version: version.to_owned(),
            origin_url: index_url.to_string(),
            license_status: grant.status,
            pages: events.pages,
            skipped_pages: events.skipped_pages,
            chunks: chunks_len,
            events,
        })
    }

    /// Fetches one URL: the guard policy always runs; the bytes come from
    /// the fixture replay when one is loaded, else from the network.
    /// Returns the body and the URL the response actually came from — the
    /// requested URL in replay (no redirects), the final URL on the wire.
    async fn fetch(&self, url: &Url) -> Result<(String, Url), IngestError> {
        self.guard.check(url)?;
        if let Some(fixture) = self.fixture {
            let recorded = fixture.serve(url).ok_or_else(|| {
                IngestError::Guard(crate::guard::GuardError::FixtureMiss(url.to_string()))
            })?;
            // Replay holds the network path to its own contracts: a
            // recorded non-success status and an oversized body fail the
            // same way the live fetch would (docs/04 §Fixtures).
            let status = reqwest::StatusCode::from_u16(recorded.status).map_err(|_| {
                IngestError::LlmsTxtInvalid {
                    url: url.to_string(),
                    reason: format!("fixture records status {}", recorded.status),
                }
            })?;
            if !status.is_success() {
                return Err(IngestError::Guard(crate::guard::GuardError::Http {
                    url: url.to_string(),
                    status,
                }));
            }
            let limit = self.config.fetch.max_bytes;
            if recorded.body.len() as u64 > limit {
                return Err(IngestError::Guard(crate::guard::GuardError::BodyTooLarge {
                    url: url.to_string(),
                    limit,
                }));
            }
            return Ok((recorded.body.clone(), url.clone()));
        }
        let (bytes, final_url) = self.guard.get(url).await?;
        String::from_utf8(bytes.to_vec())
            .map(|body| (body, final_url))
            .map_err(|_| IngestError::LlmsTxtInvalid {
                url: url.to_string(),
                reason: String::from("the response is not valid UTF-8"),
            })
    }
}

/// Everything the fetch-and-segment phase produced, ready to publish.
struct Prepared {
    record: SourceRecord,
    new_chunks: Vec<NewChunk>,
    events: IngestEvents,
}

/// The running totals one fetch-and-segment phase accumulates.
#[derive(Default)]
struct RunState {
    new_chunks: Vec<NewChunk>,
    events: IngestEvents,
}

/// What one index link needs: the index URL for errors, the redirect-aware
/// base for joining, and the run's library/version identity.
struct LinkContext<'a> {
    index_url: &'a Url,
    index_base: &'a Url,
    library: &'a str,
    version: &'a str,
}

/// Segments one page body off the async path. The blocking thread owns the
/// page segmenter clone, so the async loop never waits on parsing. Returns
/// the chunks plus the page's event counters.
async fn segment_page(
    segmenter: &Segmenter,
    library: &str,
    version: &str,
    body: String,
) -> Result<
    (
        Vec<crate::segment::PreparedChunk>,
        crate::segment::SegmentStats,
    ),
    IngestError,
> {
    let page_segmenter = segmenter.clone();
    let library = library.to_owned();
    let version = version.to_owned();
    tokio::task::spawn_blocking(move || {
        page_segmenter.segment_with_stats(&library, &version, &body)
    })
    .await
    .map_err(|join| IngestError::Join(join.to_string()))
}

/// Whether a response body is an HTML document: the trimmed start opens a
/// doctype or an `html` tag. The slice ingests markdown only, so a markdown
/// link that serves HTML is skipped rather than stored as markup
/// (docs/implementation/07 §Media split). A leading HTML comment or a code
/// fence never matches. A leading UTF-8 BOM is stripped first, the way the
/// index parser strips it (docs/implementation/07 §llms.txt adapter (v2)).
fn is_html_body(body: &str) -> bool {
    let trimmed = body.trim_start();
    let trimmed = trimmed
        .strip_prefix('\u{feff}')
        .unwrap_or(trimmed)
        .trim_start();
    let head: String = trimmed.chars().take(64).collect();
    let head = head.to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

/// Whether the URL path names a markdown page (the M3a slice ingests
/// markdown only; the HTML adapter owns other page kinds).
fn is_markdown_page(url: &Url) -> bool {
    url.path()
        .split('/')
        .next_back()
        .is_some_and(|last| last.to_ascii_lowercase().ends_with(".md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An HTML body is detected case-insensitively after leading
    /// whitespace.
    #[test]
    fn html_body_matches_a_case_insensitive_prefix() {
        assert!(is_html_body("\n  <!DOCTYPE html><html><body>x"));
        assert!(is_html_body("<HTML><body>x"));
    }

    /// A leading UTF-8 BOM does not hide an HTML body from the check, in
    /// either position around leading whitespace
    /// (docs/implementation/07 §Media split).
    #[test]
    fn html_body_strips_a_leading_bom() {
        assert!(is_html_body("\u{feff}<!doctype html><html><body>x"));
        assert!(is_html_body(" \u{feff}<html>"));
        assert!(is_html_body("\u{feff} <html>"));
    }

    /// A markdown body never matches, BOM or not.
    #[test]
    fn markdown_body_does_not_match() {
        assert!(!is_html_body("# Title\n\nbody text"));
        assert!(!is_html_body("\u{feff}# Titled"));
    }
}
