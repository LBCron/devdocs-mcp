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
                if params.rel != "alternate" {
                    if params.rel == "describedby" {
                        relations.described_by = Some(uri);
                    }
                    continue;
                }
                if params.type_ == "text/markdown" {
                    relations.markdown_twin = Some(uri);
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
                if rel == "alternate" && type_ == "text/markdown" {
                    relations.markdown_twin = Some(href);
                } else if rel == "describedby" {
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

/// One HTML attribute value from a tag text; the name matches
/// case-insensitively.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pattern = format!("{name}=");
    let mut search = 0usize;
    while let Some(found) = lower[search..].find(&pattern) {
        let at = search + found;
        // Only an attribute position: the start of the tag or whitespace
        // before the name, so `data-href=` never matches `href=`.
        let boundary = at == 0
            || tag[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if !boundary {
            search = at + pattern.len();
            continue;
        }
        let rest = tag[at + pattern.len()..].trim_start();
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"')?;
            return Some(quoted[..end].to_owned());
        }
        if let Some(quoted) = rest.strip_prefix('\'') {
            let end = quoted.find('\'')?;
            return Some(quoted[..end].to_owned());
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        return Some(rest[..end].to_owned());
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
