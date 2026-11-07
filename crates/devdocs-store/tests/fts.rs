//! FTS5 behavior on the store API: bm25 weights, snippet, tokenizer, and
//! symbol lookup.
//!
//! Tests seed a migrated temp database with raw SQL so the FTS5 semantics
//! are exercised without the publish path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::SourceId;
use devdocs_store::{Store, SymbolMatch, SymbolScope, build_match_query};
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

/// One symbol row on a seeded chunk.
fn symbol(conn: &Connection, chunk_id: i64, name: &str, kind: &str) {
    conn.execute(
        "INSERT INTO symbols(symbol, chunk_id, kind) VALUES (?1, ?2, ?3)",
        rusqlite::params![name, chunk_id, kind],
    )
    .unwrap();
}

/// The ladder fixture: two sources, several chunks per source, and symbol
/// rows that exercise every mode, scope, the limit, and the tie order:
///
/// - source 1 `next.js` (active): chunk A carries `use_router` and
///   `useRoute`, chunk B carries `use_router` and `use_router_polyfill`.
/// - source 2 `vue` (active): chunk C carries `use_router`.
struct Ladder {
    store: Store,
    conn: Connection,
    next_source: i64,
    vue_source: i64,
    chunk_a: i64,
    chunk_b: i64,
    vue_chunk: i64,
    _dir: tempfile::TempDir,
}

fn ladder() -> Ladder {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    let next_source = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
    let vue_source = devdocs_testkit::seed_source(&conn, "vue", true).unwrap();
    let chunk_a = devdocs_testkit::seed_chunk(
        &conn,
        next_source,
        0,
        "[\"Routing\"]",
        None,
        "Router hooks.",
    )
    .unwrap();
    let chunk_b = devdocs_testkit::seed_chunk(
        &conn,
        next_source,
        1,
        "[\"Routing\"]",
        None,
        "Router polyfills.",
    )
    .unwrap();
    let vue_chunk = devdocs_testkit::seed_chunk(
        &conn,
        vue_source,
        0,
        "[\"Routing\"]",
        None,
        "Vue router hooks.",
    )
    .unwrap();
    symbol(&conn, chunk_a, "use_router", "function");
    symbol(&conn, chunk_a, "useRoute", "function");
    symbol(&conn, chunk_b, "use_router", "function");
    symbol(&conn, chunk_b, "use_router_polyfill", "function");
    symbol(&conn, vue_chunk, "use_router", "function");
    Ladder {
        store,
        conn,
        next_source,
        vue_source,
        chunk_a,
        chunk_b,
        vue_chunk,
        _dir: dir,
    }
}

/// The chunk ids of a hit list.
fn ids(hits: &[devdocs_store::Hit]) -> Vec<i64> {
    hits.iter().map(|hit| hit.chunk_id).collect()
}

/// The exact mode returns the whole-identifier matches of every active
/// source, ties in the lower chunk id.
#[test]
fn symbol_exact_matches_all_sources_in_chunk_id_order() {
    let s = ladder();
    let hits = s
        .store
        .search_symbols("use_router", SymbolMatch::Exact, SymbolScope::All, 10)
        .unwrap();
    assert_eq!(ids(&hits), vec![s.chunk_a, s.chunk_b, s.vue_chunk]);
    assert_eq!(hits[0].symbol_kind.as_deref(), Some("function"));
}

/// The case-sensitive prefix does not fold case: `useR` matches
/// `useRoute`, never the stored `use_router` rows.
#[test]
fn symbol_case_sensitive_prefix_respects_case() {
    let s = ladder();
    let hits = s
        .store
        .search_symbols(
            "useR",
            SymbolMatch::CaseSensitivePrefix,
            SymbolScope::All,
            10,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![s.chunk_a], "useRoute only");
}

/// The case-insensitive mode falls back across case: `USE_ROUTER` matches
/// the stored `use_router` rows; prefix matches of the same head trail the
/// exact ones, ties in the lower chunk id. A chunk matched by exact and
/// prefix rows surfaces once — the exact row wins the per-chunk pick.
#[test]
fn symbol_case_insensitive_fallback_matches() {
    let s = ladder();
    let hits = s
        .store
        .search_symbols(
            "USE_ROUTER",
            SymbolMatch::CaseInsensitive,
            SymbolScope::All,
            10,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![s.chunk_a, s.chunk_b, s.vue_chunk]);
    assert_eq!(
        hits[1].symbol.as_deref(),
        Some("use_router"),
        "the exact row wins chunk_b's pick"
    );

    // A prefix query in the same mode matches every identifier head, with
    // exact matches ranking before prefixes and one hit per chunk.
    let hits = s
        .store
        .search_symbols(
            "use_router",
            SymbolMatch::CaseInsensitive,
            SymbolScope::All,
            10,
        )
        .unwrap();
    assert_eq!(
        ids(&hits),
        vec![s.chunk_a, s.chunk_b, s.vue_chunk],
        "use_router_polyfill dedupes into chunk_b's exact row"
    );
}

/// A secondary symbol reports itself: looking up `plan` on a chunk whose
/// primary symbol is `Planner` returns `Hit::symbol = "plan"` and `plan`'s
/// kind, not the chunk's primary symbol.
#[test]
fn symbol_lookups_report_the_matched_symbol() {
    let s = ladder();
    let chunk = devdocs_testkit::seed_chunk(
        &s.conn,
        s.next_source,
        2,
        "[\"Planning\"]",
        Some("Planner"),
        "Plans things.",
    )
    .unwrap();
    symbol(&s.conn, chunk, "Planner", "struct");
    symbol(&s.conn, chunk, "plan", "method");

    let hits = s
        .store
        .lookup_symbol("plan", SourceId(s.next_source))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, chunk);
    assert_eq!(hits[0].symbol.as_deref(), Some("plan"));
    assert_eq!(hits[0].symbol_kind.as_deref(), Some("method"));

    let hits = s
        .store
        .search_symbols("plan", SymbolMatch::Exact, SymbolScope::All, 10)
        .unwrap();
    assert_eq!(ids(&hits), vec![chunk]);
    assert_eq!(hits[0].symbol.as_deref(), Some("plan"));
    assert_eq!(hits[0].symbol_kind.as_deref(), Some("method"));
}

/// The scope narrows to one library or one source id.
#[test]
fn symbol_scope_narrows_the_search() {
    let s = ladder();
    let hits = s
        .store
        .search_symbols(
            "use_router",
            SymbolMatch::Exact,
            SymbolScope::Library(String::from("NEXT.JS")),
            10,
        )
        .unwrap();
    assert_eq!(ids(&hits), vec![s.chunk_a, s.chunk_b]);

    let hits = s
        .store
        .search_symbols(
            "use_router",
            SymbolMatch::Exact,
            SymbolScope::Library(String::from("vue")),
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1, "only the vue chunk");
    assert_eq!(hits[0].library, "vue");

    let hits = s
        .store
        .search_symbols(
            "use_router",
            SymbolMatch::Exact,
            SymbolScope::Source(s.vue_source),
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_id, s.vue_source);
}

/// The limit caps the result set.
#[test]
fn symbol_limit_caps_the_results() {
    let s = ladder();
    let hits = s
        .store
        .search_symbols("use_router", SymbolMatch::Exact, SymbolScope::All, 1)
        .unwrap();
    assert_eq!(ids(&hits), vec![s.chunk_a], "the lower chunk id wins");
}

/// The limit counts chunks, not symbol rows: one chunk with several
/// matching declarations consumes one slot, leaving room for other chunks.
#[test]
fn symbol_limit_counts_chunks_not_symbol_rows() {
    let s = ladder();
    symbol(&s.conn, s.chunk_a, "use_router_a", "function");
    symbol(&s.conn, s.chunk_a, "use_router_b", "function");
    symbol(&s.conn, s.chunk_a, "use_router_c", "function");

    // Six matching rows over two chunks (four on chunk_a); the per-chunk
    // pick keeps one row each, so both chunks fit under the limit.
    let hits = s
        .store
        .search_symbols(
            "use_router",
            SymbolMatch::CaseSensitivePrefix,
            SymbolScope::All,
            2,
        )
        .unwrap();
    assert_eq!(
        ids(&hits),
        vec![s.chunk_a, s.chunk_b],
        "chunk_a's extra matches must not consume chunk_b's slot"
    );
}

/// Inactive sources drop out of the ladder, like every serving query.
#[test]
fn symbol_ladder_skips_inactive_sources() {
    let s = ladder();
    s.conn
        .execute(
            "UPDATE sources SET active = 0 WHERE source_id = ?1",
            rusqlite::params![s.next_source],
        )
        .unwrap();
    let hits = s
        .store
        .search_symbols("use_router", SymbolMatch::Exact, SymbolScope::All, 10)
        .unwrap();
    assert_eq!(hits.len(), 1, "only the vue chunk stays");
}
