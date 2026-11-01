//! FTS5 behavior on the store API: bm25 weights, snippet, tokenizer, and
//! symbol lookup.
//!
//! Tests seed a migrated temp database with raw SQL so the FTS5 semantics
//! are exercised without the publish path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::SourceId;
use devdocs_store::{Store, build_match_query};
use rusqlite::Connection;
use tempfile::tempdir;

/// Opens a store on a migrated temp file and seeds one source plus three
/// chunks: a title match for "routing", a body match for "routing", and an
/// unrelated body chunk.
struct Seeded {
    store: Store,
    #[allow(dead_code)]
    conn: Connection,
    title_chunk: i64,
    body_chunk: i64,
    _dir: tempfile::TempDir,
}

fn seeded() -> Seeded {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    let source_id = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
    // Title hit: the FTS title column is section_path + symbol.
    let title_chunk = devdocs_testkit::seed_chunk(
        &conn,
        source_id,
        0,
        "[\"Routing\",\"use_router\"]",
        Some("use_router"),
        "Unrelated body about storage.",
    )
    .unwrap();
    // Body hit only.
    let body_chunk = devdocs_testkit::seed_chunk(
        &conn,
        source_id,
        1,
        "[\"Storage\"]",
        None,
        "The routing hook configures navigation.",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO symbols(symbol, chunk_id, kind) VALUES ('use_router', ?1, 'function')",
        rusqlite::params![title_chunk],
    )
    .unwrap();
    Seeded {
        store,
        conn,
        title_chunk,
        body_chunk,
        _dir: dir,
    }
}

/// A match in the title column outranks a match in the body under the
/// documented weights `bm25(chunks_fts, 5.0, 1.0)`.
///
/// The query term is "routing", whose porter stem "rout" matches the title
/// chunk's breadcrumb ("Routing") and the body chunk's text; porter keeps
/// "router" whole (the m>1 rule blocks the -er suffix), so that spelling
/// would only hit the body chunk.
#[test]
fn title_matches_outrank_body_matches() {
    let s = seeded();
    let expr = build_match_query("routing").into_owned();
    let hits = s.store.search_fts(&expr, 10).unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].chunk_id, s.title_chunk, "title hit first");
    assert_eq!(hits[1].chunk_id, s.body_chunk);
    assert!(
        hits[0].rank.unwrap() < hits[1].rank.unwrap(),
        "lower bm25 first"
    );
}

/// `snippet()` over the body column marks the matched term.
#[test]
fn snippet_marks_the_matched_term() {
    let s = seeded();
    let expr = build_match_query("navigation").into_owned();
    let hits = s.store.search_fts(&expr, 10).unwrap();

    assert_eq!(hits.len(), 1);
    let snippet = hits[0].snippet.clone().unwrap();
    assert!(
        snippet.contains("<mark>"),
        "snippet must mark the match: {snippet}"
    );
}

/// The tokenizer keeps `use_router` and `use-route` whole, and porter
/// stemming bridges inflected prose.
#[test]
fn tokenizer_keeps_code_tokens_whole() {
    let s = seeded();
    devdocs_testkit::seed_chunk(
        &s.conn,
        1,
        2,
        "[\"Hooks\"]",
        Some("use-route"),
        "Composes with use-route helpers and routes requests.",
    )
    .unwrap();

    // Both spellings stay whole tokens and match their own symbols.
    let hyphen = build_match_query("use-route").into_owned();
    let hits = s.store.search_fts(&hyphen, 10).unwrap();
    assert!(
        hits.iter()
            .any(|hit| hit.symbol.as_deref() == Some("use-route")),
        "use-route must stay a whole token"
    );
    let underscore = build_match_query("use_router").into_owned();
    let hits = s.store.search_fts(&underscore, 10).unwrap();
    assert!(
        hits.iter().any(|hit| hit.chunk_id == s.title_chunk),
        "use_router must stay a whole token"
    );

    // Porter stemming: "routing" and "routes" share the stem "rout".
    let stem = build_match_query("routing").into_owned();
    let hits = s.store.search_fts(&stem, 10).unwrap();
    assert!(
        hits.iter().any(|hit| hit.text.contains("routes")),
        "porter stems must bridge routing/routes"
    );
}

/// Inactive sources drop out of search results.
#[test]
fn inactive_sources_stay_out_of_results() {
    let s = seeded();
    s.conn
        .execute("UPDATE sources SET active = 0 WHERE source_id = 1", [])
        .unwrap();
    let expr = build_match_query("router").into_owned();
    let hits = s.store.search_fts(&expr, 10).unwrap();
    assert!(hits.is_empty(), "an inactive source must not surface");
}

/// `lookup_symbol` returns the chunk a symbol maps to, with its kind.
#[test]
fn lookup_symbol_finds_the_mapped_chunk() {
    let s = seeded();
    let hits = s.store.lookup_symbol("use_router", SourceId(1)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, s.title_chunk);
    assert_eq!(hits[0].symbol_kind.as_deref(), Some("function"));
    assert_eq!(hits[0].rank, None);
}

/// `lookup_symbol` on a foreign source id returns nothing.
#[test]
fn lookup_symbol_is_scoped_to_the_source() {
    let s = seeded();
    let hits = s.store.lookup_symbol("use_router", SourceId(999)).unwrap();
    assert_eq!(hits, Vec::<devdocs_store::Hit>::new());
}
