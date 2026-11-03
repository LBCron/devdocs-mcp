//! The ingest event report (docs/implementation/07 §Normalization pipeline).
//!
//! One report per run accumulates what the pipeline dropped or detoured
//! around: skipped pages and blocks, unknown fence languages, chrome drops,
//! and dedup counts. Provenance only — counts and URLs, never chunk text
//! (.agents/instructions/observability). The counts surface in two places:
//! as stderr log lines under `--verbose` (the run logs one line per event
//! category at `info`), and as the `events` object of the `--json` add
//! envelope. The default filter is `warn`, so the event lines stay hidden
//! without `--verbose`.

use serde::Serialize;

/// What one ingest run skipped, dropped, or detoured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IngestEvents {
    /// Pages ingested.
    pub pages: usize,
    /// Links skipped before or during the media split (non-page URLs, HTML
    /// bodies without a markdown twin, HTML-serving markdown paths).
    pub skipped_pages: usize,
    /// Blocks dropped by the chrome-strip pattern rules (leading
    /// navigation, "edit this page" links, cookie banners) plus blocks the
    /// segmenter skipped for carrying no content.
    pub skipped_blocks: usize,
    /// Fenced code blocks whose language is named but not in the known set.
    pub unknown_fence_languages: usize,
    /// Blocks dropped by the cross-page chrome frequency rule: a block
    /// identical in more than 60% of the corpus's pages
    /// (docs/implementation/07 §Normalization pipeline stage 2).
    pub chrome_drops: usize,
    /// Duplicate pages collapsed by the exact SHA-256 dedup; the dropped
    /// count (docs/implementation/07 §Normalization pipeline stage 6).
    pub dedup_dropped_pages: usize,
    /// Pages ingested through a `rel="alternate"` markdown twin instead of
    /// their HTML path.
    pub markdown_twins_used: usize,
    /// Subpath-scoped indexes discovered beyond the root index.
    pub subpath_indexes: usize,
    /// `llms-full.txt` exports found by the probe and ingested as one
    /// pre-built document each.
    pub full_exports: usize,
}

impl IngestEvents {
    /// Logs one stderr line per category. The default filter is `warn`, so
    /// these lines surface only under `--verbose`.
    pub fn log(&self) {
        tracing::info!(
            pages = self.pages,
            skipped_pages = self.skipped_pages,
            "ingest events: {pages} pages, {skipped_pages} skipped pages",
            pages = self.pages,
            skipped_pages = self.skipped_pages,
        );
        if self.skipped_blocks > 0 {
            tracing::info!(
                skipped_blocks = self.skipped_blocks,
                "ingest events: {count} blocks skipped by chrome patterns or empty-block skips",
                count = self.skipped_blocks
            );
        }
        if self.unknown_fence_languages > 0 {
            tracing::info!(
                unknown_fence_languages = self.unknown_fence_languages,
                "ingest events: {count} fenced blocks carry an unknown language",
                count = self.unknown_fence_languages,
            );
        }
        if self.chrome_drops > 0 {
            tracing::info!(
                chrome_drops = self.chrome_drops,
                "ingest events: {count} blocks dropped as repeated chrome",
                count = self.chrome_drops,
            );
        }
        if self.dedup_dropped_pages > 0 {
            tracing::info!(
                dedup_dropped_pages = self.dedup_dropped_pages,
                "ingest events: {count} duplicate pages collapsed",
                count = self.dedup_dropped_pages,
            );
        }
        if self.markdown_twins_used > 0 {
            tracing::info!(
                markdown_twins_used = self.markdown_twins_used,
                "ingest events: {count} pages ingested via their markdown twin",
                count = self.markdown_twins_used,
            );
        }
        if self.subpath_indexes > 0 {
            tracing::info!(
                subpath_indexes = self.subpath_indexes,
                "ingest events: {count} subpath indexes scoped",
                count = self.subpath_indexes,
            );
        }
        if self.full_exports > 0 {
            tracing::info!(
                full_exports = self.full_exports,
                "ingest events: {count} llms-full exports ingested",
                count = self.full_exports,
            );
        }
    }
}
