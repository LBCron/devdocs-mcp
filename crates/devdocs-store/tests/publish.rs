//! Publish behavior: atomic replacement, forced-failure rollback, and the
//! license gate (docs/implementation/07 §Atomic publish).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration as StdDuration;

use devdocs_model::{LicenseStatus, SourceId, SourceKind, SourceRecord};
use devdocs_store::{NewChunk, Store, build_match_query, content_hash};
use rusqlite::Connection;
use tempfile::tempdir;
use time::OffsetDateTime;
use url::Url;

/// A store with schema applied on a temp file, plus a raw connection for
/// assertions.
struct Rig {
    store: Store,
    conn: Connection,
    _dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    Rig {
        store,
        conn,
        _dir: dir,
    }
}

/// A publishable source record at the index approval stage.
fn source(status: LicenseStatus) -> SourceRecord {
    SourceRecord {
        id: SourceId(0),
        library: "next.js".to_owned(),
        version: "16.2.9".to_owned(),
        kind: SourceKind::LlmsTxt,
        origin_url: Url::parse("https://example.test/next/llms.txt").unwrap(),
        commit_sha: Some("abc123".to_owned()),
        license: Some("MIT".to_owned()),
        license_status: status,
        fetched_at: OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap(),
    }
}

/// A one-line chunk body with the given ordinal and body term.
fn chunk(ordinal: u32, symbol: Option<(&str, &str)>, body: &str) -> NewChunk {
    NewChunk {
        section_path: vec!["Routing".to_owned()],
        symbol: symbol.map(|(name, kind)| devdocs_store::Symbol {
            name: name.to_owned(),
            kind: kind.to_owned(),
        }),
        text: format!("## Routing\n\n{body}"),
        token_count: 16,
        content_hash: content_hash(body),
        ordinal,
    }
}

/// A publish upserts the source and flips it active with its metadata.
#[test]
fn publish_upserts_and_activates_the_source() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[chunk(0, None, "A body to publish.")],
        )
        .unwrap();

    let (active, _fetched_at, commit_sha): (i64, String, String) = rig
        .conn
        .query_row(
            "SELECT active, fetched_at, commit_sha FROM sources WHERE library = 'next.js'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(active, 1, "a successful publish flips the source active");
    assert_eq!(commit_sha, "abc123");
}

/// A publish inserts the chunk rows and their symbol rows.
#[test]
fn publish_inserts_chunks_and_symbols() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[
                chunk(
                    0,
                    Some(("use_router", "function")),
                    "The use_router hook configures routing.",
                ),
                chunk(1, None, "Middleware runs before routing decisions."),
            ],
        )
        .unwrap();

    let chunks: i64 = rig
        .conn
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 2);

    let symbols: (i64, String) = rig
        .conn
        .query_row(
            "SELECT count(*), kind FROM symbols WHERE symbol = 'use_router'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(symbols, (1, "function".to_owned()));
}

/// The published generation is searchable right away.
#[test]
fn publish_is_searchable_right_away() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[chunk(
                0,
                Some(("use_router", "function")),
                "The use_router hook configures routing.",
            )],
        )
        .unwrap();

    let expr = build_match_query("use_router").into_owned();
    let hits = rig.store.search_fts(&expr, 10).unwrap();
    assert_eq!(hits.len(), 1, "publish-then-search round trip");
    assert_eq!(hits[0].symbol.as_deref(), Some("use_router"));
}

/// A second publish replaces the chunk set: the old body drops out of the
/// FTS index and the new one drops in.
#[test]
fn republish_replaces_the_chunk_set() {
    let rig = rig();
    let src = source(LicenseStatus::ApprovedIndex);
    rig.store
        .publish_generation(
            &src,
            &[chunk(0, None, "generation one mentions alpha tokens.")],
        )
        .unwrap();

    rig.store
        .publish_generation(
            &src,
            &[
                chunk(
                    0,
                    Some(("rewrite", "function")),
                    "generation two covers beta rewrites.",
                ),
                chunk(1, None, "generation two adds gamma middleware."),
            ],
        )
        .unwrap();

    let chunks: i64 = rig
        .conn
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 2, "the old set is gone, the new set is in");

    let alpha = build_match_query("alpha").into_owned();
    assert!(
        rig.store.search_fts(&alpha, 10).unwrap().is_empty(),
        "old-generation terms must leave the index"
    );
    let beta = build_match_query("beta").into_owned();
    assert_eq!(rig.store.search_fts(&beta, 10).unwrap().len(), 1);
}

/// A forced failure mid-publish rolls everything back: the old generation
/// stays active and searchable.
#[test]
fn forced_failure_keeps_the_old_generation_searchable() {
    let rig = rig();
    let src = source(LicenseStatus::ApprovedIndex);
    rig.store
        .publish_generation(
            &src,
            &[chunk(0, None, "the stable generation holds ground.")],
        )
        .unwrap();

    // Two chunks share ordinal 0, so the second insert violates
    // UNIQUE(source_id, ordinal) after the old rows are already deleted.
    let broken = vec![
        chunk(0, None, "a new generation that never lands."),
        chunk(0, None, "a duplicate ordinal forces the failure."),
    ];
    let error = rig.store.publish_generation(&src, &broken).unwrap_err();
    assert!(
        matches!(error, devdocs_store::StoreError::Sql(_)),
        "expected the unique-violation error, got {error:?}"
    );

    let chunks: i64 = rig
        .conn
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 1, "the old generation must survive");

    let (active,): (i64,) = rig
        .conn
        .query_row("SELECT active FROM sources", [], |row| Ok((row.get(0)?,)))
        .unwrap();
    assert_eq!(active, 1, "the source stays active");

    let stable = build_match_query("stable").into_owned();
    let hits = rig.store.search_fts(&stable, 10).unwrap();
    assert_eq!(hits.len(), 1, "the old generation stays searchable");
}

/// The license gate refuses sources below the index stage before anything
/// is written.
#[test]
fn license_gate_refuses_publish_below_the_index_stage() {
    let rig = rig();
    for status in [
        LicenseStatus::PendingReview,
        LicenseStatus::ApprovedFetch,
        LicenseStatus::Denied,
        LicenseStatus::Expired,
        LicenseStatus::Revoked,
    ] {
        let error = rig
            .store
            .publish_generation(&source(status), &[chunk(0, None, "never lands")])
            .unwrap_err();
        match error {
            devdocs_store::StoreError::LicenseNotApproved { status: refused } => {
                assert_eq!(refused, status);
            }
            other => panic!("expected LicenseNotApproved for {status}, got {other:?}"),
        }
    }

    let rows: i64 = rig
        .conn
        .query_row("SELECT count(*) FROM sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0, "a refused publish must write nothing");
}

/// A terminal license state withdraws a previously indexed source: the
/// stored status lands, the source deactivates, and its chunks leave the
/// search results — while the refused-publish error still returns.
#[test]
fn a_terminal_license_state_withdraws_the_source() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[chunk(0, None, "a generation that later gets withdrawn.")],
        )
        .unwrap();

    let error = rig
        .store
        .publish_generation(&source(LicenseStatus::Revoked), &[])
        .unwrap_err();
    match error {
        devdocs_store::StoreError::LicenseNotApproved { status: refused } => {
            assert_eq!(refused, LicenseStatus::Revoked);
        }
        other => panic!("expected LicenseNotApproved, got {other:?}"),
    }

    let (status, active): (String, i64) = rig
        .conn
        .query_row(
            "SELECT license_status, active FROM sources WHERE library = 'next.js'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "revoked");
    assert_eq!(active, 0, "a revoked source must deactivate");

    let term = build_match_query("withdrawn").into_owned();
    assert!(
        rig.store.search_fts(&term, 10).unwrap().is_empty(),
        "a withdrawn source must leave the search results"
    );
}

/// A source whose stored status turns terminal while it stays active —
/// exactly the state a revoked registry entry leaves behind when the gate
/// refuses before any publish runs — drops out of retrieval at query time
/// (docs/09 §License gate enforcement points, point 3).
#[test]
fn search_drops_sources_whose_license_turned_terminal() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[chunk(0, None, "published before the revocation.")],
        )
        .unwrap();
    let term = build_match_query("revocation").into_owned();
    assert_eq!(rig.store.search_fts(&term, 10).unwrap().len(), 1);

    rig.conn
        .execute(
            "UPDATE sources SET license_status = 'revoked' WHERE active = 1",
            [],
        )
        .unwrap();
    assert!(
        rig.store.search_fts(&term, 10).unwrap().is_empty(),
        "a revoked source must drop out at query time"
    );
}

/// The fetched-at format survives a round trip through storage.
#[test]
fn fetched_at_is_rfc3339_utc() {
    let rig = rig();
    rig.store
        .publish_generation(
            &source(LicenseStatus::ApprovedIndex),
            &[chunk(0, None, "timestamp probe")],
        )
        .unwrap();
    let stored: String = rig
        .conn
        .query_row("SELECT fetched_at FROM sources", [], |row| row.get(0))
        .unwrap();
    let parsed =
        OffsetDateTime::parse(&stored, &time::format_description::well_known::Rfc3339).unwrap();
    let delta = parsed - OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
    assert!(delta.abs() < StdDuration::from_secs(1));
}
