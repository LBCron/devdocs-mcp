//! Unit tests for the segmenter: byte-exact slicing, packing, overlap,
//! fence safety, and the header/hash contract.
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
    assert!(boundary_inside_fence(text, close_line + 1));
    assert!(!boundary_inside_fence(text, close_line + 3));
    // A boundary past the last line keeps the running state.
    assert!(!boundary_inside_fence(text, text.len() + 10));
    let unclosed = "before\n```\nbody\n";
    assert!(boundary_inside_fence(unclosed, unclosed.len() + 10));
}
