//! Result item assembly (docs/implementation/08 §Result envelope).
//!
//! Maps a store [`Hit`] onto the envelope's `ResultItem`, deriving the
//! `docs://` ref from the chunk identity so `search` output feeds `open`
//! input without a second lookup.

use devdocs_model::VersionStatus;
use devdocs_model::docref::ParsedRef;
use devdocs_model::envelope::ResultItem;
use devdocs_store::Hit;

/// Maps a store hit onto the envelope result item.
///
/// `confidence` is the fusion score; `status` comes from the version
/// policy. The ref derives from the library, version, slugified section
/// path, and the slugified symbol as fragment (absent without a symbol).
#[must_use]
pub fn to_result_item(
    hit: &Hit,
    requested: Option<&str>,
    status: VersionStatus,
    confidence: f64,
) -> ResultItem {
    ResultItem {
        doc_ref: derive_ref(hit),
        title: hit.section_path.last().unwrap_or(&hit.library).clone(),
        section: hit.section_path.join(" > "),
        snippet: hit.snippet.clone().unwrap_or_else(|| hit.text.clone()),
        examples: Vec::new(),
        source_url: hit.source_url.clone(),
        source_commit: hit.source_commit.clone().unwrap_or_default(),
        requested_version: requested.map(str::to_owned),
        served_version: hit.version.clone(),
        version_status: status,
        content_hash: hex(&hit.content_hash),
        license_status: hit.license_status.clone(),
        confidence,
        token_count: u64::from(hit.token_count),
    }
}

/// Derives the `docs://` ref for one chunk: the library, the `v`-prefixed
/// version, the slugified section path, and the slugified symbol as the
/// fragment when one applies.
#[must_use]
pub fn derive_ref(hit: &Hit) -> String {
    let segments: Vec<String> = hit.section_path.iter().map(|s| slug(s)).collect();
    let mut ref_ = format!(
        "docs://{}/v{}/{}",
        hit.library,
        hit.version,
        segments.join("/")
    );
    if let Some(symbol) = &hit.symbol {
        ref_.push('#');
        ref_.push_str(&slug(symbol));
    }
    ref_
}

/// Slugifies one ref path or fragment segment: lowercase, runs of
/// non-alphanumeric characters fold to one `-`, ends trimmed.
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_dash = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Lowercase hex of a sha256 digest.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [HEX[usize::from(b >> 4)], HEX[usize::from(b & 0x0f)]])
        .collect::<Vec<u8>>()
        .into_iter()
        .map(char::from)
        .collect()
}

/// Finds the chunk that serves a parsed ref: the full slugified section
/// path first, then the last segment, then the fragment (symbol or title
/// match). `None` when nothing matches.
#[must_use]
pub fn match_ref<'a>(chunks: &'a [Hit], parsed: &ParsedRef) -> Option<&'a Hit> {
    let want: Vec<String> = parsed.segments.iter().map(|s| slug(s)).collect();
    // 1. The full section path matches the ref path exactly.
    let full: Vec<&Hit> = chunks
        .iter()
        .filter(|c| {
            c.section_path.len() == want.len()
                && c.section_path
                    .iter()
                    .map(|s| slug(s))
                    .eq(want.iter().cloned())
        })
        .collect();
    if full.len() == 1 {
        return full.first().copied();
    }
    // 2. The fragment names the chunk's symbol when the path is absent
    //    from the section paths.
    if let Some(fragment) = &parsed.fragment {
        let by_symbol: Vec<&Hit> = chunks
            .iter()
            .filter(|c| c.symbol.as_deref().is_some_and(|s| slug(s) == *fragment))
            .collect();
        if by_symbol.len() == 1 {
            return by_symbol.first().copied();
        }
        // 3. The fragment matches the title (the last section segment).
        let by_title: Vec<&Hit> = chunks
            .iter()
            .filter(|c| c.section_path.last().is_some_and(|t| slug(t) == *fragment))
            .collect();
        if by_title.len() == 1 {
            return by_title.first().copied();
        }
    }
    // 4. The last segment matches the chunk's last section segment.
    let last = want.last()?;
    let by_last: Vec<&Hit> = chunks
        .iter()
        .filter(|c| c.section_path.last().is_some_and(|t| slug(t) == *last))
        .collect();
    if by_last.len() == 1 {
        return by_last.first().copied();
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{derive_ref, hex, match_ref, slug, to_result_item};
    use devdocs_model::VersionStatus;
    use devdocs_store::Hit;

    fn hit(library: &str, version: &str, section: &[&str], symbol: Option<&str>) -> Hit {
        let mut hit = devdocs_testkit::sample_hit(1, 42);
        hit.library = library.to_owned();
        hit.version = version.to_owned();
        hit.section_path = section.iter().map(|s| (*s).to_owned()).collect();
        hit.symbol = symbol.map(str::to_owned);
        hit.snippet = Some("the <mark>routing</mark> layer".to_owned());
        hit.text = "text".to_owned();
        hit.content_hash = [0xab; 32];
        hit.source_url = "https://example.test/next".to_owned();
        hit.source_commit = Some("abc123".to_owned());
        hit
    }

    /// The item carries every envelope field, with the hash hex-encoded
    /// and the snippet passed through.
    #[test]
    fn items_carry_the_envelope_fields() {
        let hit = hit(
            "next.js",
            "16.2.9",
            &["Routing", "use_router"],
            Some("use_router"),
        );
        let item = to_result_item(&hit, Some("16.2.9"), VersionStatus::Verified, 0.94);
        assert_eq!(
            item.doc_ref,
            "docs://next.js/v16.2.9/routing/use-router#use-router"
        );
        assert_eq!(item.title, "use_router");
        assert_eq!(item.section, "Routing > use_router");
        assert_eq!(item.snippet, "the <mark>routing</mark> layer");
        assert_eq!(item.requested_version.as_deref(), Some("16.2.9"));
        assert_eq!(item.served_version, "16.2.9");
        assert_eq!(item.version_status, VersionStatus::Verified);
        assert_eq!(item.content_hash, "ab".repeat(32));
        assert_eq!(item.license_status, "approved_index");
        assert_eq!((item.confidence, item.token_count), (0.94, 42));
    }

    /// Ref derivation slugifies the path and the symbol.
    #[test]
    fn refs_derive_from_the_chunk_identity() {
        let with_symbol = hit("next.js", "16.2.9", &["Routing"], Some("use_router"));
        assert_eq!(
            derive_ref(&with_symbol),
            "docs://next.js/v16.2.9/routing#use-router"
        );

        let no_symbol = hit("zod", "4.6.7", &["Parse"], None);
        assert_eq!(derive_ref(&no_symbol), "docs://zod/v4.6.7/parse");
    }

    /// The slug rule folds punctuation runs and trims the ends.
    #[test]
    fn slugs_fold_punctuation() {
        assert_eq!(slug("Proxy Rewrite"), "proxy-rewrite");
        assert_eq!(slug("--use_cache (v2)--"), "use-cache-v2");
        assert_eq!(slug("###"), "");
        assert_eq!(slug("Rust 1.90"), "rust-1-90");
    }

    /// Ref matching finds the chunk by full path, then fragment, then
    /// last segment.
    #[test]
    fn refs_match_by_path_then_fragment() {
        let mut first = hit(
            "next.js",
            "16.2.9",
            &["Routing", "use_router"],
            Some("use_router"),
        );
        first.chunk_id = 1;
        let mut second = hit("next.js", "16.2.9", &["Storage"], None);
        second.chunk_id = 2;
        let chunks = vec![first, second];
        let parsed = devdocs_model::docref::ParsedRef::parse(
            "docs://next.js/v16.2.9/routing/use-router#use-router",
        )
        .unwrap();
        assert_eq!(match_ref(&chunks, &parsed).map(|h| h.chunk_id), Some(1));

        let by_last =
            devdocs_model::docref::ParsedRef::parse("docs://next.js/v16.2.9/storage").unwrap();
        assert_eq!(match_ref(&chunks, &by_last).map(|h| h.chunk_id), Some(2));

        let nothing =
            devdocs_model::docref::ParsedRef::parse("docs://next.js/v16.2.9/missing").unwrap();
        assert_eq!(match_ref(&chunks, &nothing), None);
    }

    /// Hex encoding is lowercase and two digits per byte.
    #[test]
    fn hex_encodes_lowercase() {
        assert_eq!(hex(&[0x0f, 0xab]), "0fab");
    }
}
