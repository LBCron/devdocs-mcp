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

/// The fence languages the ingest knows. The tree-sitter grammar set of the
/// M5 symbol pass owns the full map; this list is the languages those
/// grammars cover plus the formats docs pages commonly fence. A fence whose
/// first info-string token is named but absent here is counted as an
/// unknown fence language — reported, never dropped.
pub(crate) const KNOWN_FENCE_LANGUAGES: &[&str] = &[
    "rust",
    "go",
    "golang",
    "python",
    "py",
    "javascript",
    "js",
    "jsx",
    "typescript",
    "ts",
    "tsx",
    "bash",
    "sh",
    "shell",
    "zsh",
    "console",
    "json",
    "yaml",
    "yml",
    "toml",
    "html",
    "css",
    "markdown",
    "md",
    "text",
    "txt",
    "diff",
    "c",
    "cpp",
    "java",
    "sql",
    "xml",
    "http",
    "ini",
];

/// The per-page counters one segmentation pass produces
/// (docs/implementation/07 §Normalization pipeline, the ingest events).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentStats {
    /// Blocks skipped for carrying no content (a collapsed or zero-token
    /// range).
    pub skipped_blocks: usize,
    /// Fenced code blocks whose info string names a language outside
    /// the known fence language set of this module.
    pub unknown_fence_languages: usize,
}

/// The fence language of an info string: the first token, lower-cased. An
/// empty info string names no language.
fn fence_language(info: &str) -> Option<&str> {
    let token = info.split_whitespace().next()?;
    let token = token.strip_prefix("language-").unwrap_or(token);
    let token = token.strip_prefix("lang-").unwrap_or(token);
    Some(token)
}

/// Whether a fence info string names no language or a known one. An unnamed
/// fence names no language, so it never counts as unknown.
fn fence_language_known_or_unnamed(info: &str) -> bool {
    fence_language(info).is_none_or(|lang| {
        KNOWN_FENCE_LANGUAGES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(lang))
    })
}

/// Whether a fence info string names a language outside the known set.
pub(crate) fn fence_language_unknown(info: &str) -> bool {
    !fence_language_known_or_unnamed(info)
}

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
        self.segment_with_stats(library, version, page_body).0
    }

    /// Segments one page body and reports the per-page event counters the
    /// run accumulates into the ingest events.
    #[must_use]
    pub fn segment_with_stats(
        &self,
        library: &str,
        version: &str,
        page_body: &str,
    ) -> (Vec<PreparedChunk>, SegmentStats) {
        let (sections, mut stats) = Self::sections(page_body);
        let mut chunks = Vec::new();
        for section in &sections {
            for span in self.pack_section(section, &mut stats) {
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
        (chunks, stats)
    }

    /// Walks the comrak tree and returns the sections: breadcrumb plus the
    /// absolute byte ranges of their blocks inside `page_body`. Counts the
    /// unknown fence languages on the way.
    fn sections(page_body: &str) -> (Vec<Section<'_>>, SegmentStats) {
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
        let mut stats = SegmentStats::default();

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
                NodeValue::CodeBlock(blocked) if blocked.fenced => {
                    if fence_language_unknown(&blocked.info) {
                        stats.unknown_fence_languages += 1;
                        tracing::debug!(language = %blocked.info.trim(), "fenced code block carries an unknown language");
                    }
                    push_block(&mut sections, node, page_body, &offset, &mut stats);
                }
                _ => push_block(&mut sections, node, page_body, &offset, &mut stats),
            }
        }
        sections.retain(|section| !section.blocks.is_empty());
        (sections, stats)
    }

    /// Packs one section's blocks into chunk spans (byte-exact slices of
    /// the page) and merges until no boundary sits inside a fenced code
    /// block.
    fn pack_section(&self, section: &Section<'_>, stats: &mut SegmentStats) -> Vec<Range<usize>> {
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
            } else {
                // defensive: a zero-token block carries no content; skip it
                // and count the skip (the ingest events report skipped
                // blocks). Unreachable with the default chars/4 sizer — it
                // never scores a non-empty slice zero tokens — and only
                // reachable under `exact-tokens`, where the BPE may score a
                // non-empty slice zero tokens.
                stats.skipped_blocks += 1;
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
fn merge_until_fence_safe(mut spans: Vec<Range<usize>>, section_text: &str) -> Vec<Range<usize>> {
    loop {
        let mut violation: Option<usize> = None;
        for (index, window) in spans.windows(2).enumerate() {
            if boundary_inside_fence(section_text, window[0].end) {
                violation = Some(index);
                break;
            }
        }
        match violation {
            Some(index) => {
                let next = spans.remove(index + 1);
                spans[index] = spans[index].start..next.end;
                tracing::warn!(
                    "chunk boundary fell inside a fenced code block; merged the two candidate chunks"
                );
            }
            None => return spans,
        }
    }
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
/// the heading-less leading section when the page starts with content. A
/// collapsed range is skipped and counted.
fn push_block<'a>(
    sections: &mut Vec<Section<'a>>,
    node: &comrak::nodes::AstNode,
    page_body: &'a str,
    offset: &impl Fn(usize, usize) -> usize,
    stats: &mut SegmentStats,
) {
    let fenced = matches!(&node.data().value, NodeValue::CodeBlock(boxed) if boxed.fenced);
    let (start, end) = block_range(node, offset);
    let end = end.min(page_body.len());
    if start >= end {
        // defensive: a collapsed range is skipped and counted. Comrak never
        // produces one for a top-level block — its sourcepos ranges span at
        // least one byte — so the check stays only as a guard against a
        // comrak regression.
        stats.skipped_blocks += 1;
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

/// Whether the byte offset `at` (relative to `text`) sits inside an open
/// fenced code block. Best-effort regression net for the splitter's
/// atomicity guarantee, with `CommonMark` fence semantics: the probe tracks
/// the opening marker (character and run length), and only a marker-only
/// line of the same character with at least that run closes the fence —
/// the same rule the index scanner uses
/// ([`crate::llms_txt::fence_marker`]). A boundary at a line start takes
/// the fence state entering the line; a boundary past a closing fence
/// marker is outside.
fn boundary_inside_fence(text: &str, at: usize) -> bool {
    let mut offset = 0usize;
    let mut open: Option<(char, usize)> = None;
    for line in text.split('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        if at <= line_start {
            return open.is_some();
        }
        if at < line_end {
            let trimmed = line.trim_start();
            let Some((marker, _run)) = crate::llms_txt::fence_marker(trimmed) else {
                return open.is_some();
            };
            let Some(open) = open else {
                // Outside, on a fence line: the bytes before the marker sit
                // outside, from the marker on is inside the fence.
                let indent = line.len() - trimmed.len();
                let marker_start = line_start + indent;
                return at >= marker_start;
            };
            if crate::llms_txt::fence_closes(open, line.trim()) {
                // A closing fence: past the marker run is outside.
                let indent = line.len() - trimmed.len();
                let marker_end = line_start + indent + marker_run(marker, trimmed);
                return at < marker_end;
            }
            // Any other line inside an open fence is fence content.
            return true;
        }
        // Cross the line: apply the same open/close rule the scanner uses.
        let trimmed = line.trim();
        match open {
            Some(open_fence) => {
                if crate::llms_txt::fence_closes(open_fence, trimmed) {
                    open = None;
                }
            }
            None => {
                if let Some((marker, run)) = crate::llms_txt::fence_marker(trimmed) {
                    open = Some((marker, run));
                }
            }
        }
        offset = line_end + 1;
    }
    open.is_some()
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
