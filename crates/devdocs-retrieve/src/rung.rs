//! The rung ladder (docs/implementation/08 §Rungs).
//!
//! The v1 tool implements rungs 1 and 2 in-process and refuses past them
//! (docs/implementation/08 §Fallback ladder). The symbol-exact rung runs
//! the store's symbol ladder over the normalized identifier; the FTS5
//! rung is live.

use devdocs_store::Hit;
use devdocs_store::Store;
use devdocs_store::SymbolMatch;
use devdocs_store::SymbolScope;

use crate::error::RetrieveError;
use crate::matchexpr::rung_match_expr;

/// The scope a rung executes against. An exact-version request narrows to
/// one source; an unresolved library keeps the library scope; the fallback
/// runs unfiltered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// All active sources — the unfiltered fallback.
    All,
    /// One library, every active version.
    Library(&'a str),
    /// One exact source — the requested version was resolved to a pinned
    /// source id.
    Source(i64),
}

/// The close/open byte pair of one bracket group, when `close` closes one.
fn group_delimiters(close: u8) -> Option<(u8, u8)> {
    match close {
        b')' => Some((b')', b'(')),
        b']' => Some((b']', b'[')),
        b'>' => Some((b'>', b'<')),
        _ => None,
    }
}

/// Strips one trailing balanced bracket group from `text`: the final `)`,
/// `]`, or `>` together with its matching opener, when the scan between
/// them stays balanced. `None` when the text does not end in a balanced
/// group.
#[must_use]
fn strip_trailing_group(text: &str) -> Option<&str> {
    let (close, open) = group_delimiters(*text.as_bytes().last()?)?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (index, &byte) in bytes.iter().enumerate().rev() {
        if byte == close {
            depth += 1;
        } else if byte == open {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(&text[..index]);
            }
        }
    }
    None
}

/// Normalizes a symbol query for the exact rung
/// (docs/implementation/13 §M5 step 3): trim, then strip trailing balanced
/// call/index/generic groups — `()` / a balanced `(...)` pair, a trailing
/// `[]` pair, a trailing `<>` pair — repeating while one is present. Dots
/// stay: `obj.method()` normalizes to `obj.method`.
#[must_use]
pub fn normalize_symbol(query: &str) -> String {
    let mut current = query.trim().to_owned();
    while let Some(stripped) = strip_trailing_group(&current) {
        current = stripped.trim_end().to_owned();
    }
    current
}

/// The symbol-exact rung (docs/implementation/08 §Retrieval ladder, rung
/// 1): normalize the query, then run the store's lookup ladder — exact,
/// the last dot-segment as a secondary exact attempt, case-sensitive
/// prefix, case-insensitive — returning the first non-empty rank list.
/// `limit` is the fusion headroom, the same 4× shape the FTS rung applies.
///
/// # Errors
/// [`RetrieveError::Store`] when the store query fails.
pub fn symbol_rung(
    store: &Store,
    query: &str,
    scope: Scope<'_>,
    limit: u32,
) -> Result<Vec<Hit>, RetrieveError> {
    let normalized = normalize_symbol(query);
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let store_scope = store_scope(scope);
    for (mode, candidate) in ladder_attempts(&normalized) {
        let hits = store.search_symbols(&candidate, mode, store_scope.clone(), limit)?;
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(Vec::new())
}

/// The store scope a rung scope lowers to.
fn store_scope(scope: Scope<'_>) -> SymbolScope {
    match scope {
        Scope::All => SymbolScope::All,
        Scope::Library(name) => SymbolScope::Library(name.to_owned()),
        Scope::Source(id) => SymbolScope::Source(id),
    }
}

/// The lookup ladder, strongest shape first. A dotted form also tries its
/// last dot-segment as a secondary exact attempt
/// (docs/implementation/13 §M5 step 3) before the prefix modes fall
/// through.
fn ladder_attempts(normalized: &str) -> Vec<(SymbolMatch, String)> {
    let mut attempts: Vec<(SymbolMatch, String)> =
        vec![(SymbolMatch::Exact, normalized.to_owned())];
    if let Some((_, last)) = normalized.rsplit_once('.')
        && !last.is_empty()
    {
        attempts.push((SymbolMatch::Exact, last.to_owned()));
    }
    attempts.push((SymbolMatch::CaseSensitivePrefix, normalized.to_owned()));
    attempts.push((SymbolMatch::CaseInsensitive, normalized.to_owned()));
    attempts
}

/// The FTS5 BM25 rung: builds the rung MATCH expression from the raw query
/// text and searches the scope with the documented headroom limit.
///
/// `limit` is the fusion headroom (docs/08: `LIMIT` at 4× the requested
/// hits), applied here so rung results stay comparable before fusion.
///
/// # Errors
/// [`RetrieveError::Store`] when the store query fails.
pub fn fts_rung(
    store: &Store,
    query_text: &str,
    scope: Scope<'_>,
    limit: u32,
) -> Result<Vec<Hit>, RetrieveError> {
    let expr = rung_match_expr(query_text);
    let hits = match scope {
        Scope::All => store.search_fts(&expr, limit)?,
        Scope::Library(name) => store.search_fts_library(&expr, name, limit)?,
        Scope::Source(id) => store.search_fts_source(&expr, id, limit)?,
    };
    Ok(hits)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use devdocs_model::{LicenseStatus, SourceId, SourceKind, SourceRecord};
    use devdocs_store::{NewChunk, Symbol, content_hash};
    use tempfile::TempDir;
    use time::OffsetDateTime;
    use url::Url;

    use super::{Scope, fts_rung, normalize_symbol, symbol_rung};
    use crate::fusion::fuse;

    /// Normalization strips the trailing call group and keeps dots.
    #[test]
    fn normalization_strips_call_suffixes_and_keeps_dots() {
        assert_eq!(normalize_symbol("useRouter()"), "useRouter");
        assert_eq!(normalize_symbol("  useRouter( )  "), "useRouter");
        assert_eq!(normalize_symbol("obj.method()"), "obj.method");
        assert_eq!(normalize_symbol("obj.method(1, 2)"), "obj.method");
    }

    /// Normalization strips trailing balanced index and generic groups,
    /// repeatedly.
    #[test]
    fn normalization_strips_index_and_generic_groups() {
        assert_eq!(normalize_symbol("arr[]"), "arr");
        assert_eq!(normalize_symbol("map[0]"), "map");
        assert_eq!(normalize_symbol("Vec<T>"), "Vec");
        assert_eq!(normalize_symbol("Vec<Vec<T>>"), "Vec");
        assert_eq!(normalize_symbol("run()()"), "run");
    }

    /// Unbalanced or non-suffixed input passes through unchanged.
    #[test]
    fn normalization_leaves_unmatched_text_alone() {
        assert_eq!(normalize_symbol("useRouter"), "useRouter");
        assert_eq!(normalize_symbol("foo)(bar"), "foo)(bar");
        assert_eq!(normalize_symbol("(only-open"), "(only-open");
        assert_eq!(normalize_symbol("   "), "");
        // A trailing group strips even when the remaining head is
        // unbalanced — the rule is suffix-local and deterministic.
        assert_eq!(normalize_symbol("foo(a[b]"), "foo(a");
    }

    /// Seeds one published source with the given `(symbol, text)` chunks.
    /// The store is dropped, so the seeding connection never leaks.
    fn symbol_store(
        dir: &TempDir,
        library: &str,
        version: &str,
        chunks: &[(&str, &str)],
    ) -> devdocs_store::Store {
        let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
        let record = SourceRecord {
            id: SourceId(0),
            library: library.to_owned(),
            version: version.to_owned(),
            kind: SourceKind::LlmsTxt,
            origin_url: Url::parse("https://example.test/symbols").unwrap(),
            commit_sha: Some("abc123".to_owned()),
            license: Some("MIT".to_owned()),
            license_status: LicenseStatus::ApprovedIndex,
            fetched_at: OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap(),
        };
        let new_chunks: Vec<NewChunk> = chunks
            .iter()
            .enumerate()
            .map(|(ordinal, (symbol, text))| NewChunk {
                section_path: vec!["Guide".to_owned()],
                symbols: vec![Symbol {
                    name: (*symbol).to_owned(),
                    kind: String::from("function"),
                }],
                symbol: Some(Symbol {
                    name: (*symbol).to_owned(),
                    kind: String::from("function"),
                }),
                text: format!("## Guide\n\n{text}"),
                token_count: 16,
                content_hash: content_hash(text),
                ordinal: u32::try_from(ordinal).unwrap_or(0),
            })
            .collect();
        store.publish_generation(&record, &new_chunks).unwrap();
        store
    }

    /// A query that normalizes to nothing serves nothing: the rung returns
    /// before any store query.
    #[test]
    fn symbol_rung_serves_nothing_for_an_empty_normalized_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(&dir, "next.js", "16.2.9", &[("useRouter", "hook")]);
        let hits = symbol_rung(&store, "()", Scope::All, 32).unwrap();
        assert_eq!(hits, [] as [devdocs_store::Hit; 0]);
    }

    /// The rung finds an exact symbol through the store.
    #[test]
    fn symbol_rung_matches_exact_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[("useRouter", "The useRouter hook configures routing.")],
        );
        let hits = symbol_rung(&store, "useRouter()", Scope::All, 32).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.as_deref(), Some("useRouter"));
        assert_eq!(hits[0].symbol_kind.as_deref(), Some("function"));
    }

    /// Ladder precedence: exact wins over the prefix modes — a query that
    /// exactly names one symbol does not drag the longer siblings in.
    #[test]
    fn symbol_rung_prefers_exact_over_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[
                ("useRouter", "exact hit"),
                ("useRouterQuery", "prefix sibling"),
            ],
        );
        let hits = symbol_rung(&store, "useRouter()", Scope::All, 32).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.as_deref(), Some("useRouter"));
    }

    /// The case-sensitive prefix rung: a partial query matches the
    /// completion shape, case-sensitively.
    #[test]
    fn symbol_rung_matches_case_sensitive_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[("useRouter", "hook"), ("userouter", "lowercase twin")],
        );
        let hits = symbol_rung(&store, "useR", Scope::All, 32).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.as_deref(), Some("useRouter"));
    }

    /// The case-insensitive rung is the last resort: an all-caps query
    /// that no earlier rung matched still finds the symbol, exact first.
    #[test]
    fn symbol_rung_falls_through_to_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[("useRouter", "hook"), ("useRouterQuery", "sibling")],
        );
        let hits = symbol_rung(&store, "USEROUTER", Scope::All, 32).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].symbol.as_deref(), Some("useRouter"));
    }

    /// A dotted form falls back to its last dot-segment as a secondary
    /// exact attempt before the prefix modes.
    #[test]
    fn symbol_rung_tries_the_last_dot_segment() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[("push", "the standalone push helper")],
        );
        let hits = symbol_rung(&store, "router.push()", Scope::All, 32).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.as_deref(), Some("push"));
    }

    /// The rung narrows to the library scope.
    #[test]
    fn symbol_rung_scopes_to_one_library() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(&dir, "next.js", "16.2.9", &[("useRouter", "next hit")]);
        let other = symbol_store(&dir, "zod", "4.6.7", &[("useRouter", "zod hit")]);
        drop(other);
        let hits = symbol_rung(&store, "useRouter", Scope::Library("zod"), 32).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].library, "zod");
    }

    /// The symbol rung's hits fuse ahead of the FTS-only hits, and the
    /// rung is deterministic: identical inputs give identical order.
    #[test]
    fn symbol_hits_fuse_ahead_of_fts_and_stay_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let store = symbol_store(
            &dir,
            "next.js",
            "16.2.9",
            &[
                ("useRouter", "The useRouter hook configures routing."),
                ("redirect", "Routing redirects rewrite the request."),
            ],
        );
        let once = || {
            let scope = Scope::Library("next.js");
            let symbol = symbol_rung(&store, "useRouter()", scope, 32).unwrap();
            let fts = fts_rung(&store, "routing", scope, 32).unwrap();
            fuse(60, &[symbol, fts])
        };
        let fused = once();
        let again = once();
        assert_eq!(fused, again, "deterministic");
        assert_ne!(fused, [] as [(devdocs_store::Hit, f64); 0]);
        // The exact symbol hit outranks the chunks the FTS rung alone
        // served.
        assert_eq!(fused[0].0.symbol.as_deref(), Some("useRouter"));
    }
}
