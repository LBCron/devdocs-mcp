//! The v2 link relations of one page response
//! (docs/implementation/07 §llms.txt adapter rule 2, llmstxt.org v2).
//!
//! Two relations matter for ingest: `rel="alternate"
//! type="text/markdown"` names a page's markdown twin — it wins over the
//! HTML page — and `rel="describedby"` names the llms.txt file that covers
//! the page. The relations arrive as HTML `<link>` elements or an HTTP
//! `Link:` header (RFC 8288); the index link lines may carry the same
//! information as parenthesized per-page hints (docs/07 rule 2). A response
//! relation wins over the index hint: the response is the fresher data.

/// The v2 relations of one page response, or one index hint group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRelations {
    /// The markdown twin of the page, when a relation declares one.
    pub markdown_twin: Option<String>,
    /// The llms.txt index that covers the page, when a relation declares
    /// one.
    pub described_by: Option<String>,
}

impl PageRelations {
    /// Merges two relation sources: the left one wins where it declares a
    /// relation, the right one fills the gaps.
    #[must_use]
    pub fn merge(self, other: PageRelations) -> PageRelations {
        PageRelations {
            markdown_twin: self.markdown_twin.or(other.markdown_twin),
            described_by: self.described_by.or(other.described_by),
        }
    }

    /// Parses the relations out of a page's `Link:` response headers
    /// (RFC 8288 subset): `<uri>; rel="alternate"; type="text/markdown"`,
    /// `<uri>; rel="describedby"`. Unparsable link values are ignored.
    #[must_use]
    pub fn from_link_headers(values: &[String]) -> PageRelations {
        let mut relations = PageRelations::default();
        for value in values {
            for link_value in split_link_values(value) {
                let Some((uri, params)) = parse_link_value(&link_value) else {
                    continue;
                };
                // RFC 8288: the rel value is a space-separated token list,
                // so one link can carry both relations.
                let rels: Vec<&str> = params.rel.split_ascii_whitespace().collect();
                if rels.contains(&"alternate") && params.type_ == "text/markdown" {
                    relations.markdown_twin = Some(uri.clone());
                }
                if rels.contains(&"describedby") {
                    relations.described_by = Some(uri);
                }
            }
        }
        relations
    }

    /// Scans an HTML body for `<link>` elements carrying the relations:
    /// `<link rel="alternate" type="text/markdown" href="...">` and
    /// `<link rel="describedby" href="...">`. The scan is tag-level, not a
    /// full HTML parse — enough for the elements the spec names.
    #[must_use]
    pub fn from_html(body: &str) -> PageRelations {
        let mut relations = PageRelations::default();
        let body = strip_html_comments(body);
        let lower = body.to_ascii_lowercase();
        let mut cursor = 0usize;
        while let Some(found) = lower[cursor..].find("<link") {
            let tag_start = cursor + found;
            let Some(tag_end_rel) = lower[tag_start..].find('>') else {
                break;
            };
            let tag_end = tag_start + tag_end_rel;
            let tag = &body[tag_start..=tag_end];
            let href = attribute(tag, "href");
            let rel = attribute(tag, "rel")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let type_ = attribute(tag, "type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if let Some(href) = href {
                let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
                if rels.contains(&"alternate") && type_ == "text/markdown" {
                    relations.markdown_twin = Some(href.clone());
                }
                if rels.contains(&"describedby") {
                    relations.described_by = Some(href);
                }
            }
            cursor = tag_end + 1;
        }
        relations
    }
}

/// Splits a `Link:` header value into its comma-separated link values,
/// ignoring commas inside `<...>` and quoted strings.
fn split_link_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut angle_depth = 0usize;
    let mut in_quotes = false;
    for ch in value.chars() {
        match ch {
            '<' => angle_depth += 1,
            '>' => angle_depth = angle_depth.saturating_sub(1),
            '"' => in_quotes = !in_quotes,
            _ => {}
        }
        if ch == ',' && angle_depth == 0 && !in_quotes {
            values.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        values.push(current);
    }
    values
}

/// One link-value's parameters.
struct LinkParams {
    rel: String,
    type_: String,
}

/// Parses one link value into its target URI and parameters.
fn parse_link_value(link_value: &str) -> Option<(String, LinkParams)> {
    let link_value = link_value.trim();
    let rest = link_value.strip_prefix('<')?;
    let close = rest.find('>')?;
    let uri = rest[..close].to_owned();
    let params_text = rest[close + 1..].trim();
    let mut rel = String::new();
    let mut type_ = String::new();
    for param in params_text.split(';') {
        let param = param.trim();
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().trim_matches('"');
        match name.as_str() {
            "rel" => rel = value.to_ascii_lowercase(),
            "type" => type_ = value.to_ascii_lowercase(),
            _ => {}
        }
    }
    Some((uri, LinkParams { rel, type_ }))
}

/// Drops HTML comments from the body: `<!-- ... -->` is removed; an
/// unterminated comment drops the rest of the input.
fn strip_html_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) => rest = &after[end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// One HTML attribute value from a tag text; the name matches
/// case-insensitively, and whitespace may surround the `=`.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let mut search = 0usize;
    while let Some(found) = lower[search..].find(&needle) {
        let at = search + found;
        // Only an attribute position: the start of the tag or whitespace
        // before the name, so `data-href=` never matches `href=`.
        let boundary = at == 0
            || tag[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if !boundary {
            search = at + needle.len();
            continue;
        }
        let rest = &tag[at + needle.len()..];
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            search = at + needle.len();
            continue;
        };
        let value = value.trim_start();
        if let Some(quoted) = value.strip_prefix('"') {
            let end = quoted.find('"')?;
            return Some(quoted[..end].to_owned());
        }
        if let Some(quoted) = value.strip_prefix('\'') {
            let end = quoted.find('\'')?;
            return Some(quoted[..end].to_owned());
        }
        let end = value
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(value.len());
        return Some(value[..end].to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::PageRelations;

    /// The RFC 8288 header form of the two v2 relations parses: the twin
    /// and the covering index.
    #[test]
    fn link_headers_carry_the_relations() {
        let relations = PageRelations::from_link_headers(&[String::from(
            "</docs/page.html.md>; rel=\"alternate\"; type=\"text/markdown\", </docs/llms.txt>; rel=\"describedby\"",
        )]);
        assert_eq!(
            relations.markdown_twin.as_deref(),
            Some("/docs/page.html.md")
        );
        assert_eq!(relations.described_by.as_deref(), Some("/docs/llms.txt"));
    }

    /// Unquoted parameters and separate header values parse too; a
    /// `rel="alternate"` without the markdown type is no twin.
    #[test]
    fn header_shapes_and_guards() {
        let relations = PageRelations::from_link_headers(&[
            String::from("<https://x/page.md>; rel=alternate; type=text/markdown"),
            String::from("<https://x/llms.txt>; rel=describedby"),
        ]);
        assert_eq!(
            relations.markdown_twin.as_deref(),
            Some("https://x/page.md")
        );
        assert_eq!(
            relations.described_by.as_deref(),
            Some("https://x/llms.txt")
        );

        let relations =
            PageRelations::from_link_headers(&[String::from("<https://x/pdf.pdf>; rel=alternate")]);
        assert_eq!(relations.markdown_twin, None);
    }

    /// An HTML `<link>` element carries the relations; a `data-href` look
    /// -alike never matches.
    #[test]
    fn html_link_elements_carry_the_relations() {
        let body = concat!(
            "<!doctype html><html><head>",
            "<link rel=\"stylesheet\" href=\"/style.css\">",
            "<link rel=\"alternate\" type=\"text/markdown\" href=\"/docs/page.html.md\">",
            "<link rel=\"describedby\" href=\"/docs/llms.txt\">",
            "</head></html>"
        );
        let relations = PageRelations::from_html(body);
        assert_eq!(
            relations.markdown_twin.as_deref(),
            Some("/docs/page.html.md")
        );
        assert_eq!(relations.described_by.as_deref(), Some("/docs/llms.txt"));

        let relations = PageRelations::from_html("<a data-rel=\"alternate\" data-href=\"/x.md\">");
        assert_eq!(relations.markdown_twin, None);
    }

    /// An unparsable link value — no `<uri>` target — yields no relation
    /// (docs/implementation/07 §llms.txt adapter rule 2: unparsable values
    /// are ignored, never a failure).
    #[test]
    fn unparsable_link_value_yields_no_relation() {
        let relations = PageRelations::from_link_headers(&[String::from("bogus")]);
        assert_eq!(relations.markdown_twin, None);
        assert_eq!(relations.described_by, None);
    }

    /// An unterminated `<link` tag — no closing `>` — stops the scan and
    /// yields no relation.
    #[test]
    fn unterminated_link_tag_yields_no_relation() {
        let relations = PageRelations::from_html(
            "<link rel=\"alternate\" type=\"text/markdown\" href=\"/x.md\"",
        );
        assert_eq!(relations.markdown_twin, None);
        assert_eq!(relations.described_by, None);
    }

    /// A `<link>` element without an `href` carries no relation.
    #[test]
    fn link_tag_without_href_yields_no_relation() {
        let relations = PageRelations::from_html("<link rel=\"alternate\" type=\"text/markdown\">");
        assert_eq!(relations.markdown_twin, None);
        assert_eq!(relations.described_by, None);
    }

    /// A link-value parameter other than `rel` or `type` (`title=`) is
    /// skipped without losing the `rel` that follows it.
    #[test]
    fn unknown_link_parameters_are_skipped() {
        let relations = PageRelations::from_link_headers(&[String::from(
            "<https://x/llms.txt>; title=\"Docs\"; rel=\"describedby\"",
        )]);
        assert_eq!(
            relations.described_by.as_deref(),
            Some("https://x/llms.txt")
        );
        assert_eq!(relations.markdown_twin, None);
    }

    /// A `data-href` look-alike never satisfies `href=`: the attribute scan
    /// rejects a name not at an attribute boundary and keeps scanning to
    /// the real `href`.
    #[test]
    fn data_href_does_not_hide_the_real_href() {
        let relations = PageRelations::from_html(
            "<link data-href=\"/decoy\" rel=\"alternate\" type=\"text/markdown\" href=\"/real.md\">",
        );
        assert_eq!(relations.markdown_twin.as_deref(), Some("/real.md"));
    }

    /// Single-quoted attribute values parse.
    #[test]
    fn single_quoted_attribute_values_parse() {
        let relations =
            PageRelations::from_html("<link rel='alternate' type='text/markdown' href='/y.md'>");
        assert_eq!(relations.markdown_twin.as_deref(), Some("/y.md"));
    }

    /// Unquoted attribute values parse up to the next whitespace or tag
    /// close; a `/` in the value never ends it.
    #[test]
    fn unquoted_attribute_values_parse() {
        let relations =
            PageRelations::from_html("<link rel=alternate type=text/markdown href=/docs/page.md>");
        assert_eq!(relations.markdown_twin.as_deref(), Some("/docs/page.md"));
    }

    /// The rel value is a space-separated token list (RFC 8288): one link
    /// carrying `alternate describedby` sets both relations.
    #[test]
    fn rel_token_list_carries_both_relations() {
        let relations = PageRelations::from_link_headers(&[String::from(
            "</docs/page.html.md>; rel=\"alternate describedby\"; type=\"text/markdown\"",
        )]);
        assert_eq!(
            relations.markdown_twin.as_deref(),
            Some("/docs/page.html.md")
        );
        assert_eq!(
            relations.described_by.as_deref(),
            Some("/docs/page.html.md")
        );

        let relations = PageRelations::from_html(
            "<link rel=\"alternate describedby\" type=\"text/markdown\" href=\"/x.md\">",
        );
        assert_eq!(relations.markdown_twin.as_deref(), Some("/x.md"));
        assert_eq!(relations.described_by.as_deref(), Some("/x.md"));
    }

    /// A commented-out twin is ignored; the active one after it is found.
    #[test]
    fn commented_out_links_are_ignored() {
        let relations = PageRelations::from_html(
            "<!-- <link rel=\"alternate\" type=\"text/markdown\" href=\"/old.md\"> -->\
             <link rel=\"alternate\" type=\"text/markdown\" href=\"/new.md\">",
        );
        assert_eq!(relations.markdown_twin.as_deref(), Some("/new.md"));

        let relations = PageRelations::from_html(
            "<!-- <link rel=\"alternate\" type=\"text/markdown\" href=\"/old.md\"",
        );
        assert_eq!(relations.markdown_twin, None);
    }

    /// Whitespace may surround the `=` between an attribute name and its
    /// value.
    #[test]
    fn whitespace_around_the_equals_sign_parses() {
        let relations = PageRelations::from_html(
            "<link rel=\"alternate\" type=\"text/markdown\" href = \"/x.md\">",
        );
        assert_eq!(relations.markdown_twin.as_deref(), Some("/x.md"));
    }

    /// The response relations win over the index hint where both declare
    /// the same relation kind.
    #[test]
    fn response_relations_win_over_the_hint() {
        let response = PageRelations {
            markdown_twin: Some(String::from("/from-response.md")),
            described_by: None,
        };
        let hint = PageRelations {
            markdown_twin: Some(String::from("/from-hint.md")),
            described_by: Some(String::from("/hint-index.txt")),
        };
        let merged = response.merge(hint);
        assert_eq!(merged.markdown_twin.as_deref(), Some("/from-response.md"));
        assert_eq!(merged.described_by.as_deref(), Some("/hint-index.txt"));
    }
}
