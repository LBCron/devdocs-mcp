//! Segmentation: comrak parse, heading walk, block packing
//! (docs/implementation/07 §Segmentation algorithm).
//!
//! Sections build at heading boundaries with a breadcrumb path. Inside a
//! section, consecutive blocks pack into chunks up to the token target; a
//! block over the target but within twice the target splits with the
//! configured overlap; a block over twice the target becomes its own chunk,
//! never truncated. Code fences never split. Chunk bodies are byte-exact
//! slices of the page body, so the exclusive (non-overlap) spans of a
//! document tile it; the contextual header is prepended at chunk assembly
//! and excluded from the content hash.

use std::ops::Range;

use comrak::nodes::{NodeHeading, NodeValue};
use comrak::{Arena, Options, parse_document};
use text_splitter::{ChunkConfig, ChunkSizer, MarkdownSplitter};

use crate::error::IngestError;

/// The segmentation target (docs/implementation/07: target 400 tokens,
/// overlap 80 tokens).
const TARGET_TOKENS: usize = 400;
/// The segmentation overlap.
const OVERLAP_TOKENS: usize = 80;

/// How chunk sizes are measured (docs/implementation/07: the chars-per-4
/// heuristic by default, tiktoken o200k under the `exact-tokens` feature).
#[derive(Clone)]
enum TokenSizer {
    /// `ceil(chars / 4)` — the default heuristic.
    Heuristic,
    /// tiktoken o200k, exact mode.
    #[cfg(feature = "exact-tokens")]
    Exact(std::sync::Arc<tiktoken_rs::CoreBPE>),
}

impl TokenSizer {
    /// The token count of `text` in the active unit.
    fn size(&self, text: &str) -> usize {
        match self {
            Self::Heuristic => text.chars().count().div_ceil(4),
            #[cfg(feature = "exact-tokens")]
            Self::Exact(bpe) => ChunkSizer::size(bpe.as_ref(), text),
        }
    }
}

/// The `text-splitter` sizer adapter so the splitter measures in the same
/// unit the stored `token_count` uses.
struct SplitterSizer(TokenSizer);

impl ChunkSizer for SplitterSizer {
    fn size(&self, text: &str) -> usize {
        self.0.size(text)
    }
}

/// One segmentation output chunk: the body is a byte-exact slice of the
/// page body, with the contextual header prepended at assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedChunk {
    /// Breadcrumb of heading titles from the page root.
    pub section_path: Vec<String>,
    /// The contextual header `[library version › section path]`.
    pub header: String,
    /// The chunk body without the header.
    pub body: String,
    /// The contextual header prepended to the body.
    pub text: String,
    /// Token count of `text` in the active unit.
    pub token_count: u32,
    /// SHA-256 of the canonical body without the header
    /// (docs/implementation/06 §Hashing policy).
    pub content_hash: [u8; 32],
    /// The byte range of `body` inside the page body; adjacent chunks may
    /// share only the configured overlap.
    pub page_range: std::ops::Range<usize>,
}

/// The segmenter: one sizer plus the packed limits
/// (docs/implementation/07 §Segmentation algorithm).
#[derive(Clone)]
pub struct Segmenter {
    sizer: TokenSizer,
    target_tokens: usize,
    overlap_tokens: usize,
}

impl std::fmt::Debug for Segmenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segmenter")
            .field("target_tokens", &self.target_tokens)
            .field("overlap_tokens", &self.overlap_tokens)
            .finish_non_exhaustive()
    }
}

impl Segmenter {
    /// Builds the default segmenter: target 400, overlap 80.
    ///
    /// # Errors
    /// [`IngestError::Tokenizer`] when the exact tokenizer cannot load.
    pub fn new() -> Result<Self, IngestError> {
        Self::with_limits(TARGET_TOKENS, OVERLAP_TOKENS)
    }

    /// Builds a segmenter with explicit limits; the overlap must stay below
    /// the target.
    ///
    /// # Errors
    /// [`IngestError::Tokenizer`] when the exact tokenizer cannot load.
    pub fn with_limits(target_tokens: usize, overlap_tokens: usize) -> Result<Self, IngestError> {
        if overlap_tokens >= target_tokens {
            return Err(IngestError::Tokenizer {
                message: format!(
                    "overlap {overlap_tokens} must stay below the target {target_tokens}"
                ),
            });
        }
        Ok(Self {
            sizer: build_sizer()?,
            target_tokens,
            overlap_tokens,
        })
    }

    /// Segments one page body into chunks with contextual headers.
    #[must_use]
    pub fn segment(&self, library: &str, version: &str, page_body: &str) -> Vec<PreparedChunk> {
        let mut chunks = Vec::new();
        for section in Self::sections(page_body) {
            for span in self.pack_section(&section) {
                let body = &page_body[span.start..span.end];
                let header = contextual_header(library, version, &section.breadcrumb);
                let text = format!("{header}\n\n{body}");
                let token_count = u32::try_from(self.sizer.size(&text)).unwrap_or(u32::MAX);
                chunks.push(PreparedChunk {
                    section_path: section.breadcrumb.clone(),
                    header,
                    body: body.to_owned(),
                    text,
                    token_count,
                    content_hash: devdocs_store::content_hash(body),
                    page_range: span,
                });
            }
        }
        chunks
    }

    /// Walks the comrak tree and returns the sections: breadcrumb plus the
    /// absolute byte ranges of their blocks inside `page_body`.
    fn sections(page_body: &str) -> Vec<Section<'_>> {
        let arena = Arena::new();
        let root = parse_document(&arena, page_body, &Options::default());

        // Byte offset of every line start; (line, col) are 1-based.
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(
                page_body
                    .bytes()
                    .enumerate()
                    .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
            )
            .collect();
        let offset = |line: usize, col: usize| -> usize {
            line_starts
                .get(line - 1)
                .copied()
                .unwrap_or(page_body.len())
                + col.saturating_sub(1)
        };

        let mut sections: Vec<Section> = Vec::new();
        let mut breadcrumb: Vec<(u8, String)> = Vec::new();

        for node in root.children() {
            match &node.data().value {
                NodeValue::Heading(NodeHeading { level, .. }) => {
                    push_heading(
                        &mut sections,
                        &mut breadcrumb,
                        *level,
                        &inline_text(node),
                        page_body,
                    );
                }
                _ => push_block(&mut sections, node, page_body, &offset),
            }
        }
        sections.retain(|section| !section.blocks.is_empty());
        sections
    }

    /// Packs one section's blocks into chunk spans (byte-exact slices of
    /// the page) and merges until no boundary sits inside a fenced code
    /// block.
    fn pack_section(&self, section: &Section<'_>) -> Vec<Range<usize>> {
        let mut spans: Vec<Range<usize>> = Vec::new();
        let mut current: Option<(Range<usize>, usize)> = None; // span, tokens

        for block in &section.blocks {
            let range = block.start..block.end;
            let text = &section.text[range.clone()];
            let tokens = self.sizer.size(text);
            let oversized = tokens > self.target_tokens;
            if oversized && (block.fenced || tokens > 2 * self.target_tokens) {
                // A fenced code block never splits, and any block over
                // twice the target becomes its own chunk, never truncated
                // (docs/07 rules 3 and 5).
                if let Some((span, _)) = current.take() {
                    spans.push(span);
                }
                spans.push(range);
            } else if oversized {
                // Between the target and twice the target: split at
                // semantic boundaries with the overlap.
                if let Some((span, _)) = current.take() {
                    spans.push(span);
                }
                spans.extend(self.split_oversized(text, block.start));
            } else if tokens > 0 {
                // The packed span slices the page from the first block's
                // start to the last block's end, so the bytes between
                // blocks join the chunk body and share the budget
                // (docs/07 rule 3).
                let separator_tokens = match &current {
                    Some((span, _)) => self.sizer.size(&section.text[span.end..block.start]),
                    None => 0,
                };
                match &current {
                    Some((_, packed))
                        if packed + separator_tokens + tokens > self.target_tokens =>
                    {
                        if let Some((span, _)) = current.take() {
                            spans.push(span);
                        }
                        current = Some((range, tokens));
                    }
                    Some((span, packed)) => {
                        let packed = *packed + separator_tokens + tokens;
                        current = Some((span.start..block.end, packed));
                    }
                    None => current = Some((range, tokens)),
                }
            }
        }
        if let Some((span, _)) = current.take() {
            spans.push(span);
        }

        // Cross-check: no chunk boundary may fall inside a fenced code
        // block; merge and warn when it does (docs/07 step 4). The probe
        // offset is page-absolute, matching `section.text`.
        merge_until_fence_safe(spans, section.text)
    }

    /// Splits an oversized block with the `MarkdownSplitter` and returns the
    /// piece spans in absolute page offsets. `with_limits` validated
    /// overlap < target, so the overlap-bearing config cannot fail to
    /// build; the fallback only drops the overlap.
    fn split_oversized(&self, text: &str, block_start: usize) -> Vec<Range<usize>> {
        let config = match ChunkConfig::new(self.target_tokens)
            .with_sizer(SplitterSizer(self.sizer.clone()))
            .with_trim(false)
            .with_overlap(self.overlap_tokens)
        {
            Ok(config) => config,
            Err(_) => ChunkConfig::new(self.target_tokens)
                .with_sizer(SplitterSizer(self.sizer.clone()))
                .with_trim(false),
        };
        let splitter = MarkdownSplitter::new(config);
        splitter
            .chunk_char_indices(text)
            .map(|piece| {
                let start = piece.byte_offset;
                let end = start + piece.chunk.len();
                block_start + start..block_start + end
            })
            .collect()
    }
}

/// The contextual header `[library version › section path]`
/// (docs/implementation/07 §Normalization pipeline stage 4).
#[must_use]
pub fn contextual_header(library: &str, version: &str, section_path: &[String]) -> String {
    if section_path.is_empty() {
        format!("[{library} {version}]")
    } else {
        format!("[{library} {version} › {}]", section_path.join(" › "))
    }
}

/// One top-level block of a section: its absolute byte range and whether
/// the block is a fenced code block (never split).
struct Block {
    start: usize,
    end: usize,
    fenced: bool,
}

/// One section under construction.
struct Section<'a> {
    text: &'a str,
    breadcrumb: Vec<String>,
    blocks: Vec<Block>,
}

/// The absolute byte range of one top-level block node. Comrak's end
/// column points at the last character, so the exclusive end is offset+1.
fn block_range(
    node: &comrak::nodes::AstNode<'_>,
    offset: &(dyn Fn(usize, usize) -> usize + '_),
) -> (usize, usize) {
    let pos = node.data().sourcepos;
    let start = offset(pos.start.line, pos.start.column);
    let end = offset(pos.end.line, pos.end.column) + 1;
    (start, end)
}

/// Collects the inline text of a heading node (its literal text and inline
/// code).
fn inline_text<'a>(node: &'a comrak::nodes::AstNode<'a>) -> String {
    let mut out = String::new();
    for desc in node.descendants() {
        match &desc.data().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// Merges candidate spans until no boundary falls inside a fenced code
/// block; logs one warning per merge (docs/07 §Segmentation algorithm
/// step 4). Span ends and `section_text` are both page-absolute.
///
/// The fence state is computed once per section in a single forward pass
/// ([`fence_intervals`], issue #64): each boundary check consults the
/// precomputed intervals instead of rescanning the section text from its
/// first line, which made the merge quadratic in the section size. The
/// merge itself is one left-to-right pass — the earlier loop restarted
/// its boundary scan after every merge, quadratic in the span count —
/// and sections with fewer than two spans return before any scan, since
/// every section passes the whole page text.
fn merge_until_fence_safe(spans: Vec<Range<usize>>, section_text: &str) -> Vec<Range<usize>> {
    if spans.len() < 2 {
        return spans;
    }
    let fences = fence_intervals(section_text);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if boundary_inside_fence(&fences, last.end) => {
                last.end = span.end;
                tracing::warn!(
                    "chunk boundary fell inside a fenced code block; merged the two candidate chunks"
                );
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// Records one heading node: pop the breadcrumb to the heading's level,
/// push the title, and open a new empty section for its blocks.
fn push_heading<'a>(
    sections: &mut Vec<Section<'a>>,
    breadcrumb: &mut Vec<(u8, String)>,
    level: u8,
    title: &str,
    page_body: &'a str,
) {
    while breadcrumb.last().is_some_and(|(l, _)| *l >= level) {
        breadcrumb.pop();
    }
    breadcrumb.push((level, title.to_owned()));
    sections.push(Section {
        text: page_body,
        breadcrumb: breadcrumb.iter().map(|(_, t)| t.clone()).collect(),
        blocks: Vec::new(),
    });
}

/// Records one content block node into the section it belongs to, opening
/// the heading-less leading section when the page starts with content.
fn push_block<'a>(
    sections: &mut Vec<Section<'a>>,
    node: &comrak::nodes::AstNode,
    page_body: &'a str,
    offset: &impl Fn(usize, usize) -> usize,
) {
    let fenced = matches!(&node.data().value, NodeValue::CodeBlock(boxed) if boxed.fenced);
    let (start, end) = block_range(node, offset);
    let end = end.min(page_body.len());
    if start >= end {
        return;
    }
    if sections.is_empty() {
        sections.push(Section {
            text: page_body,
            breadcrumb: Vec::new(),
            blocks: Vec::new(),
        });
    }
    if let Some(last) = sections.last_mut() {
        last.blocks.push(Block { start, end, fenced });
    }
}

/// The byte ranges of `text` that sit inside an open fenced code block,
/// computed in one forward pass over the lines (issue #64). The intervals
/// carry the same `CommonMark` fence semantics as the index scanner
/// ([`crate::llms_txt::fence_marker`]): the opening marker's character and
/// run length, and only a marker-only line of the same character with at
/// least that run closes the fence.
///
/// The verdicts match the per-boundary rescan this replaces, byte for byte:
/// a boundary at a line start takes the fence state entering the line, so
/// the start of an unindented opening fence line stays outside the
/// interval, and a boundary past the closing marker run is outside. Ends
/// are exclusive, except an unclosed fence never ends (`usize::MAX`): the
/// probe's running state stays open to the end of the text and beyond, so
/// every offset from the opening marker on reads inside.
fn fence_intervals(text: &str) -> Vec<Range<usize>> {
    let mut intervals: Vec<Range<usize>> = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut open_start = 0usize;
    let mut offset = 0usize;
    for line in text.split('\n') {
        let line_start = offset;
        let trimmed = line.trim();
        match open {
            Some(open_fence) => {
                if crate::llms_txt::fence_closes(open_fence, trimmed) {
                    let left_trimmed = line.trim_start();
                    let indent = line.len() - left_trimmed.len();
                    let marker_end = line_start + indent + marker_run(open_fence.0, left_trimmed);
                    intervals.push(open_start..marker_end);
                    open = None;
                }
            }
            None => {
                if let Some((marker, run)) = crate::llms_txt::fence_marker(trimmed) {
                    let left_trimmed = line.trim_start();
                    let indent = line.len() - left_trimmed.len();
                    let marker_start = line_start + indent;
                    // A boundary on the line start takes the state entering
                    // the line (outside), so an unindented marker line opens
                    // the interval one byte past its start.
                    open_start = if indent == 0 {
                        marker_start + 1
                    } else {
                        marker_start
                    };
                    open = Some((marker, run));
                }
            }
        }
        offset = line_start + line.len() + 1;
    }
    if open.is_some() {
        intervals.push(open_start..usize::MAX);
    }
    intervals
}

/// Whether the byte offset `at` sits inside one of the fence intervals
/// computed by [`fence_intervals`]. The intervals are sorted and disjoint,
/// so one binary search answers the probe.
fn boundary_inside_fence(fences: &[Range<usize>], at: usize) -> bool {
    let past = fences.partition_point(|fence| fence.start <= at);
    past > 0 && at < fences[past - 1].end
}

/// The marker run length of a line already known to start with the marker.
fn marker_run(marker: char, trimmed: &str) -> usize {
    trimmed.chars().take_while(|&c| c == marker).count()
}

/// Builds the active sizer: tiktoken o200k under `exact-tokens`, the
/// chars-per-4 heuristic otherwise. The `Result` keeps the signature stable
/// across feature builds; the heuristic build cannot fail.
#[cfg_attr(not(feature = "exact-tokens"), allow(clippy::unnecessary_wraps))]
fn build_sizer() -> Result<TokenSizer, IngestError> {
    #[cfg(feature = "exact-tokens")]
    {
        let bpe = tiktoken_rs::o200k_base().map_err(|e| IngestError::Tokenizer {
            message: format!("cannot load the o200k tokenizer: {e}"),
        })?;
        Ok(TokenSizer::Exact(std::sync::Arc::new(bpe)))
    }
    #[cfg(not(feature = "exact-tokens"))]
    {
        Ok(TokenSizer::Heuristic)
    }
}

#[cfg(test)]
mod tests;
