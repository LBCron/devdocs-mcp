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
    /// Blocks the segmenter skipped for carrying no content.
    pub skipped_blocks: usize,
    /// Blocks dropped by the chrome-strip pattern rules (leading
    /// navigation, "edit this page" links, cookie banners).
    pub chrome_pattern_drops: usize,
    /// Fenced code blocks whose language is named but not in the known set.
    pub unknown_fence_languages: usize,
    /// Fenced code blocks whose language the symbol pass would consume but
    /// whose grammar is not in this build (a disabled `grammar-*` feature,
    /// or the TypeScript aliases).
    pub unsupported_fence_languages: usize,
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
            "ingest events: {} pages, {} skipped pages",
            self.pages,
            self.skipped_pages,
        );
        let counters = [
            (
                "blocks skipped for carrying no content",
                self.skipped_blocks,
            ),
            (
                "blocks dropped by the chrome pattern rules",
                self.chrome_pattern_drops,
            ),
            (
                "fenced blocks carry an unknown language",
                self.unknown_fence_languages,
            ),
            (
                "fenced blocks name a symbol language without its grammar",
                self.unsupported_fence_languages,
            ),
            ("blocks dropped as repeated chrome", self.chrome_drops),
            ("duplicate pages collapsed", self.dedup_dropped_pages),
            (
                "pages ingested via their markdown twin",
                self.markdown_twins_used,
            ),
            ("subpath indexes scoped", self.subpath_indexes),
            ("llms-full exports ingested", self.full_exports),
        ];
        for (category, count) in counters {
            if count > 0 {
                tracing::info!(count, category, "ingest event: {category}: {count}",);
            }
        }
    }
}
