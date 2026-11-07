//! Unit tests for the segmenter: byte-exact slicing, packing, overlap,
//! fence safety, and the header/hash contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{Segmenter, boundary_inside_fence, contextual_header, fence_intervals};
use crate::symbols::grammar_for;

/// Whether this build carries the grammar for `lang`: without its feature
/// the fence counts unsupported and yields no symbol, so each
/// symbol-carrying test branches on it and pins the degrade behavior in
/// every feature state.
fn grammar_enabled(lang: &str) -> bool {
    grammar_for(lang).is_some()
}

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

/// A midsize split flushes the blocks packed ahead of it: the prose chunk
/// closes before the split pieces begin, and the first split piece rides
/// the block's symbols.
#[test]
fn midsize_split_flushes_the_packed_current() {
    // 45 words ≈ 62 heuristic tokens: inside the (target, 2× target) band
    // of the 40-token segmenter, with a small paragraph packed ahead.
    let words = (0..45).map(|i| format!("tok{i}")).collect::<Vec<_>>();
    let page = format!("## Mid\n\nsmall prose\n\n{}\n", words.join(" "));
    let chunks = small().segment("acme", "1.0", &page);
    assert!(
        chunks.len() >= 3,
        "expected the prose chunk plus split pieces: {chunks:?}"
    );
    assert!(
        chunks[0].body.contains("small prose"),
        "the first chunk is the flushed prose, got: {:?}",
        chunks[0].body
    );
    assert!(
        !chunks[1].body.contains("small prose"),
        "the split pieces do not re-serve the flushed prose"
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
        // Plain content; é keeps byte offsets and char counts apart, so
        // the probe equivalence covers multibyte content too.
        4 => "[a-zé ]{1,12}",
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
    let merge = |spans: Vec<std::ops::Range<usize>>| {
        let candidates: Vec<super::Candidate> = spans
            .into_iter()
            .map(|span| super::Candidate {
                span,
                symbols: Vec::new(),
            })
            .collect();
        super::merge_until_fence_safe(candidates, text)
    };
    // A single span returns untouched.
    let spans = |candidates: Vec<super::Candidate>| -> Vec<std::ops::Range<usize>> {
        candidates.into_iter().map(|c| c.span).collect()
    };
    let single: Vec<std::ops::Range<usize>> = std::iter::once(0..2).collect();
    assert_eq!(spans(merge(single.clone())), single);
    // Boundaries 4 and 8 sit inside the fence: the chain merges into one
    // span, one merge per boundary.
    assert_eq!(spans(merge(vec![0..4, 4..8, 8..12])), vec![0..12]);
    // Boundary 2 sits outside (before the fence), boundary 4 inside: the
    // last span absorbs into its predecessor; its own end, also inside
    // the fence, merges nothing once it is the final span.
    assert_eq!(spans(merge(vec![0..2, 2..4, 8..12])), vec![0..2, 2..12]);
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

/// A fence whose language is named but unknown is counted; known languages
/// and unnamed fences are not
/// (docs/implementation/07 §Normalization pipeline, the ingest events).
#[test]
fn unknown_fence_languages_are_counted() {
    // The debug event formats only under a subscriber that enables it.
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(std::io::sink)
        .finish();
    let (chunks, stats) = tracing::subscriber::with_default(subscriber, || {
        Segmenter::new().unwrap().segment_with_stats(
            "lib",
            "1.0",
            "# T\n\n```cobol\nMOVE A TO B.\n```\n\nbody\n",
        )
    });
    assert_ne!(chunks, [] as [super::PreparedChunk; 0]);
    assert_eq!(stats.unknown_fence_languages, 1);
    assert_eq!(stats.skipped_blocks, 0);

    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```rust\nlet x = 1;\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);

    // An unnamed fence names no language, where it is not counted.
    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```\nplain\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);

    // The `language-*` prefix convention is stripped before the lookup.
    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```language-python\nx\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);
}

/// A rust fence carries its extracted symbols on the produced chunk, in
/// block order; prose around the fence joins the same chunk.
#[test]
fn rust_fence_symbols_land_on_the_chunk() {
    let (chunks, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\nprose\n\n```rust\nfn parse_config() {}\nstruct Command;\n```\n\nmore prose\n",
    );
    if !grammar_enabled("rust") {
        assert_eq!(
            stats.unsupported_fence_languages, 1,
            "without the grammar the fence counts unsupported"
        );
        assert!(
            chunks.iter().all(|chunk| chunk.symbols.is_empty()),
            "without the grammar no symbol is extracted"
        );
        return;
    }
    assert_eq!(stats.unsupported_fence_languages, 0);
    let chunk = chunks
        .iter()
        .find(|chunk| !chunk.symbols.is_empty())
        .expect("a chunk carries the fence's symbols");
    let names: Vec<&str> = chunk.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["parse_config", "Command"]);
    assert_eq!(chunk.symbols[0].kind, "function");
    assert_eq!(chunk.symbols[1].kind, "struct");
    assert_eq!(chunk.symbols[0].line, 1, "line is 1-based in the fence");
}

/// A fence naming an unknown language (`wasm`) is counted and the chunk is
/// still produced; a symbol-capable language without its grammar
/// (TypeScript, no grammar by design) counts as unsupported. Unnamed
/// fences count as neither.
#[test]
fn unsupported_and_unknown_fences_are_counted_but_never_fail() {
    let (chunks, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```wasm\n(module)\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 1);
    assert_eq!(stats.unsupported_fence_languages, 0);
    assert!(!chunks.is_empty(), "the source still produces chunks");

    let (chunks, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```ts\nconst x = 1;\n```\n\n```bash\necho hi\n```\n\nbody\n",
    );
    assert_eq!(
        stats.unsupported_fence_languages, 1,
        "ts counts, bash does not"
    );
    assert_eq!(stats.unknown_fence_languages, 0);
    assert!(!chunks.is_empty(), "the source still produces chunks");

    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```\nplain\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);
    assert_eq!(stats.unsupported_fence_languages, 0);
}

/// A fenced code block nested in a list item extracts its symbols onto the
/// chunk its container block packs into; the extraction sees the fence at
/// any container depth (docs/implementation/07 §Symbol extraction).
#[test]
fn nested_fences_extract_symbols() {
    let page = "# Guide\n\n- step one:\n\n  ```rust\n  fn nested_in_list() {}\n  ```\n";
    let (chunks, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    let names: Vec<&str> = chunks
        .iter()
        .flat_map(|chunk| chunk.symbols.iter().map(|s| s.name.as_str()))
        .collect();
    if grammar_enabled("rust") {
        assert_eq!(names, vec!["nested_in_list"]);
    } else {
        assert_eq!(
            stats.unsupported_fence_languages, 1,
            "without the grammar the fence counts unsupported"
        );
        assert!(names.is_empty(), "without the grammar no symbol emerges");
    }

    // A fence inside a blockquote.
    let page = "# Guide\n\n> run this:\n>\n> ```go\n> func Nested() {}\n> ```\n";
    let (chunks, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    let names: Vec<&str> = chunks
        .iter()
        .flat_map(|chunk| chunk.symbols.iter().map(|s| s.name.as_str()))
        .collect();
    if grammar_enabled("go") {
        assert_eq!(names, vec!["Nested"]);
    } else {
        assert_eq!(
            stats.unsupported_fence_languages, 1,
            "without the grammar the fence counts unsupported"
        );
        assert!(names.is_empty(), "without the grammar no symbol emerges");
    }

    // A fence inside a list nested in a list.
    let page =
        "# Guide\n\n- outer:\n\n  - inner:\n\n    ```python\n    def deep(): pass\n    ```\n";
    let (chunks, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    let names: Vec<&str> = chunks
        .iter()
        .flat_map(|chunk| chunk.symbols.iter().map(|s| s.name.as_str()))
        .collect();
    if grammar_enabled("python") {
        assert_eq!(names, vec!["deep"]);
    } else {
        assert_eq!(
            stats.unsupported_fence_languages, 1,
            "without the grammar the fence counts unsupported"
        );
        assert!(names.is_empty(), "without the grammar no symbol emerges");
    }
}

/// The fence counters cover nested fences too: an unknown nested language
/// counts, and an unnamed nested fence stays uncounted
/// (docs/implementation/07 §Symbol extraction, the counters).
#[test]
fn nested_fence_counters_match_top_level_ones() {
    // Unknown nested language.
    let page = "# T\n\n- item:\n\n  ```wasm,foo\n  (module)\n  ```\n";
    let (chunks, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    assert_eq!(stats.unknown_fence_languages, 1);
    assert_eq!(stats.unsupported_fence_languages, 0);
    assert!(chunks.iter().all(|chunk| chunk.symbols.is_empty()));

    // Unsupported nested language (TypeScript has no grammar by design).
    let page = "# T\n\n> ```ts\n> const x = 1;\n> ```\n";
    let (_, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    assert_eq!(stats.unsupported_fence_languages, 1);

    // An unnamed nested fence names no language, so it counts in neither.
    let page = "# T\n\n- item:\n\n  ```\n  plain\n  ```\n";
    let (_, stats) = Segmenter::new()
        .unwrap()
        .segment_with_stats("lib", "1.0", page);
    assert_eq!(stats.unknown_fence_languages, 0);
    assert_eq!(stats.unsupported_fence_languages, 0);
}

/// An oversized list that splits routes each nested-fence symbol group to
/// the piece containing its fence, not to the first piece: a rust fence in
/// a later item carries its symbol on the chunk holding that fence.
#[test]
fn split_container_routes_nested_symbols_to_their_piece() {
    // Prose items push the list into the (target, 2× target) band of the
    // 40-token segmenter (~62 heuristic tokens); the fence sits in the
    // final item, past the first split piece.
    let mut items: Vec<String> = (1..=6)
        .map(|n| format!("- prose item number {n} with words\n"))
        .collect();
    items.push("- last:\n\n  ```rust\n  fn deep_item_fn() {}\n  ```\n".to_owned());
    let page = format!("## List\n\n{}", items.join("\n"));
    let chunks = small().segment("lib", "1.0", &page);
    let list_chunks: Vec<_> = chunks
        .iter()
        .filter(|chunk| chunk.section_path == vec!["List"])
        .collect();
    assert!(
        list_chunks.len() >= 2,
        "expected the list to split: {list_chunks:?}"
    );
    if !grammar_enabled("rust") {
        assert!(
            list_chunks.iter().all(|chunk| chunk.symbols.is_empty()),
            "without the grammar no symbol is extracted: {list_chunks:?}"
        );
        return;
    }
    let carrying: Vec<_> = list_chunks
        .iter()
        .filter(|chunk| chunk.symbols.iter().any(|s| s.name == "deep_item_fn"))
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "the symbol lands on exactly one piece: {list_chunks:?}"
    );
    assert!(
        carrying[0].body.contains("fn deep_item_fn() {}"),
        "the carrying piece holds the fence: {:?}",
        carrying[0].body
    );
    assert!(
        !list_chunks[0].body.contains("fn deep_item_fn() {}"),
        "the first piece is prose only: {:?}",
        list_chunks[0].body
    );
}

/// The fence language is the first token up to whitespace or a comma, so
/// attribute-bearing info strings name their language
/// (docs/implementation/07 §Symbol extraction).
#[test]
fn fence_info_attributes_do_not_hide_the_language() {
    // A comma-joined attribute (`rust,ignore` is one whitespace token).
    let (chunks, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```rust,ignore\nfn tagged() {}\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);
    if grammar_enabled("rust") {
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.symbols.iter().any(|s| s.name == "tagged")),
            "the comma attribute keeps the language symbol-capable"
        );
    } else {
        assert!(
            chunks.iter().all(|chunk| chunk.symbols.is_empty()),
            "without the grammar no symbol is extracted"
        );
    }

    // A colon-joined attribute after a comma.
    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```python,name: x\nvalue = 1\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);

    // A genuinely unknown comma token still counts as unknown.
    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```wasm,foo\n(module)\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 1);

    // The whitespace-separated attribute form keeps working.
    let (_, stats) = Segmenter::new().unwrap().segment_with_stats(
        "lib",
        "1.0",
        "# T\n\n```rust ignore\nfn spaced() {}\n```\n\nbody\n",
    );
    assert_eq!(stats.unknown_fence_languages, 0);
}
