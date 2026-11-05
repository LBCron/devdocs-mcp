//! The markdown chrome strip (docs/implementation/07 §Normalization
//! pipeline stage 2).
//!
//! The strip runs over the whole source corpus after all pages are
//! buffered, before segmentation: pattern rules drop leading navigation,
//! "edit this page" links, and cookie banners; the frequency rule drops a
//! block identical in more than 60% of the corpus's pages. A dropped block
//! collapses its own bytes and the whitespace run after it, so the bytes
//! between kept blocks survive byte-exactly and the stripped body is the
//! page body minus the dropped chrome.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use comrak::nodes::{NodeHeading, NodeValue};
use comrak::{Arena, Options, parse_document};

/// One buffered document of the source: the page path (its URL) and the
/// markdown body. The chrome strip and the dedup pass work over these.
#[derive(Debug, Clone)]
pub struct PageDocument {
    /// The document path: the URL the body came from.
    pub path: String,
    /// The markdown body.
    pub body: String,
}

/// What one chrome-strip pass dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChromeReport {
    /// Blocks dropped by the pattern rules (leading navigation, edit links,
    /// cookie banners).
    pub pattern_drops: usize,
    /// Blocks dropped by the cross-page frequency rule.
    pub frequency_drops: usize,
}

/// Strips chrome from every document of the corpus, in place.
///
/// Pattern rules (per document):
///
/// - Leading navigation: blocks before the first heading that carry only
///   links — a link list or a paragraph of links.
/// - "Edit this page" links: a short paragraph whose text matches the
///   edit-link phrasing.
/// - Cookie banners: a short paragraph mentioning cookies.
///
/// The frequency rule (corpus-wide): a block whose text is identical in
/// more than 60% of the corpus's pages is chrome and drops everywhere it
/// appears. The rule needs at least two pages to compare.
#[must_use]
pub fn strip_chrome(pages: &mut [PageDocument]) -> ChromeReport {
    if pages.is_empty() {
        return ChromeReport::default();
    }
    // Per page: the top-level blocks with their byte ranges. Classification
    // comes from the parsed node; both rules match on the body slice.
    let page_blocks: Vec<Vec<PageBlock>> = pages
        .iter()
        .map(|page| top_level_blocks(&page.body))
        .collect();
    // The set of chrome texts is collected before any body is mutated.
    let chrome_texts = frequency_chrome_texts(pages, &page_blocks);

    let mut report = ChromeReport::default();
    for index in 0..pages.len() {
        let dropped = classify_drops(&pages[index].body, &page_blocks[index], &chrome_texts);
        if dropped.is_empty() {
            continue;
        }
        for (_range, kind) in &dropped {
            match kind {
                DropKind::Pattern => report.pattern_drops += 1,
                DropKind::Frequency => report.frequency_drops += 1,
            }
        }
        let ranges: Vec<Range<usize>> = dropped.into_iter().map(|(range, _)| range).collect();
        remove_blocks(&mut pages[index].body, ranges);
    }
    report
}

/// The chrome texts of the corpus under the frequency rule: normalized
/// block texts carried by more than 60% of the pages, headings excluded.
/// With fewer than two pages there is nothing to compare, so nothing is
/// chrome.
fn frequency_chrome_texts(
    pages: &[PageDocument],
    page_blocks: &[Vec<PageBlock>],
) -> HashSet<String> {
    let total_pages = pages.len();
    if total_pages < 2 {
        return HashSet::new();
    }
    // Normalized text → the distinct pages carrying it.
    let mut occurrences: HashMap<&str, HashSet<usize>> = HashMap::new();
    for (page_index, (page, blocks)) in pages.iter().zip(page_blocks).enumerate() {
        for block in blocks {
            // Headings anchor sections; dropping a repeated one corrupts
            // the section paths of later chunks, so they never count here.
            if block.is_heading {
                continue;
            }
            let text = page.body[block.range.clone()].trim();
            if text.is_empty() {
                continue;
            }
            occurrences.entry(text).or_default().insert(page_index);
        }
    }
    occurrences
        .into_iter()
        .filter(|(_text, page_set)| page_set.len() * 10 > total_pages * 6)
        .map(|(text, _)| text.to_owned())
        .collect()
}

/// Classifies the top-level blocks of one page: why each block drops
/// (`DropKind`) and where it sits in the body. The pattern rules match on
/// the body slice; the leading-nav rule stops at the first heading.
fn classify_drops(
    body: &str,
    blocks: &[PageBlock],
    chrome_texts: &HashSet<String>,
) -> Vec<(Range<usize>, DropKind)> {
    let mut dropped: Vec<(Range<usize>, DropKind)> = Vec::new();
    let mut seen_heading = false;
    for block in blocks {
        if block.is_heading {
            seen_heading = true;
        }
        let text = body[block.range.clone()].trim();
        let pattern_chrome = if seen_heading {
            edit_link_text(text) || cookie_banner_text(text, block.link_only)
        } else {
            block.link_only || edit_link_text(text) || cookie_banner_text(text, block.link_only)
        };
        let kind = match (pattern_chrome, chrome_texts.contains(text)) {
            (true, _) => DropKind::Pattern,
            (false, true) => DropKind::Frequency,
            (false, false) => continue,
        };
        dropped.push((block.range.clone(), kind));
    }
    dropped
}

/// Why one block was dropped by the strip.
#[derive(Clone, Copy)]
enum DropKind {
    /// A pattern rule dropped it.
    Pattern,
    /// The cross-page frequency rule dropped it.
    Frequency,
}

/// One top-level block of a page: its byte range, whether it is a heading
/// (the leading-nav rule stops at the first heading), and whether the
/// block carries only links.
struct PageBlock {
    range: Range<usize>,
    is_heading: bool,
    link_only: bool,
}

/// Extracts the top-level blocks of a markdown body: byte ranges (comrak
/// `sourcepos`, the same slicing the segmenter uses), heading flag, and
/// the link-only flag.
fn top_level_blocks(body: &str) -> Vec<PageBlock> {
    let arena = Arena::new();
    let root = parse_document(&arena, body, &Options::default());

    // Byte offset of every line start; (line, col) are 1-based.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            body.bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        )
        .collect();
    let offset = |line: usize, col: usize| -> usize {
        line_starts.get(line - 1).copied().unwrap_or(body.len()) + col.saturating_sub(1)
    };

    let mut blocks = Vec::new();
    for node in root.children() {
        let pos = node.data().sourcepos;
        let start = offset(pos.start.line, pos.start.column);
        let end = (offset(pos.end.line, pos.end.column) + 1).min(body.len());
        if start >= end {
            continue;
        }
        blocks.push(PageBlock {
            range: start..end,
            is_heading: matches!(&node.data().value, NodeValue::Heading(NodeHeading { .. })),
            link_only: is_link_only_block(node),
        });
    }
    blocks
}

/// Whether a top-level block carries only links: a paragraph whose inline
/// children are links and whitespace, or a list whose every item's
/// paragraphs carry only links.
fn is_link_only_block<'a>(node: &'a comrak::nodes::AstNode<'a>) -> bool {
    match &node.data().value {
        NodeValue::Paragraph => paragraph_link_only(node),
        NodeValue::List(_) => {
            // Every item must hold only link-only paragraphs; an empty list
            // carries nothing, so it is not a nav block.
            let mut any = false;
            for item in node.children() {
                any = true;
                let item_ok = item.children().all(|block| {
                    matches!(&block.data().value, NodeValue::Paragraph)
                        && paragraph_link_only(block)
                });
                if !item_ok {
                    return false;
                }
            }
            any
        }
        _ => false,
    }
}

/// Whether a paragraph's inline children are links and whitespace only.
fn paragraph_link_only<'a>(node: &'a comrak::nodes::AstNode<'a>) -> bool {
    let mut has_link = false;
    for child in node.children() {
        match &child.data().value {
            NodeValue::Link(_) => has_link = true,
            NodeValue::Text(text) if text.trim().is_empty() => {}
            NodeValue::SoftBreak | NodeValue::LineBreak => {}
            _ => return false,
        }
    }
    has_link
}

/// Whether a block's text matches the edit-link phrasing: the classic
/// "Edit this page" / "Edit on GitHub" link. Short blocks only, so a long
/// guide paragraph that mentions editing never matches.
fn edit_link_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.len() <= 120
        && (lower.contains("edit this page")
            || lower.contains("edit on github")
            || lower.contains("suggest an edit"))
}

/// Whether a block is a cookie banner: a short block mentioning cookies
/// that also carries a link or the accept/consent call to action. Content
/// that merely discusses cookies — at any length, without the call to
/// action — never matches.
fn cookie_banner_text(text: &str, link_only: bool) -> bool {
    let lower = text.to_ascii_lowercase();
    text.len() <= 200
        && lower.contains("cookie")
        && (link_only || lower.contains("accept") || lower.contains("consent"))
}

/// Removes the dropped blocks from the body: each dropped block's bytes
/// plus the whitespace runs around it collapse, so the bytes between kept
/// blocks survive byte-exactly and no doubled separator is left behind.
fn remove_blocks(body: &mut String, mut dropped: Vec<Range<usize>>) {
    dropped.sort_by_key(|range| range.start);
    let original = std::mem::take(body);
    let bytes = original.as_bytes();
    // The start of the whitespace run that ends at `at`.
    let run_before = |mut at: usize| -> usize {
        while at > 0 && matches!(bytes[at - 1], b' ' | b'\t' | b'\r' | b'\n') {
            at -= 1;
        }
        at
    };
    let mut out = String::with_capacity(original.len());
    let mut cursor = 0usize;
    for range in dropped {
        if range.start < cursor {
            continue; // overlapping drops: already removed
        }
        let mut next = range.end.min(original.len());
        while next < original.len() && matches!(bytes[next], b' ' | b'\t' | b'\r' | b'\n') {
            next += 1;
        }
        // A tail block takes the separator before it. A middle block keeps
        // that separator, so the kept blocks on either side stay apart.
        let keep_until = if next == original.len() {
            run_before(range.start).max(cursor)
        } else {
            range.start.max(cursor)
        };
        out.push_str(&original[cursor..keep_until]);
        cursor = next;
    }
    out.push_str(&original[cursor.min(original.len())..]);
    *body = out;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{ChromeReport, PageDocument, strip_chrome};

    fn page(path: &str, body: impl Into<String>) -> PageDocument {
        PageDocument {
            path: path.to_owned(),
            body: body.into(),
        }
    }

    /// A link list before the first heading is navigation and drops; the
    /// content after the heading survives byte-exactly.
    #[test]
    fn leading_navigation_drops() {
        let mut pages = vec![page(
            "https://x/a.md",
            "- [Home](/)\n- [Docs](/docs)\n\n# Guide\n\nInstall the widget first.\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(
            report,
            ChromeReport {
                pattern_drops: 1,
                frequency_drops: 0
            }
        );
        assert_eq!(pages[0].body, "# Guide\n\nInstall the widget first.\n");
    }

    /// A list before the first heading that mixes a link item with a plain
    /// text item is content, not navigation: one non-link item keeps the
    /// whole block.
    #[test]
    fn mixed_list_before_the_first_heading_survives() {
        let mut pages = vec![page(
            "https://x/a.md",
            "- [Docs](/docs)\n- plain text item\n\n# Guide\n\ncontent\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.pattern_drops, 0);
        assert!(pages[0].body.contains("plain text item"));
        assert!(pages[0].body.contains("[Docs](/docs)"));
    }

    /// A multi-line paragraph of links before the first heading is
    /// navigation: the soft break between the links does not break the
    /// link-only block.
    #[test]
    fn two_line_link_paragraph_drops_as_navigation() {
        let mut pages = vec![page(
            "https://x/a.md",
            "[Home](/)\n[Docs](/docs)\n\n# Guide\n\ncontent\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.pattern_drops, 1);
        assert_eq!(pages[0].body, "# Guide\n\ncontent\n");
    }

    /// An "Edit this page" link drops wherever it sits.
    #[test]
    fn edit_link_drops() {
        let mut pages = vec![page(
            "https://x/a.md",
            "# Guide\n\nInstall the widget first.\n\n[Edit this page on GitHub](/edit)\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.pattern_drops, 1);
        // The separator around the dropped tail block collapses with it.
        assert_eq!(pages[0].body, "# Guide\n\nInstall the widget first.");
    }

    /// A cookie banner drops; a paragraph that discusses cookies — with no
    /// link and no accept/consent call to action — never does.
    #[test]
    fn cookie_banner_drops_but_content_discussion_survives() {
        let mut pages = vec![
            page(
                "https://x/a.md",
                "# Guide\n\nWe use cookies to improve your experience. [Accept all](/accept)\n\nInstall the widget first.\n",
            ),
            page(
                "https://x/b.md",
                "# Storage\n\nCookies are stored in the session jar and expire after the\nlogin flow completes; the cookie jar never persists across\nrestarts of the daemon, the storage format is stable across\nreleases, and the expiry window tracks the license term of the\ndeployment rather than the wall clock of the client session.\n\nUse the jar.\n",
            ),
            page(
                "https://x/c.md",
                "# Privacy\n\nWe use cookies to improve your experience.\n\nMore content.\n",
            ),
        ];
        let report = strip_chrome(&mut pages);
        assert_eq!(
            report.pattern_drops, 1,
            "only the banner with the call to action drops"
        );
        assert!(
            pages[0].body.contains("Install the widget"),
            "the banner dropped"
        );
        assert!(
            !pages[0].body.contains("We use cookies"),
            "the banner is gone"
        );
        assert!(
            pages[1].body.contains("the cookie jar never persists"),
            "the long cookie paragraph survives"
        );
        assert!(
            pages[2].body.contains("We use cookies"),
            "a short cookie mention without the call to action survives"
        );
    }

    /// A dropped middle block keeps the separator before it, so the kept
    /// paragraphs on either side stay apart.
    #[test]
    fn middle_dropped_block_keeps_paragraphs_apart() {
        let mut pages = vec![page(
            "https://x/a.md",
            "# Guide\n\nFirst paragraph.\n\nWe use cookies to improve your experience. [Accept all](/accept)\n\nSecond paragraph.\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.pattern_drops, 1);
        assert!(!pages[0].body.contains("We use cookies"), "banner dropped");
        assert_eq!(
            pages[0].body, "# Guide\n\nFirst paragraph.\n\nSecond paragraph.\n",
            "the kept paragraphs stay separated by a blank line"
        );
    }

    /// A block identical in more than 60% of the pages drops everywhere it
    /// appears, even when no pattern rule matches it
    /// (docs/implementation/07 §Normalization pipeline stage 2).
    #[test]
    fn frequency_rule_drops_blocks_identical_across_pages() {
        let footer = "\n\n© Acme Inc. All rights reserved.\n";
        let mut pages = vec![
            page(
                "https://x/a.md",
                format!("# Guide\n\nGuide content one.{footer}"),
            ),
            page(
                "https://x/b.md",
                format!("# Storage\n\nStorage content two.{footer}"),
            ),
            page(
                "https://x/c.md",
                format!("# Routing\n\nRouting content three.{footer}"),
            ),
        ];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.frequency_drops, 3, "one drop per page carrying it");
        assert_eq!(report.pattern_drops, 0);
        assert_eq!(pages[0].body, "# Guide\n\nGuide content one.");
        assert_eq!(pages[1].body, "# Storage\n\nStorage content two.");
        assert_eq!(pages[2].body, "# Routing\n\nRouting content three.");
    }

    /// A block identical in only half the pages is content, not chrome.
    #[test]
    fn blocks_under_the_frequency_threshold_survive() {
        let shared = "\n\n© Acme Inc. All rights reserved.\n";
        let mut pages = vec![
            page(
                "https://x/a.md",
                format!("# Guide\n\nGuide content.{shared}"),
            ),
            page(
                "https://x/b.md",
                format!("# Storage\n\nStorage content.{shared}"),
            ),
            page("https://x/c.md", "# Routing\n\nRouting content.\n"),
            page("https://x/d.md", "# Search\n\nSearch content.\n"),
        ];
        let report = strip_chrome(&mut pages);
        assert_eq!(
            report.frequency_drops, 0,
            "2/4 pages is under the 60% threshold"
        );
        assert!(pages[0].body.contains("© Acme Inc."));
    }

    /// A heading identical across pages never frequency-drops: dropping it
    /// would corrupt the section paths of the chunks below it.
    #[test]
    fn frequency_rule_never_drops_headings() {
        let mut pages = vec![
            page(
                "https://x/a.md",
                "# Guide\n\nGuide content one.\n\n## Parameters\n\nParameter content.\n",
            ),
            page(
                "https://x/b.md",
                "# Storage\n\nStorage content two.\n\n## Parameters\n\nOther content.\n",
            ),
            page("https://x/c.md", "# Routing\n\nRouting content three.\n"),
        ];
        let report = strip_chrome(&mut pages);
        assert_eq!(
            report.frequency_drops, 0,
            "headings never count under the frequency rule"
        );
        assert!(
            pages[0].body.contains("## Parameters"),
            "the repeated heading survives in the body"
        );
        assert!(
            pages[1].body.contains("## Parameters"),
            "the repeated heading survives in the body"
        );
    }

    /// A one-page corpus never frequency-drops: with a single page every
    /// block is trivially identical in 100% of pages, which would erase the
    /// whole content.
    #[test]
    fn single_page_corpus_never_frequency_drops() {
        let mut pages = vec![page(
            "https://x/a.md",
            "# Guide\n\n© Acme Inc. All rights reserved.\n\nInstall the widget.\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.frequency_drops, 0);
        assert!(pages[0].body.contains("© Acme Inc."));
    }

    /// A block repeated twice inside one page of a multi-page corpus does
    /// not trip the frequency rule: the rule counts distinct pages, not
    /// occurrences.
    #[test]
    fn frequency_counts_distinct_pages_not_occurrences() {
        let mut pages = vec![
            page(
                "https://x/a.md",
                "# Guide\n\n© Acme Inc.\n\nInstall the widget.\n\n© Acme Inc.\n",
            ),
            page("https://x/b.md", "# Storage\n\nStore the state.\n"),
            page("https://x/c.md", "# Routing\n\nRoute the calls.\n"),
        ];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.frequency_drops, 0, "1/3 pages is not chrome");
        assert!(pages[0].body.contains("© Acme Inc."));
    }

    /// The segmentation property over a stripped corpus: chunk bodies are
    /// byte-exact slices of the stripped body, and the non-overlap spans
    /// reassemble the stripped body minus dropped chrome — the only gaps
    /// are whitespace and heading lines
    /// (docs/implementation/07 §Segmentation algorithm).
    #[test]
    fn chunks_reassemble_the_stripped_body() {
        let mut pages = vec![page(
            "https://x/a.md",
            "- [Home](/)\n- [Docs](/docs)\n\n# Guide\n\nInstall the widget first.\n\n## Setup\n\nRun the installer.\n\n```bash\nexport KEY=1\n```\n",
        )];
        let report = strip_chrome(&mut pages);
        assert_eq!(report.pattern_drops, 1);
        let stripped = pages[0].body.clone();
        assert!(!stripped.contains("[Home]"), "the nav dropped");
        let chunks = crate::segment::Segmenter::new()
            .unwrap()
            .segment("acme", "1.0", &stripped);
        assert_ne!(chunks, [] as [crate::segment::PreparedChunk; 0]);
        for chunk in &chunks {
            assert_eq!(
                &stripped[chunk.page_range.clone()],
                chunk.body,
                "chunk bodies are byte-exact slices"
            );
        }
        // The gaps between chunk spans hold only whitespace or heading
        // lines: no non-chrome content was lost by the strip or the
        // segmentation.
        for pair in chunks.windows(2) {
            let gap = &stripped[pair[0].page_range.end..pair[1].page_range.start];
            assert!(
                gap.lines()
                    .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
                "non-whitespace gap {gap:?} in the stripped body"
            );
        }
        // The tail is covered.
        assert!(
            chunks
                .last()
                .is_some_and(|last| last.page_range.end >= stripped.trim_end().len())
        );
    }
}
