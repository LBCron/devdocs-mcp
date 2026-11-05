//! Property tests for segmentation (docs/implementation/04 §Property
//! tests): chunk spans cover the document without gaps, no chunk starts or
//! ends inside a fenced code block, every fence lands whole inside one
//! chunk, and the header/hash contract holds for every chunk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseResult};

use devdocs_ingest::PreparedChunk;
use devdocs_ingest::chrome::{PageDocument, strip_chrome};
use devdocs_ingest::segment::Segmenter;

/// `n` synthetic words with the given prefix.
fn words(len: usize, prefix: &str) -> Vec<String> {
    (0..len).map(|i| format!("{prefix}{i}")).collect()
}

/// One generated block of a page.
#[derive(Debug, Clone)]
enum Block {
    /// A paragraph of random words.
    Paragraph(Vec<String>),
    /// A fenced code block with `rows` content lines.
    Fence(u8),
}

/// One generated section: a heading plus its blocks.
#[derive(Debug, Clone)]
struct GenSection {
    level: u8,
    title: String,
    blocks: Vec<Block>,
}

/// The page generator: one to four sections, each with one to four blocks;
/// paragraph lengths and fence sizes vary around the 40-token target.
fn page_strategy() -> impl Strategy<Value = String> {
    let block = prop_oneof![
        (1usize..30).prop_map(|len| Block::Paragraph(words(len, "w"))),
        (1usize..12).prop_map(|len| Block::Paragraph(words(len, "p"))),
        (1u8..12).prop_map(Block::Fence),
    ];
    let section = (
        "[A-Z][a-z]{2,8}",
        1u8..=3,
        prop::collection::vec(block, 1..=4),
    )
        .prop_map(|(title, level, blocks)| GenSection {
            level,
            title,
            blocks,
        });
    prop::collection::vec(section, 1..=4).prop_map(|sections| {
        let mut page = String::new();
        let mut fence_id = 0usize;
        for (index, section) in sections.iter().enumerate() {
            page.push_str(&"#".repeat(section.level as usize));
            page.push(' ');
            page.push_str(&section.title);
            page.push_str(&index.to_string());
            page.push_str("\n\n");
            for block in &section.blocks {
                match block {
                    Block::Paragraph(list) => {
                        page.push_str(&list.join(" "));
                        page.push_str("\n\n");
                    }
                    Block::Fence(rows) => {
                        fence_id += 1;
                        page.push_str("```\n");
                        for row in 0..*rows {
                            let _ = writeln!(page, "row-{fence_id}-{row}");
                        }
                        page.push_str("```\n\n");
                    }
                }
            }
        }
        page
    })
}

/// Collects every fenced block's full text from the generated page.
fn fence_texts(page: &str) -> Vec<String> {
    let mut fences = Vec::new();
    let mut current: Option<String> = None;
    for line in page.lines() {
        if line.starts_with("```") {
            match current.take() {
                Some(open) => fences.push(open),
                None => current = Some(String::new()),
            }
        } else if let Some(open) = current.as_mut() {
            open.push_str(line);
            open.push('\n');
        }
    }
    fences
}

/// Segment with a small target so packing, splitting, and oversized blocks
/// all exercise.
fn segment(page: &str) -> Vec<PreparedChunk> {
    Segmenter::with_limits(40, 8)
        .unwrap()
        .segment("lib", "1.0", page)
}

/// The no-lost-content checks: nothing before the first chunk, only
/// whitespace inside a section gap, only heading lines across a section
/// boundary, and the tail covered by the last chunk.
fn assert_no_content_lost(page: &str, chunks: &[PreparedChunk]) -> TestCaseResult {
    // Nothing before the first chunk: the prefix may hold only blank lines
    // and heading lines — a dropped first block of the first section fails
    // here, where the gap checks cannot see it.
    if let Some(first) = chunks.first() {
        let prefix = &page[..first.page_range.start];
        prop_assert!(
            prefix
                .lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
            "content before the first chunk: {prefix:?}"
        );
    }

    // No content lost inside a section: consecutive chunks with the same
    // breadcrumb either overlap or leave only inter-block whitespace
    // between them. Across a section boundary the only allowed hole is the
    // next heading line plus whitespace — so a dropped first block of a
    // section fails here too.
    for pair in chunks.windows(2) {
        let (prev, next) = (&pair[0], &pair[1]);
        if next.page_range.start > prev.page_range.end {
            let gap = &page[prev.page_range.end..next.page_range.start];
            if prev.section_path == next.section_path {
                prop_assert!(
                    gap.chars().all(char::is_whitespace),
                    "non-whitespace gap {gap:?} inside a section: page {page:?}"
                );
            } else {
                prop_assert!(
                    gap.lines()
                        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
                    "cross-section gap carries non-heading content {gap:?}: page {page:?}"
                );
            }
        }
    }
    if let Some(last) = chunks.last() {
        // The last chunk reaches the end of the page content: the tail it
        // leaves may hold only whitespace and heading lines — a section
        // boundary is a legal hole, including at the very end, and a
        // section whose only blocks dropped as chrome leaves exactly that.
        let tail = &page[last.page_range.end..];
        prop_assert!(
            tail.lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
            "page tail uncovered: {tail:?} after {}",
            last.page_range.end
        );
    }
    Ok(())
}

#[test]
fn generated_pages_satisfy_the_segmentation_properties() {
    proptest!(Config::with_cases(256), |(page in page_strategy())| {
        let chunks = segment(&page);
        assert_no_content_lost(&page, &chunks)?;

        // 2. No chunk starts or ends inside a fence: fence markers are
        // balanced within every chunk.
        for chunk in &chunks {
            let markers = chunk
                .body
                .lines()
                .filter(|line| line.starts_with("```"))
                .count();
            prop_assert_eq!(markers % 2, 0, "unbalanced fences in {:?}", chunk.body);
        }

        // 3. Every fence lands whole inside exactly one chunk.
        for fence in fence_texts(&page) {
            let holders = chunks
                .iter()
                .filter(|chunk| chunk.body.contains(&fence))
                .count();
            prop_assert_eq!(holders, 1, "fence {:?} split or lost", fence);
        }

        // 4. Header and hash contract per chunk.
        for chunk in &chunks {
            let expected = format!("{}\n\n{}", chunk.header, chunk.body);
            prop_assert_eq!(&chunk.text, &expected);
            prop_assert_eq!(
                chunk.content_hash,
                devdocs_store::content_hash(&chunk.body)
            );
            prop_assert_eq!(&page[chunk.page_range.clone()], &chunk.body);
        }
    });
}

/// The corpus generator: three to four distinct content pages, each
/// wrapped in shared chrome — a leading navigation list and a trailing
/// footer paragraph identical across the corpus.
fn chrome_corpus_strategy() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(page_strategy(), 3..=4).prop_map(|pages| {
        pages
            .into_iter()
            .map(|body| format!("- [Nav](/nav)\n- [Docs](/docs)\n\n{body}\n\n© Shared footer.\n"))
            .collect()
    })
}

/// The chrome strip plus segmentation property over a whole corpus
/// (docs/implementation/04 §Property tests): after the strip, chunk
/// bodies are byte-exact slices of the stripped body and the non-overlap
/// spans reassemble the stripped body minus dropped chrome — the only
/// gaps are whitespace and heading lines, and the tail is covered.
#[test]
fn stripped_corpora_reassemble_from_their_chunks() {
    proptest!(Config::with_cases(64), |(pages in chrome_corpus_strategy())| {
        let mut docs: Vec<PageDocument> = pages
            .iter()
            .enumerate()
            .map(|(index, body)| PageDocument {
                path: format!("https://x/p{index}.md"),
                body: body.clone(),
            })
            .collect();
        let report = strip_chrome(&mut docs);
        prop_assert!(
            report.pattern_drops > 0 && report.frequency_drops > 0,
            "the generated chrome never dropped: {report:?}"
        );

        for doc in &docs {
            let chunks = segment(&doc.body);
            assert_no_content_lost(&doc.body, &chunks)?;
            for chunk in &chunks {
                prop_assert_eq!(
                    &doc.body[chunk.page_range.clone()],
                    &chunk.body,
                    "chunk bodies are byte-exact slices of the stripped body"
                );
            }
        }
    });
}
