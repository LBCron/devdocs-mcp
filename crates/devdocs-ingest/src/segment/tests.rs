//! Unit tests for the segmenter: byte-exact slicing, packing, overlap,
//! fence safety, and the header/hash contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{Segmenter, boundary_inside_fence, contextual_header, fence_intervals};

/// The fence probe over a text's precomputed intervals.
fn probe(text: &str, at: usize) -> bool {
    boundary_inside_fence(&fence_intervals(text), at)
}

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
    assert!(!probe(text, 0));
    assert!(!probe(text, 6));
    // Inside the fence content: inside.
    let open_line = text.find("{\"a\": 1}").unwrap();
    assert!(probe(text, open_line));
    // Just past the closing fence line: outside.
    let close_end = text.find("```\nafter").unwrap() + 4;
    assert!(!probe(text, close_end));
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
    assert!(probe(text, json_line + 1));
    // Past the closing ~~~: outside.
    let after = text.find("b\n").unwrap();
    assert!(!probe(text, after));
}

/// A line carrying the marker run plus trailing text never closes a
/// fence, in the probe just as in the index scanner.
#[test]
fn fence_probe_close_needs_a_marker_only_line() {
    let text = "a\n```\n```example\nb\n```\nc\n";
    let inside = text.find("b\n").unwrap();
    assert!(probe(text, inside));
    let after = text.find("c\n").unwrap();
    assert!(!probe(text, after));
}

use proptest::strategy::Strategy as _;

/// One generated line of a fence-focused page: plain content, or a marker
/// line that varies the marker character, run length, indent, and
/// trailing text so every open/close rule of the probe gets exercised.
fn fence_line() -> impl proptest::strategy::Strategy<Value = String> {
    proptest::prop_oneof![
        // Plain content.
        4 => "[a-z ]{1,12}",
        // A marker line: indent, marker run, and trailing text vary.
        2 => " {0,3}[`~]{3,5} ?[a-z]{0,4}",
        // Trailing spaces behind a marker-only line.
        1 => "[`~]{3,4} {1,3}",
    ]
}

/// The per-boundary rescan the interval probe replaced (issue #64), kept
/// as the equivalence reference: the same verdicts the merge loop used
/// before the one-pass intervals landed.
fn rescan_probe(text: &str, at: usize) -> bool {
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
                let indent = line.len() - trimmed.len();
                return at >= line_start + indent;
            };
            if crate::llms_txt::fence_closes(open, line.trim()) {
                let indent = line.len() - trimmed.len();
                return at < line_start + indent + super::marker_run(marker, trimmed);
            }
            return true;
        }
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

/// One generated fence-focused page: the generated lines joined by newlines.
fn fence_page() -> impl proptest::strategy::Strategy<Value = String> {
    proptest::collection::vec(fence_line(), 1..24).prop_map(|lines| lines.join("\n"))
}

// The one-pass interval probe (#64) agrees with the per-boundary rescan
// it replaced, byte for byte, on every offset of a generated page.
proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]
    #[test]
    fn interval_probe_matches_the_rescan_probe(page in fence_page()) {
        let fences = fence_intervals(&page);
        for at in 0..=page.len() {
            proptest::prop_assert_eq!(
                boundary_inside_fence(&fences, at),
                rescan_probe(&page, at),
                "offset {} disagrees on {:?}",
                at,
                page
            );
        }
    }
}

/// The merge pass joins spans whose boundaries fall inside a fence in one
/// left-to-right sweep, and leaves inputs with fewer than two spans —
/// which skip the fence scan — byte-identical.
#[test]
fn merge_pass_merges_fence_crossing_boundaries_in_order() {
    // The unclosed fence at byte 3 covers every offset from 3 on.
    let text = "a\n```\ncode\n";
    let merge = |spans: Vec<std::ops::Range<usize>>| super::merge_until_fence_safe(spans, text);
    // A single span returns untouched.
    let single: Vec<std::ops::Range<usize>> = std::iter::once(0..2).collect();
    assert_eq!(merge(single.clone()), single);
    // Boundaries 4 and 8 sit inside the fence: the chain merges into one
    // span, one merge per boundary.
    assert_eq!(merge(vec![0..4, 4..8, 8..12]), vec![0..12]);
    // Boundary 2 sits outside (before the fence), boundary 4 inside: the
    // last span absorbs into its predecessor; its own end, also inside
    // the fence, merges nothing once it is the final span.
    assert_eq!(merge(vec![0..2, 2..4, 8..12]), vec![0..2, 2..12]);
}

/// The segmenter debug shape names its limits.
#[test]
fn segmenter_debug_names_the_limits() {
    let text = format!("{:?}", small());
    assert!(text.contains("target_tokens"), "{text}");
    assert!(text.contains("overlap_tokens"), "{text}");
}

/// Overlap at or above the target is a construction error.
#[test]
fn overlap_at_or_above_the_target_refuses() {
    assert!(Segmenter::with_limits(40, 40).is_err());
    assert!(Segmenter::with_limits(40, 50).is_err());
}

/// Content before the first heading forms a heading-less section: the
/// chunk carries the bare `[library version]` header.
#[test]
fn heading_less_content_gets_a_bare_header() {
    let chunks = Segmenter::with_limits(40, 8).unwrap().segment(
        "lib",
        "1.0",
        "plain words before any heading\n",
    );
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].section_path, Vec::<String>::new());
    assert_eq!(chunks[0].header, "[lib 1.0]");
    assert!(chunks[0].body.contains("plain words"));
}

/// Inline code inside a heading joins the breadcrumb.
#[test]
fn inline_code_in_a_heading_joins_the_breadcrumb() {
    let page = "# Use `acme` here\n\nbody text\n";
    let chunks = Segmenter::new().unwrap().segment("lib", "1.0", page);
    assert_eq!(
        chunks[0].section_path,
        vec![String::from("Use acme here")],
        "{:?}",
        chunks[0].section_path
    );
}

/// A boundary that lands mid-line on a fence marker sits inside the
/// open state until past the marker characters.
#[test]
fn fence_probe_handles_marker_lines_and_unclosed_fences() {
    let text = "before\n```\nbody\n```\nafter\n";
    // Inside the closing marker line: within the three characters is
    // inside, past them is outside.
    let close_line = text.find("```\nafter").unwrap();
    assert!(probe(text, close_line + 1));
    assert!(!probe(text, close_line + 3));
    // A boundary past the last line keeps the running state.
    assert!(!probe(text, text.len() + 10));
    let unclosed = "before\n```\nbody\n";
    assert!(probe(unclosed, unclosed.len() + 10));
}
