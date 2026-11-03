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
            if let NodeValue::Heading(NodeHeading { level, .. }) = &node.data().value {
                let level = *level;
                let title = inline_text(node);
                // Pop the stack to the heading's level, then push.
                while breadcrumb.last().is_some_and(|(l, _)| *l >= level) {
                    breadcrumb.pop();
                }
                breadcrumb.push((level, title.clone()));
                sections.push(Section {
                    text: page_body,
                    breadcrumb: breadcrumb.iter().map(|(_, t)| t.clone()).collect(),
                    blocks: Vec::new(),
                });
            } else {
                let fenced =
                    matches!(&node.data().value, NodeValue::CodeBlock(boxed) if boxed.fenced);
                let (start, end) = block_range(node, &offset);
                let end = end.min(page_body.len());
                if start >= end {
                    continue;
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
            let Some((marker, run)) = crate::llms_txt::fence_marker(trimmed) else {
                return open.is_some();
            };
            let Some((open_char, open_run)) = open else {
                // Outside, on a fence line: the bytes before the marker sit
                // outside, from the marker on is inside the fence.
                let indent = line.len() - trimmed.len();
                let marker_start = line_start + indent;
                return at >= marker_start;
            };
            if marker == open_char && run >= open_run && trimmed.chars().all(|c| c == marker) {
                // A closing fence: past the marker run is outside.
                let indent = line.len() - trimmed.len();
                let marker_end = line_start + indent + run;
                return at < marker_end;
            }
            // Any other line inside an open fence is fence content.
            return true;
        }
        // Cross the line: apply the same open/close rule the scanner uses.
        let trimmed = line.trim();
        match open {
            Some((open_char, open_run)) => {
                if let Some((marker, run)) = crate::llms_txt::fence_marker(trimmed)
                    && marker == open_char
                    && run >= open_run
                    && trimmed.chars().all(|c| c == marker)
                {
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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Segmenter, boundary_inside_fence, contextual_header};

    /// A segmenter with a small target so packing and splitting exercise on
    /// short inputs.
    fn small() -> Segmenter {
        Segmenter::with_limits(40, 8).unwrap()
    }

    /// The contextual header joins the path with the documented separator
    /// and omits it when the path is empty.
    #[test]
    fn contextual_header_shapes() {
        assert_eq!(contextual_header("acme", "2.1.0", &[]), "[acme 2.1.0]");
        let path = vec![String::from("Guides"), String::from("Routing")];
        assert_eq!(
            contextual_header("acme", "2.1.0", &path),
            "[acme 2.1.0 › Guides › Routing]"
        );
    }

    /// Comrak source positions slice paragraph text byte-exactly: the
    /// extracted body equals the original bytes.
    #[test]
    fn sourcepos_slices_are_byte_exact() {
        let page = "# Title\n\nHello world\n\nSecond para\n";
        // Target 3 tokens: "Hello world" fills one chunk exactly, so the
        // second paragraph starts a fresh one.
        let chunks = Segmenter::with_limits(3, 1)
            .unwrap()
            .segment("acme", "1.0", page);
        assert_eq!(chunks.len(), 2, "two paragraphs, two chunks");
        assert_eq!(chunks[0].body, "Hello world");
        assert_eq!(chunks[1].body, "Second para");
        assert_eq!(chunks[0].section_path, vec!["Title"]);
        assert!(
            chunks[0]
                .text
                .starts_with("[acme 1.0 › Title]\n\nHello world")
        );
    }

    /// Breadcrumbs nest by heading level and pop on sibling and parent
    /// headings.
    #[test]
    fn breadcrumbs_nest_and_pop() {
        let page = concat!(
            "# Root\n\nintro\n\n",
            "## First\n\nfirst body\n\n",
            "### Deep\n\ndeep body\n\n",
            "## Second\n\nsecond body\n\n",
            "# Top\n\ntop body\n"
        );
        let chunks = Segmenter::new().unwrap().segment("acme", "1.0", page);
        let paths: Vec<Vec<String>> = chunks.iter().map(|c| c.section_path.clone()).collect();
        assert_eq!(paths[0], vec!["Root"]);
        assert_eq!(paths[1], vec!["Root", "First"]);
        assert_eq!(paths[2], vec!["Root", "First", "Deep"]);
        assert_eq!(paths[3], vec!["Root", "Second"]);
        assert_eq!(paths[4], vec!["Top"]);
    }

    /// Consecutive blocks pack into one chunk while the target holds; the
    /// next block that overflows the target closes the chunk.
    #[test]
    fn blocks_pack_to_the_target() {
        let page = "## S\n\none two three\n\nfour five six\n\nseven eight nine ten\n";
        // Target 9 tokens: paragraphs one and two pack together, the third
        // overflows the chunk.
        let chunks = Segmenter::with_limits(9, 1)
            .unwrap()
            .segment("acme", "1.0", page);
        assert_eq!(chunks.len(), 2, "packed to two chunks: {chunks:?}");
        assert!(chunks[0].body.contains("one two three"));
        assert!(chunks[0].body.contains("four five six"));
        assert!(chunks[1].body.contains("seven eight nine ten"));
    }

    /// A block over twice the target becomes its own chunk and stays whole.
    #[test]
    fn oversized_block_stays_whole() {
        let big = format!("## Big\n\n{}\n", "word ".repeat(200));
        let chunks = small().segment("acme", "1.0", &big);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].token_count > 80);
        assert!(chunks[0].body.contains("word word word word"));
        assert_eq!(chunks[0].body.matches("word").count(), 200);
    }

    /// A block between the target and twice the target splits with the
    /// overlap: adjacent pieces share text.
    #[test]
    fn midsize_block_splits_with_overlap() {
        // 45 words of 6 chars ≈ 68 heuristic tokens: inside the (target,
        // 2× target) band of the 40-token segmenter.
        let words = (0..45).map(|i| format!("tok{i}")).collect::<Vec<_>>();
        let page = format!("## Mid\n\n{}\n", words.join(" "));
        let chunks = small().segment("acme", "1.0", &page);
        assert!(chunks.len() >= 2, "expected a split: {chunks:?}");
        // Overlap: the second piece's byte range starts before the first's
        // ends, and the shared text is identical in both.
        assert!(
            chunks[1].page_range.start < chunks[0].page_range.end,
            "no byte overlap between split pieces"
        );
        let shared = &chunks[0].body[chunks[1].page_range.start - chunks[0].page_range.start..];
        assert!(
            chunks[1].body.starts_with(shared),
            "the second piece does not carry the overlap text"
        );
    }

    /// A fenced code block never splits, even when it exceeds the target.
    #[test]
    fn fence_never_splits() {
        let fence = format!("```\n{}\n```\n", "row\n".repeat(40));
        let page = format!("## Table\n\n{fence}");
        let chunks = small().segment("acme", "1.0", &page);
        assert_eq!(chunks.len(), 1, "the fence stays one chunk: {chunks:?}");
        assert_eq!(chunks[0].body.matches("row").count(), 40);
    }

    /// Every fence marker line lands inside one chunk, so no chunk starts
    /// or ends inside a fence.
    #[test]
    fn chunk_boundaries_avoid_fences() {
        let page = concat!(
            "## A\n\ntext before\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\ntext after\n\n",
            "## B\n\n~~~\nplain fence\n~~~\n\nmore text\n"
        );
        let chunks = small().segment("acme", "1.0", page);
        for chunk in &chunks {
            let markers = chunk
                .body
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    trimmed.starts_with("```") || trimmed.starts_with("~~~")
                })
                .count();
            assert_eq!(markers % 2, 0, "unbalanced fences in {:?}", chunk.body);
        }
    }

    /// The fence probe works on page-absolute offsets: a fence in an
    /// earlier section must not flag a later section's boundary. The first
    /// paragraph of section B is 29 bytes long, so a section-relative probe
    /// for its end offset reads inside the earlier fence region.
    #[test]
    fn fence_probe_uses_page_offsets() {
        let page = format!(
            concat!(
                "# Intro\n\nhi\n\n```json\n{{\"aaaaaaaaaaaaaaaaaaaa\": 1}}\n```\n\n# B\n\n",
                "word word word word word word\n\n",
                "{para2}\n"
            ),
            para2 = "word ".repeat(30).trim_end()
        );
        let chunks = small().segment("acme", "1.0", &page);
        // The two paragraphs of section B cannot share one chunk (they
        // overflow the target); a false fence hit on the boundary would
        // merge them into one and log a bogus warning.
        let b_chunks: Vec<_> = chunks
            .iter()
            .filter(|chunk| chunk.section_path == vec!["B"])
            .collect();
        assert_eq!(b_chunks.len(), 2, "B must yield two chunks: {chunks:?}");
    }

    /// The packing budget counts the bytes between blocks: separators join
    /// the packed body, so a full chunk cannot overshoot the token target.
    #[test]
    fn packing_counts_separator_bytes() {
        // 80 four-char words: each block costs one token, and each two-byte
        // separator joins the packed body.
        let page = format!("## S\n\n{}\n", "wwww\n\n".repeat(80));
        let chunks = Segmenter::with_limits(40, 8)
            .unwrap()
            .segment("acme", "1.0", &page);
        assert!(chunks.len() > 1, "the page must split: {chunks:?}");
        let sizer = super::build_sizer().unwrap();
        for chunk in &chunks {
            let tokens = sizer.size(&chunk.body);
            assert!(
                tokens <= 40,
                "packed chunk overshot the target: {tokens} tokens for {:?}",
                chunk.body
            );
        }
    }

    /// The fence-boundary probe recognizes open and closed states.
    #[test]
    fn fence_boundary_probe() {
        let text = "before\n```json\n{\"a\": 1}\n```\nafter\n";
        // "before" and its end: outside.
        assert!(!boundary_inside_fence(text, 0));
        assert!(!boundary_inside_fence(text, 6));
        // Inside the fence content: inside.
        let open_line = text.find("{\"a\": 1}").unwrap();
        assert!(boundary_inside_fence(text, open_line));
        // Just past the closing fence line: outside.
        let close_end = text.find("```\nafter").unwrap() + 4;
        assert!(!boundary_inside_fence(text, close_end));
    }

    /// The content hash covers the body without the header, and the stored
    /// text carries the header.
    #[test]
    fn hash_and_header_contract() {
        let page = "# T\n\nbody text\n";
        let chunks = Segmenter::new().unwrap().segment("lib", "9.9", page);
        let chunk = &chunks[0];
        assert_eq!(chunk.text, format!("{}\n\n{}", chunk.header, chunk.body));
        assert_eq!(chunk.content_hash, devdocs_store::content_hash(&chunk.body));
    }

    /// The probe matches the closing marker to the opening marker: a
    /// backtick line inside a tilde fence is fence content, and a boundary
    /// after the real closing fence is outside.
    #[test]
    fn fence_probe_matches_the_opening_marker() {
        let text = "a\n~~~\n```json\n{}\n~~~\nb\n";
        // The backtick line inside the tilde fence does not close it.
        let json_line = text.find("```json").unwrap();
        assert!(boundary_inside_fence(text, json_line + 1));
        // Past the closing ~~~: outside.
        let after = text.find("b\n").unwrap();
        assert!(!boundary_inside_fence(text, after));
    }

    /// A line carrying the marker run plus trailing text never closes a
    /// fence, in the probe just as in the index scanner.
    #[test]
    fn fence_probe_close_needs_a_marker_only_line() {
        let text = "a\n```\n```example\nb\n```\nc\n";
        let inside = text.find("b\n").unwrap();
        assert!(boundary_inside_fence(text, inside));
        let after = text.find("c\n").unwrap();
        assert!(!boundary_inside_fence(text, after));
    }
}
