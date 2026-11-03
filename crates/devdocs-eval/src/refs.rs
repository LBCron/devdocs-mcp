//! The `docs://` ref grammar (docs/implementation/14 §Golden-set format):
//! parsing and pin validation.

use crate::suite::TaskType;

/// A parsed `docs://` ref: `docs://<library>/v<version>/<path>[#<fragment>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRef {
    /// The library authority.
    pub library: String,
    /// The pinned version, without the leading `v`.
    pub version: String,
    /// The non-empty path segments.
    pub segments: Vec<String>,
    /// The fragment after `#`, when present.
    pub fragment: Option<String>,
}

impl ParsedRef {
    /// Parses the ref grammar; `None` for any broken shape.
    ///
    /// The path is one or more `/`-separated segments
    /// without `..`, `?`, or whitespace; a fragment must be non-empty
    /// without whitespace.
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

/// Rejects versions that are not an exact pin (docs/implementation/14:
/// "Version is exact. A question without a pinned version is invalid input
/// for the harness.").
pub(crate) fn ensure_pinned_version(version: &str) -> Result<(), String> {
    let bad = |why: &str| format!("version {version:?} is not an exact pin: {why}");
    match version.chars().next() {
        None => return Err(bad("empty")),
        Some('v' | 'V') => {
            return Err(bad(
                "drop the leading v; the pin is bare and refs carry the v",
            ));
        }
        Some('^' | '~' | '>' | '<' | '=' | '*') => {
            return Err(bad("a range selector pins nothing"));
        }
        _ => {}
    }
    if version.eq_ignore_ascii_case("latest") {
        return Err(bad("'latest' pins nothing"));
    }
    if version.chars().any(char::is_whitespace) {
        return Err(bad("whitespace"));
    }
    // Only the characters a SemVer 2.0.0 version can carry; anything else
    // (a range selector, a comparison) is not an exact pin.
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return Err(bad("only letters, digits, '.', '-', and '+' are allowed"));
    }
    for component in version.split('.') {
        if component.is_empty() {
            return Err(bad("empty component"));
        }
        if component == "*" || component.eq_ignore_ascii_case("x") {
            return Err(bad("a wildcard component pins nothing"));
        }
    }
    Ok(())
}

/// Validates one `docs://` ref against the question's pin.
///
/// `multi_library` questions may pin refs for other libraries; those refs
/// pin their own version.
pub(crate) fn validate_ref(
    r: &str,
    library: &str,
    version: &str,
    task_type: TaskType,
) -> Result<(), String> {
    let bad = |why: String| format!("malformed ref {r:?}: {why}");
    let parsed = ParsedRef::parse(r).ok_or_else(|| {
        bad("the shape is not docs://<library>/v<version>/<path>[#<fragment>]".to_owned())
    })?;
    // A `multi_library` ref may pin another library's version. A ref for
    // the question's own library — in any task type — still pins the
    // question version.
    if task_type != TaskType::MultiLibrary || parsed.library.eq_ignore_ascii_case(library) {
        if !parsed.library.eq_ignore_ascii_case(library) {
            return Err(bad(format!(
                "library {:?} does not match the question library {library:?}",
                parsed.library
            )));
        }
        if parsed.version != version {
            return Err(bad(format!(
                "pins version {:?}, question pins {version:?}",
                parsed.version
            )));
        }
    }
    // Every ref version is still an exact pin.
    ensure_pinned_version(&parsed.version).map_err(bad)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{ParsedRef, TaskType, ensure_pinned_version, validate_ref};

    #[test]
    fn versions_must_be_exact_pins() {
        for bad in [
            "^1.2.3",
            "~1.2",
            ">=1.0",
            "1.x",
            "1.2.*",
            "latest",
            "v16.2.9",
            "",
            "16.2.9 ",
            "16..9",
            "1.2.3||2.0.0",
            "1.2.3<2.0.0",
            "1.2.3!=2.0.0",
            "16.2.9~1",
        ] {
            let err = ensure_pinned_version(bad).unwrap_err();
            assert!(err.contains("not an exact pin"), "{bad}: {err}");
        }
        for good in [
            "16.2.9",
            "4.6.7",
            "1.0.229",
            "4.0.0-beta.1",
            "16",
            "4.0.0+build.7",
        ] {
            ensure_pinned_version(good).unwrap();
        }
    }

    #[test]
    fn malformed_refs_are_rejected() {
        let cases = [
            "https://next.js/v16.2.9/proxy",
            "next.js/v16.2.9/proxy",
            "docs://next.js/proxy",
            "docs://next.js/v16.2.9",
            "docs://next.js/v16.2.9/",
            "docs://next.js/v16.3.0/proxy",
            "docs://next.js/16.2.9/proxy",
            "docs://zod/v4.6.7/parse",
            "docs://next.js/v16.2.9/../etc",
            "docs://next.js/v16.2.9/a//b",
            "docs://next.js/v16.2.9/a b",
            "docs://next.js/v16.2.9/a?b",
            "docs://next.js/v16.2.9/proxy#",
            "docs://next.js/v16.2.9/proxy#a b",
        ];
        for r in cases {
            let err = validate_ref(r, "next.js", "16.2.9", TaskType::ExactApi).unwrap_err();
            assert!(err.contains("malformed ref"), "{r}: {err}");
        }
    }

    #[test]
    fn well_formed_refs_validate() {
        for (r, library, version) in [
            ("docs://next.js/v16.2.9/proxy", "next.js", "16.2.9"),
            ("docs://next.js/v16.2.9/proxy#rewrite", "next.js", "16.2.9"),
            (
                "docs://next.js/v16.2.9/app/api-reference#use-cache",
                "next.js",
                "16.2.9",
            ),
            ("docs://zod/v4.6.7/parse", "zod", "4.6.7"),
        ] {
            validate_ref(r, library, version, TaskType::ExactApi)
                .unwrap_or_else(|e| panic!("{r}: {e}"));
        }
    }

    #[test]
    fn multi_library_questions_accept_other_libraries() {
        validate_ref(
            "docs://serde/v1.0.229/derive",
            "next.js",
            "16.2.9",
            TaskType::MultiLibrary,
        )
        .unwrap();
        let err = validate_ref(
            "docs://serde/v1.0.229/derive",
            "next.js",
            "16.2.9",
            TaskType::ExactApi,
        )
        .unwrap_err();
        assert!(err.contains("does not match the question library"));
    }

    #[test]
    fn multi_library_refs_on_the_own_library_pin_the_question_version() {
        validate_ref(
            "docs://next.js/v16.2.9/proxy",
            "next.js",
            "16.2.9",
            TaskType::MultiLibrary,
        )
        .unwrap();
        let err = validate_ref(
            "docs://next.js/v15.0.0/proxy",
            "next.js",
            "16.2.9",
            TaskType::MultiLibrary,
        )
        .unwrap_err();
        assert!(err.contains("pins version"), "{err}");
    }

    #[test]
    fn parse_yields_the_ref_parts() {
        let parsed = ParsedRef::parse("docs://next.js/v16.2.9/app/proxy#rewrite").unwrap();
        assert_eq!(parsed.library, "next.js");
        assert_eq!(parsed.version, "16.2.9");
        assert_eq!(parsed.segments, vec!["app", "proxy"]);
        assert_eq!(parsed.fragment.as_deref(), Some("rewrite"));
    }

    #[test]
    fn an_empty_authority_or_version_does_not_parse() {
        assert!(
            ParsedRef::parse("docs:///v1.0.0/path").is_none(),
            "an empty library authority parses to nothing"
        );
        assert!(
            ParsedRef::parse("docs://next.js/v/path").is_none(),
            "an empty version parses to nothing"
        );
    }

    proptest::proptest! {
        #[test]
        fn well_formed_refs_always_validate(
            library in "[a-z][a-z0-9.]{2,15}",
            version in r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}",
            segment in "[a-z][a-z0-9-]{0,15}",
            fragment in "[a-z][a-z0-9-]{0,15}",
            depth in 1usize..4,
        ) {
            let path = std::iter::repeat_n(segment.as_str(), depth).collect::<Vec<_>>().join("/");
            let r = format!("docs://{library}/v{version}/{path}#{fragment}");
            super::validate_ref(&r, &library, &version, super::TaskType::ExactApi)
                .unwrap_or_else(|e| panic!("{r}: {e}"));
        }
    }
}
