//! The `docs://` ref grammar (docs/implementation/14 §Golden-set format):
//! parsing shared by the eval harness and the retrieval open path.

/// A parsed `docs://` ref: `docs://<library>/v<version>/<path>[#<fragment>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRef {
    /// The library authority.
    pub library: String,
    /// The pinned version in the ref, without the leading `v`.
    pub version: String,
    /// The non-empty path segments, matched against chunk section paths at
    /// open time.
    pub segments: Vec<String>,
    /// The fragment after `#`, when present.
    pub fragment: Option<String>,
}

impl ParsedRef {
    /// Parses the ref grammar; `None` for any broken shape.
    ///
    /// The path is one or more `/`-separated segments without `..`, `?`,
    /// or whitespace; a fragment must be non-empty without whitespace.
    #[must_use]
    pub fn parse(r: &str) -> Option<ParsedRef> {
        let rest = r.strip_prefix("docs://")?;
        let (library, tail) = rest.split_once('/')?;
        if library.is_empty() {
            return None;
        }
        let (path, fragment) = match tail.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (tail, None),
        };
        let (version_segment, segment_text) = path.split_once('/')?;
        let version = version_segment.strip_prefix('v')?;
        if version.is_empty() {
            return None;
        }
        let segments: Vec<String> = segment_text.split('/').map(str::to_owned).collect();
        if segments.is_empty()
            || segments.iter().any(|s| {
                s.is_empty() || s == ".." || s.contains('?') || s.contains(char::is_whitespace)
            })
        {
            return None;
        }
        if fragment.is_some_and(|f| f.is_empty() || f.contains(char::is_whitespace)) {
            return None;
        }
        Some(ParsedRef {
            library: library.to_owned(),
            version: version.to_owned(),
            segments,
            fragment: fragment.map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::ParsedRef;

    /// Well-formed refs parse into their parts.
    #[test]
    fn well_formed_refs_parse() {
        let parsed = ParsedRef::parse("docs://next.js/v16.2.9/app/proxy#rewrite").unwrap();
        assert_eq!(parsed.library, "next.js");
        assert_eq!(parsed.version, "16.2.9");
        assert_eq!(parsed.segments, vec!["app", "proxy"]);
        assert_eq!(parsed.fragment.as_deref(), Some("rewrite"));

        let bare = ParsedRef::parse("docs://zod/v4.6.7/parse").unwrap();
        assert_eq!(bare.segments, vec!["parse"]);
        assert_eq!(bare.fragment, None);
    }

    /// Broken shapes do not parse.
    #[test]
    fn broken_shapes_do_not_parse() {
        let cases = [
            "https://next.js/v16.2.9/proxy",
            "next.js/v16.2.9/proxy",
            "docs://next.js/proxy",
            "docs://next.js/v16.2.9",
            "docs://next.js/v16.2.9/",
            "docs://next.js/16.2.9/proxy",
            "docs:///v1.0.0/path",
            "docs://next.js/v/path",
            "docs://next.js/v16.2.9/../etc",
            "docs://next.js/v16.2.9/a//b",
            "docs://next.js/v16.2.9/a b",
            "docs://next.js/v16.2.9/a?b",
            "docs://next.js/v16.2.9/proxy#",
            "docs://next.js/v16.2.9/proxy#a b",
        ];
        for r in cases {
            assert!(ParsedRef::parse(r).is_none(), "{r} must not parse");
        }
    }
}
