//! Retrieval read paths on the store API: source-scoped search, full
//! chunk reads for `open`, source lookup, and version listing.
//!
//! Tests seed a migrated temp database with raw SQL so the retrieval
//! semantics are exercised without the publish path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_store::{Store, build_match_query};
use rusqlite::Connection;
use tempfile::tempdir;

/// Opens a store on a migrated temp file and seeds three sources: next.js
/// at two versions, a second active origin of next.js 16.2.9, and an
/// inactive zod. The inactive source must never serve.
struct Seeded {
    store: Store,
    /// The active next.js 16.2.9 source (the lowest id of the pair).
    v16: i64,
    /// The second active origin of next.js 16.2.9 (a higher id).
    v16b: i64,
    /// The active next.js 15.0.0 source.
    v15: i64,
    /// The inactive zod 4.6.7 source.
    zod: i64,
    dir: tempfile::TempDir,
}

/// Inserts the raw `sources` rows: active next.js at 15.0.0, a second
/// active origin of next.js 16.2.9, inactive zod, and repoints the
/// `seed_source` next.js row at its real version and origin. Returns the
/// (next.js 16, mirror 16, next.js 15, zod) source ids.
fn seeded_sources(conn: &Connection) -> (i64, i64, i64, i64) {
    let v16 = devdocs_testkit::seed_source(conn, "next.js", true).unwrap();
    // seed_source pins version 1.0.0 with a per-library origin; the second
    // next.js source needs distinct (version, origin) values to clear the
    // UNIQUE (library, version, kind, origin_url) constraint.
    conn.execute(
        "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
         VALUES ('next.js', '15.0.0', 'llms_txt', 'https://example.test/next-15', 'approved_index', \
         '2026-01-01T00:00:00Z', 1)",
        [],
    )
    .unwrap();
    let v15 = conn.last_insert_rowid();
    let zod = devdocs_testkit::seed_source(conn, "zod", false).unwrap();
    // A second active origin of the same (library, version) pair: the
    // UNIQUE key allows several per pair when only the origin differs.
    conn.execute(
        "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
         VALUES ('next.js', '16.2.9', 'llms_txt', 'https://example.test/16-mirror', 'approved_index', \
         '2026-01-01T00:00:00Z', 1)",
        [],
    )
    .unwrap();
    let v16b = conn.last_insert_rowid();
    // Repoint the first next.js source at its real version and origin.
    conn.execute(
        "UPDATE sources SET version = '16.2.9', origin_url = 'https://example.test/16' WHERE source_id = ?1",
        rusqlite::params![v16],
    )
    .unwrap();
    (v16, v16b, v15, zod)
}

/// Seeds two chunks on the next.js 16 source and one each on its mirror
/// origin, the next.js 15 and zod sources.
fn seed_chunks(conn: &Connection, v16: i64, v16b: i64, v15: i64, zod: i64) {
    devdocs_testkit::seed_chunk(
        conn,
        v16,
        0,
        "[\"Routing\"]",
        None,
        "The routing layer maps requests to handlers.",
    )
    .unwrap();
    devdocs_testkit::seed_chunk(
        conn,
        v16,
        1,
        "[\"Storage\"]",
        None,
        "Storage middleware caches responses.",
    )
    .unwrap();
    devdocs_testkit::seed_chunk(
        conn,
        v16b,
        0,
        "[\"Routing\"]",
        None,
        "Mirror routing notes live only on this origin.",
    )
    .unwrap();
    devdocs_testkit::seed_chunk(
        conn,
        v15,
        0,
        "[\"Routing\"]",
        None,
        "Routing in version 15 used a pages directory.",
    )
    .unwrap();
    devdocs_testkit::seed_chunk(conn, zod, 0, "[\"Parse\"]", None, "parse validates schema.")
        .unwrap();
}

fn seeded() -> Seeded {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    let (v16, v16b, v15, zod) = seeded_sources(&conn);
    seed_chunks(&conn, v16, v16b, v15, zod);
    Seeded {
        store,
        v16,
        v16b,
        v15,
        zod,
        dir,
    }
}

/// The source-scoped search serves only the requested source, even when
/// other active sources of the same library match.
#[test]
fn source_scoped_search_serves_only_that_source() {
    let s = seeded();
    let expr = build_match_query("routing").into_owned();
    let hits = s.store.search_fts_source(&expr, s.v16, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_id, s.v16);
    assert_eq!(hits[0].version, "16.2.9");

    let v15 = s.store.search_fts_source(&expr, s.v15, 10).unwrap();
    assert_eq!(v15.len(), 1);
    assert_eq!(v15[0].version, "15.0.0");
}

/// The source-scoped search carries the metadata the result envelope
/// needs: content hash, origin URL, commit, and license status.
#[test]
fn source_scoped_hits_carry_envelope_metadata() {
    let s = seeded();
    let expr = build_match_query("routing").into_owned();
    let hits = s.store.search_fts_source(&expr, s.v16, 10).unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.content_hash, [7u8; 32], "testkit hash");
    assert_eq!(hit.source_url, "https://example.test/16");
    assert_eq!(hit.source_commit, None);
    assert_eq!(hit.license_status, "approved_index");
    assert!(hit.snippet.is_some());
    assert!(hit.rank.is_some());
}

/// An inactive source never serves, on any read path.
#[test]
fn inactive_sources_never_serve() {
    let s = seeded();
    let expr = build_match_query("parse").into_owned();
    assert_eq!(
        s.store.search_fts(&expr, 10).unwrap(),
        Vec::<devdocs_store::Hit>::new()
    );
    assert_eq!(s.store.find_source("zod", "1.0.0").unwrap(), None);
    assert_eq!(
        s.store.source_chunks(s.zod).unwrap(),
        Vec::<devdocs_store::Hit>::new()
    );
}

/// The full-chunk read returns every chunk of the source in ordinal
/// order, without snippet or rank.
#[test]
fn source_chunks_read_in_ordinal_order() {
    let s = seeded();
    let chunks = s.store.source_chunks(s.v16).unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].section_path, vec!["Routing"]);
    assert_eq!(chunks[1].section_path, vec!["Storage"]);
    for chunk in &chunks {
        assert_eq!(chunk.snippet, None);
        assert_eq!(chunk.rank, None);
        assert_eq!(chunk.library, "next.js");
        assert_eq!(chunk.version, "16.2.9");
    }
}

/// A `chunks.content_hash` that is not a 32-byte BLOB — only possible
/// through a hand-corrupted database — surfaces as a
/// `FromSqlConversionFailure` on the read paths instead of panicking.
#[test]
fn corrupted_content_hash_blob_fails_as_from_sql_conversion() {
    let s = seeded();
    let conn = Connection::open(s.dir.path().join("docs.db")).unwrap();
    conn.execute("UPDATE chunks SET content_hash = X'00'", [])
        .unwrap();

    for err in [
        s.store
            .search_fts_source(&build_match_query("routing"), s.v16, 10)
            .unwrap_err(),
        s.store.source_chunks(s.v16).unwrap_err(),
    ] {
        assert!(
            matches!(
                err,
                devdocs_store::StoreError::Sql(rusqlite::Error::FromSqlConversionFailure(_, _, _))
            ),
            "expected FromSqlConversionFailure, got {err:?}"
        );
    }
}

/// The source lookup matches the exact version case-insensitively on the
/// library.
#[test]
fn find_source_matches_exact_pair() {
    let s = seeded();
    assert_eq!(
        s.store.find_source("next.js", "16.2.9").unwrap(),
        Some(s.v16)
    );
    assert_eq!(
        s.store.find_source("Next.JS", "16.2.9").unwrap(),
        Some(s.v16)
    );
    assert_eq!(s.store.find_source("next.js", "16.2.10").unwrap(), None);
}

/// The version listing returns the exact versions of the library's active
/// sources, descending.
#[test]
fn library_versions_list_descending() {
    let s = seeded();
    assert_eq!(
        s.store.library_versions("next.js").unwrap(),
        vec!["16.2.9".to_owned(), "15.0.0".to_owned()]
    );
    assert_eq!(
        s.store.library_versions("zod").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        s.store.library_versions("nope").unwrap(),
        Vec::<String>::new()
    );
}

/// The plural source lookup returns every active origin of one exact
/// `(library, version)` pair, lowest id first, case-insensitive on the
/// library; the singular lookup keeps resolving the lowest id so one ref
/// always serves one source.
#[test]
fn find_sources_returns_every_active_origin_of_the_pair() {
    let s = seeded();
    assert_eq!(
        s.store.find_sources("next.js", "16.2.9").unwrap(),
        vec![s.v16, s.v16b]
    );
    assert_eq!(
        s.store.find_sources("Next.JS", "16.2.9").unwrap(),
        vec![s.v16, s.v16b]
    );
    assert_eq!(
        s.store.find_sources("next.js", "16.2.10").unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(
        s.store.find_source("next.js", "16.2.9").unwrap(),
        Some(s.v16)
    );
}

/// The source-set search serves chunks unique to the higher-id origin —
/// a pinned version reaches every origin of the pair, not only the
/// lowest source id.
#[test]
fn source_set_search_serves_every_origin_of_the_pair() {
    let s = seeded();
    // The mirror chunk exists only on the higher-id origin.
    let mirror = s
        .store
        .search_fts_sources(&build_match_query("mirror"), &[s.v16, s.v16b], 10)
        .unwrap();
    assert_eq!(mirror.len(), 1);
    assert_eq!(mirror[0].source_id, s.v16b);

    // A term both origins carry serves from both.
    let shared = s
        .store
        .search_fts_sources(&build_match_query("routing"), &[s.v16, s.v16b], 10)
        .unwrap();
    let mut sources: Vec<i64> = shared.iter().map(|hit| hit.source_id).collect();
    sources.sort_unstable();
    assert_eq!(sources, vec![s.v16, s.v16b]);

    // An empty set serves nothing.
    assert_eq!(
        s.store
            .search_fts_sources(&build_match_query("routing"), &[], 10)
            .unwrap(),
        Vec::<devdocs_store::Hit>::new()
    );
}
