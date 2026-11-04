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

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use devdocs_model::config::Config;
use devdocs_model::source::{LicenseStatus, SourceKind, SourceRecord};
use devdocs_store::{NewChunk, Store};
use time::OffsetDateTime;
use url::Url;

use crate::chrome::PageDocument;
use crate::error::IngestError;
use crate::events::IngestEvents;
use crate::fixture::{FixtureSet, RecordedResponse};
use crate::guard::FetchGuard;
use crate::llms_txt::parse_llms_txt;
use crate::relations::PageRelations;
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

    /// Fetches and parses one llms.txt index into a scope for its
    /// directory: the join base is the URL the response actually came from,
    /// and the covered subtree is its directory.
    async fn load_scope(&self, index_url: &Url) -> Result<Scope, IngestError> {
        let (body, base, _) = self.fetch(index_url).await?;
        let index = parse_llms_txt(&body, index_url.as_str())?;
        Ok(Scope {
            index_url: index_url.clone(),
            join_base: base.clone(),
            dir: directory_url(&base),
            index,
        })
    }

    /// Probes one URL: absence is an expected outcome, not a failure — a
    /// 404 or 410 on the wire, a missing replay response, or a non-UTF-8
    /// body all read as "not there" (docs/implementation/07 §llms.txt
    /// adapter rule 3: detection is a probe, not an assumption). The guard
    /// policy still applies, every other fetch error fails the run, and a
    /// recorded response holds to the same replay contracts as the fetch
    /// path.
    async fn probe(&self, url: &Url) -> Result<Option<String>, IngestError> {
        self.guard.check(url)?;
        if let Some(fixture) = self.fixture {
            return match fixture.serve(url) {
                Some(recorded) => self
                    .replay_body(url, recorded)
                    .map(|body| (!is_html_body(&body)).then_some(body)),
                None => Ok(None),
            };
        }
        match self.guard.get_with_link_headers(url).await {
            Ok((bytes, _, _)) => Ok(String::from_utf8(bytes.to_vec())
                .ok()
                .filter(|body| !is_html_body(body))),
            Err(crate::guard::GuardError::Http { status, .. })
                if matches!(status.as_u16(), 404 | 410) =>
            {
                tracing::debug!(status = status.as_u16(), %url, "probe found nothing");
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Walks the scope queue breadth-first and buffers every governed page
    /// and every found export into `docs`
    /// (docs/implementation/07 §llms.txt adapter rule 4). A FIFO queue is
    /// breadth-first — the root scope drains before the subpath scopes it
    /// discovered, so the most specific llms.txt governs every page it
    /// covers. A page a queued deeper scope covers and lists itself defers
    /// to that scope's turn, so the deeper listing — with its own relation
    /// hints — is what ingests; a page already buffered through a shallower
    /// listing is re-fetched there and its deeper listing replaces the
    /// buffered document in place.
    async fn walk_scopes(
        &self,
        root: Scope,
        docs: &mut Vec<PageDocument>,
        seen: &mut HashMap<String, usize>,
        run: &mut RunState,
    ) -> Result<(), IngestError> {
        let mut scopes: Vec<Scope> = Vec::new();
        let mut pending: std::collections::VecDeque<Scope> = std::collections::VecDeque::new();
        let mut scope_urls: HashSet<String> = HashSet::new();
        scope_urls.insert(root.index_url.to_string());
        pending.push_back(root);
        while let Some(scope) = pending.pop_front() {
            let scope_index = scopes.len();
            scopes.push(scope);
            let scope = &scopes[scope_index];

            // (a) Subpath discovery: a link naming an llms.txt opens a
            // deeper scope for its subtree.
            for link in scope.index.links() {
                let link_url = resolve_link(scope, link.url())?;
                if is_index_url(&link_url) {
                    self.queue_sub_scope(&link_url, &mut pending, &mut scope_urls, run)
                        .await?;
                }
            }

            // (b) The scope's pages. A page a queued deeper scope covers
            // and lists itself defers to that scope's turn — the deeper
            // listing wins, with its own relation hints
            // (docs/implementation/07 §llms.txt adapter rule 4).
            for link in scope.index.links() {
                let link_url = resolve_link(scope, link.url())?;
                if is_index_url(&link_url) {
                    continue;
                }
                if !is_page_url(&link_url) {
                    tracing::warn!("skipping non-markdown link {}", link_url);
                    run.events.skipped_pages += 1;
                    continue;
                }
                if most_specific_scope(&scopes, &link_url) != Some(scope_index)
                    || deferred_to_queued(&pending, &link_url)
                {
                    continue;
                }
                let described = self.ingest_link(link_url, link, docs, seen, run).await?;
                // A `rel="describedby"` response names the index that
                // covers the page; an unseen one joins the queue
                // (docs/implementation/07 §llms.txt adapter rule 2).
                if let Some(described) = described {
                    self.queue_sub_scope(&described, &mut pending, &mut scope_urls, run)
                        .await?;
                }
            }

            // (c) The llms-full probe (docs/implementation/07 §llms.txt
            // adapter rule 3).
            self.probe_llms_full(scope, docs, run).await?;
        }
        Ok(())
    }

    /// Queues an unseen index scope for the walk, counting it as a subpath
    /// index. A scope already queued or walked does not re-enter: one
    /// scope URL enters the queue at most once.
    async fn queue_sub_scope(
        &self,
        index_url: &Url,
        pending: &mut std::collections::VecDeque<Scope>,
        scope_urls: &mut HashSet<String>,
        run: &mut RunState,
    ) -> Result<(), IngestError> {
        if !scope_urls.insert(index_url.to_string()) {
            return Ok(());
        }
        run.events.subpath_indexes += 1;
        let sub = self.load_scope(index_url).await?;
        pending.push_back(sub);
        Ok(())
    }

    /// Probes the scope's `llms-full.txt` export
    /// (docs/implementation/07 §llms.txt adapter rule 3). When present,
    /// the export is one pre-built document and is buffered like any other
    /// markdown body.
    async fn probe_llms_full(
        &self,
        scope: &Scope,
        docs: &mut Vec<PageDocument>,
        run: &mut RunState,
    ) -> Result<(), IngestError> {
        let export_url = resolve_link(scope, "llms-full.txt")?;
        if let Some(body) = self.probe(&export_url).await? {
            tracing::info!(export = %export_url, "ingesting the llms-full export");
            docs.push(PageDocument {
                path: export_url.to_string(),
                body,
            });
            run.events.full_exports += 1;
        }
        Ok(())
    }

    /// The fetch-and-segment phase, inside the total-cap timeout: load the
    /// root scope, walk the scopes breadth-first, fetch every governed
    /// page, and buffer the corpus. Publishes nothing.
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
        let root = self.load_scope(index_url).await?;
        let segmenter = Segmenter::new()?;
        let mut run = RunState::default();
        let mut docs: Vec<PageDocument> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        self.walk_scopes(root, &mut docs, &mut seen, &mut run)
            .await?;

        // Normalization runs off the async path over the whole corpus:
        // comrak parsing and hashing are CPU work. Dedup first, so a
        // duplicate page cannot skew the chrome frequency counts; then the
        // chrome strip (docs/implementation/07 §Normalization pipeline).
        let (dedup_dropped, chrome_report, docs) = tokio::task::spawn_blocking(move || {
            let dropped = dedup_pages(&mut docs);
            let report = crate::chrome::strip_chrome(&mut docs);
            (dropped, report, docs)
        })
        .await
        .map_err(|join| IngestError::Join(join.to_string()))?;
        run.events.dedup_dropped_pages = dedup_dropped;
        run.events.chrome_pattern_drops = chrome_report.pattern_drops;
        run.events.chrome_drops = chrome_report.frequency_drops;

        // Segment every stripped document (docs/implementation/07
        // §Segmentation algorithm), one spawn_blocking per page.
        for doc in &docs {
            let (chunks, stats) =
                segment_page(&segmenter, library, version, doc.body.clone()).await?;
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

    /// Ingests one already-resolved page link: fetch, prefer the v2
    /// markdown twin over an HTML body, and buffer the markdown body into
    /// the corpus (docs/implementation/07 §llms.txt adapter rule 2).
    /// Segmentation happens after the chrome strip, over the whole buffered
    /// corpus. `seen` maps every URL to the buffered document it produced;
    /// a duplicate hit re-fetches the page and, when this listing produces
    /// a document, replaces the buffered one in place — the most specific
    /// listing wins (docs/implementation/07 §llms.txt adapter rule 4). A
    /// listing that produces nothing leaves the buffered document standing.
    /// Returns the page's `rel="describedby"` index URL, when the page
    /// response or the index hint named one.
    async fn ingest_link(
        &self,
        page_url: Url,
        link: &crate::llms_txt::LlmsLink,
        docs: &mut Vec<PageDocument>,
        seen: &mut HashMap<String, usize>,
        run: &mut RunState,
    ) -> Result<Option<Url>, IngestError> {
        let slot = seen.get(&page_url.to_string()).copied();
        let (body, final_url, link_headers) = self.fetch(&page_url).await?;
        let response_relations = if is_html_body(&body) {
            PageRelations::from_html(&body)
        } else {
            PageRelations::default()
        };
        let relations = PageRelations::from_link_headers(&link_headers)
            .merge(response_relations)
            .merge(PageRelations {
                markdown_twin: link.markdown_twin().map(str::to_owned),
                described_by: link.described_by().map(str::to_owned),
            });
        // Relative relation targets join against the final URL: a redirect
        // can move the page to another directory, and the pre-redirect
        // `page_url` would join them to the wrong base.
        let described = relations
            .described_by
            .as_deref()
            .and_then(|named| final_url.join(named).ok())
            .filter(is_index_url);

        if is_html_body(&body) {
            // The markdown twin wins over the HTML page (docs/07 rule 2).
            // Without one, the page falls back to its HTML path, which the
            // media split skips: no HTML extractor exists in this slice
            // (docs/implementation/07 §Media split). A page whose first
            // listing produces nothing stays out of `seen`, so a later,
            // more specific scope listing can ingest it with its twin hint.
            let Some(twin) = relations.markdown_twin.clone() else {
                if slot.is_none() {
                    tracing::warn!("html page {} carries no markdown twin; skipping", page_url);
                    run.events.skipped_pages += 1;
                }
                return Ok(described);
            };
            let twin_url = final_url
                .join(&twin)
                .map_err(|e| IngestError::LlmsTxtInvalid {
                    url: page_url.to_string(),
                    reason: format!("markdown twin {twin:?} does not resolve: {e}"),
                })?;
            if twin_url == page_url {
                // A twin that names the page itself: the page serves no
                // markdown body, so nothing buffers here.
                if slot.is_none() {
                    run.events.skipped_pages += 1;
                }
                return Ok(described);
            }
            if seen.contains_key(&twin_url.to_string()) {
                // The twin is already in the corpus through another route:
                // a first listing is covered by it, a duplicate listing
                // keeps the buffered document.
                return Ok(described);
            }
            let (twin_body, _, _) = self.fetch(&twin_url).await?;
            if is_html_body(&twin_body) {
                if slot.is_none() {
                    tracing::warn!(
                        "markdown twin {} served HTML; {} falls back to its HTML path",
                        twin_url,
                        page_url
                    );
                    run.events.skipped_pages += 1;
                }
                return Ok(described);
            }
            let idx = if let Some(idx) = slot {
                replace_buffered(
                    docs,
                    seen,
                    idx,
                    PageDocument {
                        path: twin_url.to_string(),
                        body: twin_body,
                    },
                );
                idx
            } else {
                let idx = docs.len();
                docs.push(PageDocument {
                    path: twin_url.to_string(),
                    body: twin_body,
                });
                seen.insert(twin_url.to_string(), idx);
                // The twin-use count tracks documents the buffer gains; an
                // in-place replacement adds none.
                run.events.markdown_twins_used += 1;
                idx
            };
            seen.insert(page_url.to_string(), idx);
            return Ok(described);
        }

        let doc = PageDocument {
            path: page_url.to_string(),
            body,
        };
        if let Some(idx) = slot {
            replace_buffered(docs, seen, idx, doc);
        } else {
            seen.insert(page_url.to_string(), docs.len());
            docs.push(doc);
        }
        Ok(described)
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
    /// Returns the body, the URL the response actually came from — the
    /// requested URL in replay (no redirects), the final URL on the wire —
    /// and every `Link` header of the final response, which carry the v2
    /// relations.
    async fn fetch(&self, url: &Url) -> Result<(String, Url, Vec<String>), IngestError> {
        self.guard.check(url)?;
        if let Some(fixture) = self.fixture {
            let recorded = fixture.serve(url).ok_or_else(|| {
                IngestError::Guard(crate::guard::GuardError::FixtureMiss(url.to_string()))
            })?;
            let body = self.replay_body(url, recorded)?;
            let link_headers = recorded.headers.get("link").cloned().into_iter().collect();
            return Ok((body, url.clone(), link_headers));
        }
        let (bytes, final_url, link_headers) = self.guard.get_with_link_headers(url).await?;
        String::from_utf8(bytes.to_vec())
            .map(|body| (body, final_url, link_headers))
            .map_err(|_| IngestError::LlmsTxtInvalid {
                url: url.to_string(),
                reason: String::from("the response is not valid UTF-8"),
            })
    }

    /// Validates a recorded response against the replay contracts and
    /// returns its body: a recorded non-success status and an oversized
    /// body fail the same way the live fetch would
    /// (docs/implementation/04 §Fixtures, docs/implementation/09 invariant
    /// 5). Both the fetch and the probe paths run through this check.
    fn replay_body(&self, url: &Url, recorded: &RecordedResponse) -> Result<String, IngestError> {
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
        Ok(recorded.body.clone())
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

/// One parsed llms.txt index and the subtree it covers
/// (docs/implementation/07 §llms.txt adapter rule 4).
struct Scope {
    /// The index URL, for errors and events.
    index_url: Url,
    /// The URL the index response actually came from — redirect-aware —
    /// which the index's relative links join against.
    join_base: Url,
    /// The directory the index covers: the most specific index for a page
    /// is the one whose directory is the longest path prefix of the page.
    dir: Url,
    /// The parsed index.
    index: crate::llms_txt::LlmsTxtIndex,
}

/// Resolves one index link against its scope's redirect-aware join base.
fn resolve_link(scope: &Scope, link: &str) -> Result<Url, IngestError> {
    scope
        .join_base
        .join(link)
        .map_err(|e| IngestError::LlmsTxtInvalid {
            url: scope.index_url.to_string(),
            reason: format!("link {link:?} does not resolve: {e}"),
        })
}

/// The directory URL of `url`: the path up to and including the last slash.
fn directory_url(url: &Url) -> Url {
    let mut dir = url.clone();
    let path = url.path();
    let upto = path.rfind('/').map_or(0, |at| at + 1);
    dir.set_path(&path[..upto]);
    dir
}

/// Whether a URL names a subpath index: its file name is `llms.txt`
/// (`llms-full.txt` is the export, not an index).
fn is_index_url(url: &Url) -> bool {
    url.path()
        .split('/')
        .next_back()
        .is_some_and(|file| file.eq_ignore_ascii_case("llms.txt"))
}

/// The scope covering `page_url` with the longest directory path — the
/// most specific llms.txt applies (docs/implementation/07 §llms.txt
/// adapter rule 4).
fn most_specific_scope(scopes: &[Scope], page_url: &Url) -> Option<usize> {
    scopes
        .iter()
        .enumerate()
        .filter(|(_index, scope)| page_url.path().starts_with(scope.dir.path()))
        .max_by_key(|(_index, scope)| scope.dir.path().len())
        .map(|(index, _scope)| index)
}

/// Whether a queued scope covers `page_url` and lists it itself: the page
/// then defers to the deeper scope's turn, whose own listing — with its
/// own relation hints — ingests it. A deeper scope that covers the page
/// without listing it leaves the shallower listing in charge: the more
/// specific index that actually describes the page is the one whose hints
/// apply.
fn deferred_to_queued(pending: &std::collections::VecDeque<Scope>, page_url: &Url) -> bool {
    pending.iter().any(|scope| {
        page_url.path().starts_with(scope.dir.path())
            && scope.index.links().any(|link| {
                scope
                    .join_base
                    .join(link.url())
                    .is_ok_and(|listed| listed == *page_url)
            })
    })
}

/// Replaces the buffered document at `idx` with `doc` in place: the buffer
/// grows only by push, never reorder. The replaced document's URL mapping
/// drops when it named another route, so a stale listing cannot restore a
/// body the corpus no longer holds; `doc`'s own URL maps to `idx`.
fn replace_buffered(
    docs: &mut [PageDocument],
    seen: &mut HashMap<String, usize>,
    idx: usize,
    doc: PageDocument,
) {
    let old_path = docs[idx].path.clone();
    docs[idx] = doc;
    if old_path != docs[idx].path && seen.get(&old_path).is_some_and(|&at| at == idx) {
        seen.remove(&old_path);
    }
    seen.insert(docs[idx].path.clone(), idx);
}

/// Collapses duplicate pages by canonical body hash: the first occurrence
/// of a SHA-256 content hash stays, every later duplicate drops. The hash
/// is the store's canonical hash (CRLF folds to LF), so twins that differ
/// only in line endings collapse too — a deliberate refinement of the
/// ticket's raw "compares SHA-256" wording: line-ending twins are the same
/// page (docs/implementation/07 §Normalization pipeline stage 6). Returns
/// the dropped count.
fn dedup_pages(docs: &mut Vec<PageDocument>) -> usize {
    let before = docs.len();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    docs.retain(|doc| seen.insert(devdocs_store::content_hash(&doc.body)));
    let dropped = before - docs.len();
    if dropped > 0 {
        tracing::debug!(dropped, "duplicate pages collapsed by content hash");
    }
    dropped
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

/// Whether the URL path names a page the slice can ingest: markdown, HTML,
/// or an extensionless directory-style path. The index and export files
/// (`llms.txt`, `llms-full.txt`) are not pages.
fn is_page_url(url: &Url) -> bool {
    // defensive: `split` always yields at least one element, so the else
    // branch never runs; the let-else keeps `file` a plain `&str` instead
    // of an `Option`.
    let Some(file) = url.path().split('/').next_back() else {
        return false;
    };
    if file.is_empty() {
        return true;
    }
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return true;
    };
    let stem = stem.to_ascii_lowercase();
    let ext = ext.to_ascii_lowercase();
    if (stem == "llms" || stem == "llms-full") && ext == "txt" {
        return false;
    }
    matches!(ext.as_str(), "md" | "mdx" | "html" | "htm" | "xhtml")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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

    /// Page URLs span markdown, HTML, and extensionless paths; assets,
    /// index files, and exports are not pages.
    #[test]
    fn page_urls_span_the_page_shapes() {
        let url = |path: &str| Url::parse(&format!("https://x{path}")).unwrap();
        for path in ["/intro.md", "/intro.mdx", "/intro.html", "/docs/intro", "/"] {
            assert!(is_page_url(&url(path)), "{path}");
        }
        for path in [
            "/llms.txt",
            "/docs/llms.txt",
            "/llms-full.txt",
            "/logo.png",
            "/spec.pdf",
        ] {
            assert!(!is_page_url(&url(path)), "{path}");
        }
    }

    /// Duplicate pages collapse to the first occurrence; distinct pages
    /// stay; CRLF/LF twins collapse through the canonical hash
    /// (docs/implementation/07 §Normalization pipeline stage 6).
    #[test]
    fn dedup_collapses_identical_bodies() {
        use crate::chrome::PageDocument;

        let doc = |path: &str, body: &str| PageDocument {
            path: path.to_owned(),
            body: body.to_owned(),
        };
        let mut docs = vec![
            doc("https://x/a.md", "# A\n\nalpha\n"),
            doc("https://x/b.md", "# B\n\nbeta\n"),
            doc("https://x/a-copy.md", "# A\n\nalpha\n"),
            doc("https://x/crlf.md", "# A\r\n\r\nalpha\r\n"),
            doc("https://x/c.md", "# C\n\ngamma\n"),
        ];
        let dropped = dedup_pages(&mut docs);
        assert_eq!(dropped, 2, "the copy and the CRLF twin collapse");
        let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(
            paths,
            ["https://x/a.md", "https://x/b.md", "https://x/c.md"]
        );
    }

    /// Index URLs are the llms.txt files; the export file is not an index.
    #[test]
    fn index_urls_name_llms_txt() {
        let url = |path: &str| Url::parse(&format!("https://x{path}")).unwrap();
        assert!(is_index_url(&url("/llms.txt")));
        assert!(is_index_url(&url("/docs/api/llms.txt")));
        assert!(!is_index_url(&url("/llms-full.txt")));
        assert!(!is_index_url(&url("/docs/page.md")));
    }

    /// The most specific index governs: the deepest directory whose path
    /// prefixes the page wins
    /// (docs/implementation/07 §llms.txt adapter rule 4).
    #[test]
    fn the_most_specific_scope_governs() {
        let url = |path: &str| Url::parse(&format!("https://x{path}")).unwrap();
        let scope = |dir: &str| Scope {
            index_url: url("/dev/null"),
            join_base: url("/dev/null"),
            dir: url(dir),
            index: parse_llms_txt("# T\n\n- [a](/a.md)\n", "u").unwrap(),
        };
        let scopes = vec![scope("/"), scope("/docs/"), scope("/docs/api/")];
        assert_eq!(most_specific_scope(&scopes, &url("/overview.md")), Some(0));
        assert_eq!(
            most_specific_scope(&scopes, &url("/docs/intro.md")),
            Some(1)
        );
        assert_eq!(
            most_specific_scope(&scopes, &url("/docs/api/auth.md")),
            Some(2)
        );
        // The root covers everything, so nothing is ungoverned.
        assert_eq!(
            most_specific_scope(&scopes, &url("/elsewhere/page.md")),
            Some(0)
        );
    }
}
