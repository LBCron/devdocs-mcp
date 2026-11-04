//! The llms.txt v2 index parser (docs/implementation/07 §llms.txt adapter).
//!
//! Grammar (llmstxt.org v2, verified 2026-11): optional BOM, required H1,
//! optional blockquote, zero or more non-heading sections, zero or more
//! H2-delimited sections each carrying a link list. A link line is
//! `- [title](url)` optionally followed by `: description`. The parser is
//! lenient about prose between links and skips fenced blocks.

use crate::error::IngestError;

/// One link line of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmsLink {
    title: String,
    url: String,
    description: Option<String>,
    markdown_twin: Option<String>,
    described_by: Option<String>,
}

impl LlmsLink {
    /// The link text.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The link URL as written in the index.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The notes after the colon, when present.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The markdown twin the v2 `rel="alternate" type="text/markdown"`
    /// hint names, when present (docs/implementation/07 §llms.txt adapter
    /// rule 2).
    #[must_use]
    pub fn markdown_twin(&self) -> Option<&str> {
        self.markdown_twin.as_deref()
    }

    /// The covering index the v2 `rel="describedby"` hint names, when
    /// present (docs/implementation/07 §llms.txt adapter rule 2).
    #[must_use]
    pub fn described_by(&self) -> Option<&str> {
        self.described_by.as_deref()
    }
}

/// One H2-delimited section of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmsSection {
    /// The H2 heading text; `None` for links before the first H2.
    pub heading: Option<String>,
    /// The section's link lines, in document order.
    pub links: Vec<LlmsLink>,
}

/// The parsed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmsTxtIndex {
    /// The H1 title — the only required section of the grammar.
    pub title: String,
    /// The blockquote summary, when present.
    pub summary: Option<String>,
    /// Sections in document order; a leading section with `heading: None`
    /// holds links that sit before the first H2.
    pub sections: Vec<LlmsSection>,
}

impl LlmsTxtIndex {
    /// Every link of the index, in document order.
    pub fn links(&self) -> impl Iterator<Item = &LlmsLink> {
        self.sections.iter().flat_map(|section| &section.links)
    }
}

/// Parses an llms.txt v2 index.
///
/// # Errors
/// [`IngestError::LlmsTxtInvalid`] when the H1 is missing, and
/// [`IngestError::LlmsTxtEmpty`] when the parsed index carries zero link
/// lines (docs/implementation/07 §llms.txt adapter rule 5).
pub fn parse_llms_txt(text: &str, url: &str) -> Result<LlmsTxtIndex, IngestError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();

    let title = parse_title(&mut lines, url)?;
    let mut lines = lines.peekable();
    let summary = parse_summary(&mut lines);

    // Sections and link lines. A fenced block closes only on a line of the
    // opening marker's character with at least its run length, per
    // CommonMark (the grammar has no code blocks; this leniency keeps code
    // samples in an index from leaking link lines).
    let mut sections: Vec<LlmsSection> = Vec::new();
    let mut link_count = 0usize;
    let mut open_fence: Option<(char, usize)> = None;
    for line in lines {
        let trimmed = line.trim();
        if let Some((marker, run)) = fence_marker(trimmed) {
            match open_fence {
                // Inside a fence: only a run at least as long, of the same
                // character, holding nothing else closes it. Outside: any
                // marker opens one.
                Some(open) => {
                    if fence_closes(open, trimmed) {
                        open_fence = None;
                    }
                }
                None => open_fence = Some((marker, run)),
            }
            continue;
        }
        if open_fence.is_some() {
            continue;
        }
        if push_section_line(&mut sections, trimmed) {
            link_count += 1;
        }
    }

    if link_count == 0 {
        return Err(IngestError::LlmsTxtEmpty {
            url: url.to_owned(),
        });
    }
    Ok(LlmsTxtIndex {
        title,
        summary,
        sections,
    })
}

/// Handles one line outside a fence: an H2 opens a section, a link line
/// joins the current one (opening the heading-less leading section when
/// the page starts with links). Returns true when the line was a link.
fn push_section_line(sections: &mut Vec<LlmsSection>, trimmed: &str) -> bool {
    if let Some(heading) = trimmed.strip_prefix("## ") {
        sections.push(LlmsSection {
            heading: Some(heading.trim().to_owned()),
            links: Vec::new(),
        });
        return false;
    }
    let Some(link) = parse_link_line(trimmed) else {
        return false;
    };
    if sections.is_empty() {
        sections.push(LlmsSection {
            heading: None,
            links: Vec::new(),
        });
    }
    if let Some(section) = sections.last_mut() {
        section.links.push(link);
    }
    true
}

/// Consumes the lines up to and including the required H1
/// (llmstxt.org v2). Blank lines before it are skipped.
fn parse_title<'a, I>(lines: &mut I, url: &str) -> Result<String, IngestError>
where
    I: Iterator<Item = &'a str>,
{
    loop {
        match lines.next() {
            Some(line) if !line.trim().is_empty() => {
                let Some(rest) = line.trim_start().strip_prefix("# ") else {
                    return Err(IngestError::LlmsTxtInvalid {
                        url: url.to_owned(),
                        reason: format!("expected the H1 title, found: {line:?}"),
                    });
                };
                let title = rest.trim();
                if title.is_empty() {
                    // llms.txt v2 requires the H1 to carry the project name
                    // (docs/implementation/07-ingest-pipeline.md §llms.txt adapter (v2)).
                    return Err(IngestError::LlmsTxtInvalid {
                        url: url.to_owned(),
                        reason: String::from("the H1 title is empty"),
                    });
                }
                return Ok(title.to_owned());
            }
            Some(_) => {}
            None => {
                return Err(IngestError::LlmsTxtInvalid {
                    url: url.to_owned(),
                    reason: String::from("the index is empty"),
                });
            }
        }
    }
}

/// Consumes the blockquote: consecutive `>` lines after the H1. Blank
/// lines may sit between the H1 and the quote; a blank line ends the quote
/// the way a markdown blank line ends a block. The line that ends the scan
/// stays unconsumed for the section loop.
fn parse_summary<'a, I>(lines: &mut std::iter::Peekable<I>) -> Option<String>
where
    I: Iterator<Item = &'a str>,
{
    let mut summary_lines: Vec<&str> = Vec::new();
    while let Some(line) = lines.peek() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if summary_lines.is_empty() {
                lines.next();
                continue;
            }
            break;
        }
        if let Some(quoted) = trimmed.strip_prefix('>') {
            summary_lines.push(quoted.strip_prefix(' ').unwrap_or(quoted));
            lines.next();
        } else {
            break;
        }
    }
    if summary_lines.is_empty() {
        None
    } else {
        Some(summary_lines.join(" "))
    }
}

/// Whether `line` closes the open fence `(marker, run)`: the same marker
/// character, at least the opening run length, and nothing but the marker
/// run on the line (`CommonMark` §4.4 — a sample line that starts with the
/// marker run and carries trailing text is fence content, not a close).
/// Shared by the index scanner and the segmenter's fence probe.
pub(crate) fn fence_closes(open: (char, usize), line: &str) -> bool {
    let Some((marker, run)) = fence_marker(line) else {
        return false;
    };
    marker == open.0 && run >= open.1 && line.chars().all(|c| c == marker)
}

/// The fence marker on a line: the marker character and its run length,
/// when the line could open or close a fenced block (three or more
/// backticks or tildes). Shared with the segmenter's fence probe.
pub(crate) fn fence_marker(trimmed: &str) -> Option<(char, usize)> {
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|&c| c == marker).count();
    (run >= 3).then_some((marker, run))
}

/// Parses one link line: `- [title](url)` with an optional `: description`.
/// A leading `*` bullet is tolerated next to `-`.
fn parse_link_line(line: &str) -> Option<LlmsLink> {
    let rest = line
        .strip_prefix("- [")
        .or_else(|| line.strip_prefix("* ["))?;
    let close_bracket = rest.find("](")?;
    let title = &rest[..close_bracket];
    let after = &rest[close_bracket + 2..];
    // The destination ends at a parenthesis at nesting depth zero: balanced
    // parentheses are legal path characters (RFC 3986 sub-delims), so
    // `guide_(v2).md` must not truncate at the first `)`.
    let mut depth = 0usize;
    let url_end = after
        .char_indices()
        .find(|(_, c)| match c {
            '(' => {
                depth += 1;
                false
            }
            ')' => {
                if depth == 0 {
                    true
                } else {
                    depth -= 1;
                    false
                }
            }
            _ => false,
        })?
        .0;
    let url = &after[..url_end];
    // The spec requires a hyperlink `[name](url)`: a blank name or an
    // empty URL is not a link line.
    if title.trim().is_empty() || url.trim().is_empty() {
        return None;
    }
    // The optional parenthesized per-page hints sit before the colon
    // (docs/implementation/07 §llms.txt adapter rule 2):
    // `- [Page](url) (rel="alternate" type="text/markdown" twin-url): notes`.
    let (hint_text, tail) = split_hints(&after[url_end + 1..]);
    let description = tail
        .trim()
        .strip_prefix(':')
        .map(str::trim)
        .filter(|desc| !desc.is_empty())
        .map(str::to_owned);
    let (markdown_twin, described_by) = parse_hints(&hint_text);
    Some(LlmsLink {
        title: title.to_owned(),
        url: url.to_owned(),
        description,
        markdown_twin,
        described_by,
    })
}

/// Splits the text after a link destination into the optional
/// parenthesized hint group and the tail after it. No group means the
/// whole text is the tail.
fn split_hints(tail: &str) -> (String, &str) {
    let trimmed_start = tail.trim_start();
    let Some(rest) = trimmed_start.strip_prefix('(') else {
        return (String::new(), tail);
    };
    let mut depth = 1usize;
    for (offset, ch) in rest.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (rest[..offset].to_owned(), &rest[offset + 1..]);
                }
            }
            _ => {}
        }
    }
    (String::new(), tail)
}

/// Parses the v2 relations out of one hint group: `rel="alternate"
/// type="text/markdown" twin-url` and `rel="describedby" index-url`. The
/// target URL is the first token that is not a `rel=` or `type=`
/// parameter.
fn parse_hints(hint: &str) -> (Option<String>, Option<String>) {
    let tokens: Vec<&str> = hint.split_whitespace().collect();
    let mut twin = None;
    let mut described = None;
    let mut index = 0usize;
    while index < tokens.len() {
        let Some(rel) = tokens[index]
            .strip_prefix("rel=")
            .map(|value| value.trim_matches('"'))
        else {
            index += 1;
            continue;
        };
        match rel {
            "alternate" => {
                let mut markdown_type = false;
                let mut target = None;
                let mut scan = index + 1;
                while scan < tokens.len() && !tokens[scan].starts_with("rel=") {
                    // Quotes can sit inside a token (`type="text/markdown"`),
                    // so strip every quote, not only edge ones.
                    let token = tokens[scan].replace('"', "");
                    if token.eq_ignore_ascii_case("type=text/markdown") {
                        markdown_type = true;
                    } else if !token.starts_with("type=") && target.is_none() {
                        target = Some(token);
                    }
                    scan += 1;
                }
                if markdown_type {
                    twin = target;
                }
                index = scan;
            }
            "describedby" => {
                let mut scan = index + 1;
                while scan < tokens.len() && !tokens[scan].starts_with("rel=") {
                    let token = tokens[scan].replace('"', "");
                    if !token.starts_with("type=") {
                        described = Some(token);
                        break;
                    }
                    scan += 1;
                }
                index = scan;
            }
            _ => index += 1,
        }
    }
    (twin, described)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use devdocs_testkit::fixtures;

    use super::{LlmsTxtIndex, parse_llms_txt};
    use crate::error::IngestError;

    /// The code shape helper for asserting one link.
    fn link_at(index: &LlmsTxtIndex, section: usize, link: usize) -> (&str, &str, Option<&str>) {
        let s = &index.sections[section];
        (
            s.links[link].title(),
            s.links[link].url(),
            s.links[link].description(),
        )
    }

    /// The llmstxt.org-shaped example parses with title, summary, sections,
    /// and links (docs/04 §Fixtures).
    #[test]
    fn parses_the_spec_example() {
        let index = parse_llms_txt(
            fixtures::LLMS_TXT_SPEC_EXAMPLE,
            "https://fastht.ml/llms.txt",
        )
        .unwrap();
        assert_eq!(index.title, "FastHTML");
        assert!(
            index
                .summary
                .as_deref()
                .is_some_and(|s| s.contains("Starlette, Uvicorn, HTMX"))
        );
        assert_eq!(index.sections.len(), 3);
        assert_eq!(index.sections[0].heading.as_deref(), Some("Docs"));
        assert_eq!(index.sections[1].heading.as_deref(), Some("Examples"));
        assert_eq!(index.sections[2].heading.as_deref(), Some("Optional"));
        assert_eq!(link_at(&index, 0, 0).0, "FastHTML quick start");
        assert_eq!(
            link_at(&index, 0, 0).1,
            "https://fastht.ml/docs/tutorials/quickstart_for_web_devs.html.md"
        );
        assert!(link_at(&index, 0, 0).2.is_some());
        assert_eq!(index.links().count(), 4);
    }

    /// A Mintlify-style index parses (docs/04 §Fixtures).
    #[test]
    fn parses_the_mintlify_style_index() {
        let index = parse_llms_txt(
            fixtures::LLMS_TXT_MINTLIFY_STYLE,
            "https://docs.acme.dev/llms.txt",
        )
        .unwrap();
        assert_eq!(index.title, "Acme Widgets");
        assert_eq!(index.sections.len(), 2);
        assert_eq!(index.links().count(), 4);
        assert_eq!(link_at(&index, 1, 0).0, "Authentication");
        assert_eq!(
            link_at(&index, 1, 0).1,
            "https://docs.acme.dev/api-reference/authentication.md"
        );
    }

    /// An index with zero links fails `LLMS_TXT_EMPTY`
    /// (docs/implementation/07 §llms.txt adapter rule 5).
    #[test]
    fn zero_links_fails_llms_txt_empty() {
        let error = parse_llms_txt(
            fixtures::LLMS_TXT_NO_LINKS,
            "https://docs.acme.dev/llms.txt",
        )
        .unwrap_err();
        match error {
            IngestError::LlmsTxtEmpty { url } => {
                assert_eq!(url, "https://docs.acme.dev/llms.txt");
            }
            other => panic!("expected LlmsTxtEmpty, got {other}"),
        }
    }

    /// A file without the required H1 fails as invalid.
    #[test]
    fn missing_h1_fails_invalid() {
        for text in ["", "just some text\n\n- [a](https://x.md)"] {
            let error = parse_llms_txt(text, "https://x/llms.txt").unwrap_err();
            assert!(
                matches!(error, IngestError::LlmsTxtInvalid { .. }),
                "{text}"
            );
        }
    }

    /// An H1 that carries no title fails as invalid: llms.txt v2 requires
    /// the H1 to name the project (docs/implementation/07 §llms.txt adapter
    /// (v2)).
    #[test]
    fn empty_h1_fails_invalid() {
        for text in ["# \n\n- [a](https://x/a.md)", "#   \n- [a](https://x/a.md)"] {
            let error = parse_llms_txt(text, "https://x/llms.txt").unwrap_err();
            match error {
                IngestError::LlmsTxtInvalid { reason, .. } => {
                    assert_eq!(reason, "the H1 title is empty");
                }
                other => panic!("expected LlmsTxtInvalid, got {other}"),
            }
        }
    }

    /// A leading BOM is stripped before the H1 check.
    #[test]
    fn bom_is_tolerated() {
        let index = parse_llms_txt("\u{feff}# T\n\n- [a](https://x/a.md)", "u").unwrap();
        assert_eq!(index.title, "T");
        assert_eq!(index.links().count(), 1);
    }

    /// A link destination keeps its balanced parentheses: the line ends at
    /// the depth-zero close, not at the first `)`.
    #[test]
    fn link_destination_keeps_balanced_parentheses() {
        let index =
            parse_llms_txt("# T\n\n- [Guide](https://x/guide_(v2).md): notes\n", "u").unwrap();
        assert_eq!(link_at(&index, 0, 0).1, "https://x/guide_(v2).md");
        assert_eq!(link_at(&index, 0, 0).2, Some("notes"));
    }

    /// A fenced block skips link lines inside it, and a marker of the
    /// other character does not close a fence.
    #[test]
    fn fence_skips_links_until_the_matching_close() {
        let text = concat!(
            "# T\n\n",
            "~~~\n",
            "```json\n",
            "- [inside](https://x/inside.md)\n",
            "```\n",
            "- [still_inside](https://x/still.md)\n",
            "~~~\n",
            "- [outside](https://x/outside.md)\n",
        );
        let index = parse_llms_txt(text, "u").unwrap();
        assert_eq!(index.links().count(), 1, "{index:?}");
        assert_eq!(link_at(&index, 0, 0).0, "outside");
    }

    /// Links before the first H2 land in a heading-less section.
    #[test]
    fn loose_links_before_sections() {
        let index = parse_llms_txt(
            "# T\n\n- [first](https://x/first.md)\n\n## Docs\n\n- [second](https://x/second.md)\n",
            "u",
        )
        .unwrap();
        assert_eq!(index.sections.len(), 2);
        assert_eq!(index.sections[0].heading, None);
        assert_eq!(index.sections[0].links.len(), 1);
        assert_eq!(index.sections[1].heading.as_deref(), Some("Docs"));
    }

    /// Fenced blocks do not contribute links.
    #[test]
    fn fenced_links_are_skipped() {
        let text =
            "# T\n\n```md\n- [fake](https://x/fake.md)\n```\n\n- [real](https://x/real.md)\n";
        let index = parse_llms_txt(text, "u").unwrap();
        assert_eq!(index.links().count(), 1);
        assert_eq!(index.links().next().unwrap().url(), "https://x/real.md");
    }

    /// A line that carries the marker run plus trailing text is fence
    /// content, not a close: the fence stays open until the marker-only
    /// line, and links inside the sample are never parsed.
    #[test]
    fn fence_close_needs_a_marker_only_line() {
        let text = concat!(
            "# T\n\n",
            "- [real](https://x/real.md): ok\n",
            "```\n",
            "```example\n",
            "- [fake](https://x/fake.md): inside the sample\n",
            "```\n",
            "- [after](https://x/after.md): ok\n",
        );
        let index = parse_llms_txt(text, "u").unwrap();
        assert_eq!(index.links().count(), 2, "{index:?}");
        assert_eq!(link_at(&index, 0, 0).0, "real");
        assert_eq!(link_at(&index, 0, 1).0, "after");
    }

    /// The description is optional and may sit after a colon on the same
    /// line; bullets with `*` are tolerated.
    #[test]
    fn link_line_shapes() {
        let index = parse_llms_txt(
            "# T\n\n- [a](https://x/a.md): desc one\n* [b](https://x/b.md)\n- [ ](https://x/c.md)\n- [](https://x/d.md)\n",
            "u",
        )
        .unwrap();
        assert_eq!(index.links().count(), 2);
        assert_eq!(link_at(&index, 0, 0).2, Some("desc one"));
        assert_eq!(link_at(&index, 0, 1).2, None);
    }

    /// The v2 parenthesized hints parse: the markdown twin and the
    /// covering index. The description after the hints still parses
    /// (docs/implementation/07 §llms.txt adapter rule 2).
    #[test]
    fn v2_hints_carry_the_relations() {
        let index = parse_llms_txt(
            concat!(
                "# T\n\n",
                "- [Guide](https://x/guide.html) (rel=\"alternate\" type=\"text/markdown\" https://x/guide.md): notes\n",
                "- [Api](https://x/api.html) (rel=\"describedby\" /x/api/llms.txt)\n",
                "- [Plain](https://x/plain.md)\n",
            ),
            "u",
        )
        .unwrap();
        assert_eq!(index.links().count(), 3);
        assert_eq!(
            index.links().next().unwrap().markdown_twin(),
            Some("https://x/guide.md")
        );
        assert_eq!(index.links().next().unwrap().description(), Some("notes"));
        let api = index.links().nth(1).unwrap();
        assert_eq!(api.described_by(), Some("/x/api/llms.txt"));
        assert_eq!(api.markdown_twin(), None);
        let plain = index.links().nth(2).unwrap();
        assert_eq!(plain.markdown_twin(), None);
        assert_eq!(plain.described_by(), None);
    }

    /// A `rel="alternate"` hint without the markdown type names no twin.
    #[test]
    fn alternate_hint_needs_the_markdown_type() {
        let index = parse_llms_txt(
            "# T\n\n- [Pdf](https://x/doc.pdf) (rel=\"alternate\" type=\"application/pdf\" https://x/doc.pdf)\n",
            "u",
        )
        .unwrap();
        assert_eq!(index.links().next().unwrap().markdown_twin(), None);
    }

    /// An unterminated hint group leaves the line as a plain link.
    #[test]
    fn unterminated_hints_do_not_break_the_line() {
        let index = parse_llms_txt(
            "# T\n\n- [Guide](https://x/guide.html) (rel=\"alternate\" never closed\n",
            "u",
        )
        .unwrap();
        let link = index.links().next().unwrap();
        assert_eq!(link.markdown_twin(), None);
        assert_eq!(link.url(), "https://x/guide.html");
    }

    /// A hint group whose parentheses nest after the twin URL — as in a
    /// `(v2)` suffix — closes at the depth-zero `)`, so the URL and the
    /// description after the group both parse
    /// (docs/implementation/07 §llms.txt adapter rule 2).
    #[test]
    fn nested_parentheses_in_a_hint_group() {
        let index = parse_llms_txt(
            concat!(
                "# T\n\n",
                "- [Guide](https://x/g.html) (rel=\"alternate\" type=\"text/markdown\" ",
                "https://x/g.md (v2)): notes\n",
            ),
            "u",
        )
        .unwrap();
        let link = index.links().next().unwrap();
        assert_eq!(link.markdown_twin(), Some("https://x/g.md"), "{link:?}");
        assert_eq!(link.description(), Some("notes"));
    }

    /// A `rel="describedby"` hint whose `type=` token sits before the index
    /// URL: the token is skipped and the next non-parameter token names the
    /// covering index (docs/implementation/07 §llms.txt adapter rule 2).
    #[test]
    fn describedby_hint_skips_a_leading_type_token() {
        let index = parse_llms_txt(
            "# T\n\n- [Api](https://x/api.html) (rel=\"describedby\" type=\"text/plain\" /x/api/llms.txt)\n",
            "u",
        )
        .unwrap();
        let link = index.links().next().unwrap();
        assert_eq!(link.described_by(), Some("/x/api/llms.txt"), "{link:?}");
        assert_eq!(link.markdown_twin(), None);
    }

    /// A hint group naming a relation kind the adapter does not map —
    /// neither `alternate` nor `describedby` — is ignored, and the link
    /// itself still parses.
    #[test]
    fn unmapped_relation_kind_is_ignored() {
        let index = parse_llms_txt(
            "# T\n\n- [X](https://x/x.html) (rel=\"bookmark\" https://x/other.html)\n",
            "u",
        )
        .unwrap();
        let link = index.links().next().unwrap();
        assert_eq!(link.markdown_twin(), None);
        assert_eq!(link.described_by(), None);
        assert_eq!(link.url(), "https://x/x.html");
        assert_eq!(link.title(), "X");
    }
}
