//! The M3a ingest pipeline: license gate → fetch → parse → segment →
//! publish (docs/implementation/07 §Pipeline stages).
//!
//! The slice ingests an llms.txt index plus its linked markdown pages
//! (`.md` paths); non-markdown links are skipped with a warning and a
//! count. Any guard error fails the whole run closed before publish. The
//! total ingest wall time caps at 600 s (docs/implementation/09 invariant
//! 7); a cancelled or timed-out run leaves the store untouched because the
//! publish is one transaction.

use std::time::Duration;

use devdocs_model::config::Config;
use devdocs_model::source::{LicenseStatus, SourceKind, SourceRecord};
use devdocs_store::{NewChunk, Store};
use time::OffsetDateTime;
use url::Url;

use crate::error::IngestError;
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
        tokio::time::timeout(
            TOTAL_INGEST_CAP,
            self.run_inner(&store, index_url, library, version, grant),
        )
        .await
        .map_err(|_| IngestError::TotalTimeout {
            seconds: TOTAL_INGEST_CAP.as_secs(),
        })?
    }

    /// The pipeline body inside the total-cap timeout.
    async fn run_inner(
        &self,
        store: &std::sync::Arc<Store>,
        index_url: &Url,
        library: &str,
        version: &str,
        grant: crate::license::LicenseGrant,
    ) -> Result<IngestOutcome, IngestError> {
        let index_body = self.fetch(index_url).await?;
        let index = parse_llms_txt(&index_body, index_url.as_str())?;

        let segmenter = Segmenter::new()?;
        let mut new_chunks: Vec<NewChunk> = Vec::new();
        let mut pages = 0usize;
        let mut skipped = 0usize;
        for link in index.links() {
            let page_url = index_url
                .join(link.url())
                .map_err(|e| IngestError::LlmsTxtInvalid {
                    url: index_url.to_string(),
                    reason: format!("link {:?} does not resolve: {e}", link.url()),
                })?;
            if !is_markdown_page(&page_url) {
                tracing::warn!("skipping non-markdown link {}", page_url);
                skipped += 1;
                continue;
            }
            let body = self.fetch(&page_url).await?;
            let page_segmenter = segmenter.clone();
            let library = library.to_owned();
            let version = version.to_owned();
            let prepared = tokio::task::spawn_blocking(move || {
                page_segmenter.segment(&library, &version, &body)
            })
            .await
            .map_err(|join| IngestError::Join(join.to_string()))?;
            for chunk in prepared {
                new_chunks.push(NewChunk {
                    section_path: chunk.section_path,
                    symbol: None,
                    text: chunk.text,
                    token_count: chunk.token_count,
                    content_hash: chunk.content_hash,
                    ordinal: u32::try_from(new_chunks.len()).unwrap_or(u32::MAX),
                });
            }
            pages += 1;
        }

        if pages == 0 {
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
            license: grant.license,
            license_status: grant.status,
            fetched_at: OffsetDateTime::now_utc(),
        };
        let chunks_len = new_chunks.len();
        let publish_store = std::sync::Arc::clone(store);
        let publish_result = tokio::task::spawn_blocking(move || {
            publish_store.publish_generation(&record, &new_chunks)
        })
        .await
        .map_err(|join| IngestError::Join(join.to_string()))?;
        publish_result?;

        Ok(IngestOutcome {
            library: library.to_owned(),
            version: version.to_owned(),
            origin_url: index_url.to_string(),
            license_status: grant.status,
            pages,
            skipped_pages: skipped,
            chunks: chunks_len,
        })
    }

    /// Fetches one URL: the guard policy always runs; the bytes come from
    /// the fixture replay when one is loaded, else from the network.
    async fn fetch(&self, url: &Url) -> Result<String, IngestError> {
        self.guard.check(url)?;
        if let Some(fixture) = self.fixture {
            let recorded = fixture.serve(url).ok_or_else(|| {
                IngestError::Guard(crate::guard::GuardError::FixtureMiss(url.to_string()))
            })?;
            return Ok(recorded.body.clone());
        }
        let bytes = self.guard.get(url).await?;
        String::from_utf8(bytes.to_vec()).map_err(|_| IngestError::LlmsTxtInvalid {
            url: url.to_string(),
            reason: String::from("the response is not valid UTF-8"),
        })
    }
}

/// Whether the URL path names a markdown page (the M3a slice ingests
/// markdown only; the HTML adapter owns other page kinds).
fn is_markdown_page(url: &Url) -> bool {
    url.path()
        .split('/')
        .next_back()
        .is_some_and(|last| last.to_ascii_lowercase().ends_with(".md"))
}
