//! The rung ladder (docs/implementation/08 §Rungs).
//!
//! The v1 tool implements rungs 1 and 2 in-process and refuses past them
//! (docs/implementation/08 §Fallback ladder). The symbol rung returns
//! empty until symbol extraction lands at M5; the FTS5 rung is live.

use devdocs_store::Hit;
use devdocs_store::Store;

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

/// The symbol-exact rung. Returns empty until symbol extraction lands at
/// M5 (docs/implementation/13 §M4: "symbol rung returns empty for now").
pub fn symbol_rung(_store: &Store, _scope: Scope<'_>) -> Vec<Hit> {
    Vec::new()
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

    use super::{Scope, fts_rung, symbol_rung};
    use crate::error::RetrieveError;

    /// The symbol rung returns empty until M5.
    #[test]
    fn symbol_rung_returns_empty_until_m5() {
        let dir = tempfile::tempdir().unwrap();
        let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
        assert_eq!(symbol_rung(&store, Scope::All), Vec::new());
    }

    /// The FTS rung runs the prefix expression against a live store.
    #[test]
    fn fts_rung_searches_through_the_store() {
        use rusqlite::Connection;

        let dir = tempfile::tempdir().unwrap();
        let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
        let conn = Connection::open(dir.path().join("docs.db")).unwrap();
        let source = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        devdocs_testkit::seed_chunk(
            &conn,
            source,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer maps requests.",
        )
        .unwrap();

        let hits = fts_rung(&store, "rout", Scope::All, 32)
            .map_err(|e| match e {
                RetrieveError::Store(e) => e,
                other => panic!("unexpected retrieval error: {other}"),
            })
            .unwrap();
        // The prefix star on "rout" matches "routing".
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].section_path, vec!["Routing"]);
    }

    /// An empty query text produces the no-match expression: a valid query
    /// that matches nothing, not an error.
    #[test]
    fn empty_query_matches_nothing_without_error() {
        use rusqlite::Connection;

        let dir = tempfile::tempdir().unwrap();
        let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
        let conn = Connection::open(dir.path().join("docs.db")).unwrap();
        let source = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        devdocs_testkit::seed_chunk(
            &conn,
            source,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer maps requests.",
        )
        .unwrap();

        let hits = fts_rung(&store, "", Scope::All, 32)
            .map_err(|e| match e {
                RetrieveError::Store(e) => e,
                other => panic!("unexpected retrieval error: {other}"),
            })
            .unwrap();
        assert_eq!(hits, Vec::new());
    }
}
