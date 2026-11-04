//! The serve-layer read surfaces: the `versions` discovery list
//! (docs/implementation/08 §CLI contract), the search pipeline's
//! migration rung, miss reasons, and cursor resume, and the `open` and
//! `verify` ref paths.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::MissReason;
use devdocs_model::VersionStatus;
use devdocs_retrieve::Plan;
use devdocs_retrieve::Query;
use devdocs_retrieve::RetrieveError;
use devdocs_retrieve::serve::{open, search, versions};
use devdocs_store::Store;
use tempfile::tempdir;

/// Seeds one active and one inactive source of `acme`, plus one active
/// source of another library.
fn seeded() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        devdocs_testkit::seed_source(conn, "acme", true)?;
        conn.execute(
            "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
             VALUES ('acme', '0.9.0', 'llms_txt', 'https://example.test/acme-0', 'approved_index', \
             '2026-01-01T00:00:00Z', 0)",
            [],
        )?;
        devdocs_testkit::seed_source(conn, "other", true)?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// Only active sources list; a withdrawn generation is not an available
/// version.
#[test]
fn versions_list_only_active_sources() {
    let (_dir, store) = seeded();
    let envelope = versions(&store, "acme").unwrap();
    let listed: Vec<&str> = envelope
        .results
        .iter()
        .map(|s| s.version.as_str())
        .collect();
    assert_eq!(listed, vec!["1.0.0"]);
}

/// A library with no active sources misses with `not_indexed`.
#[test]
fn an_unknown_library_misses() {
    let (_dir, store) = seeded();
    let error = versions(&store, "missing").unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "unknown library must miss, got {error}"
    );
}

/// Seeds `acme` with two active versions — 1.0.0 and 0.9.0 — one routing
/// chunk per version.
fn seeded_two_versions() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let current = devdocs_testkit::seed_source(conn, "acme", true)?;
        conn.execute(
            "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
             VALUES ('acme', '0.9.0', 'llms_txt', 'https://example.test/acme-old', 'approved_index', \
             '2026-01-01T00:00:00Z', 1)",
            [],
        )?;
        let older = conn.last_insert_rowid();
        devdocs_testkit::seed_chunk(
            conn,
            current,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer helps you migrate routing requests.",
        )?;
        devdocs_testkit::seed_chunk(
            conn,
            older,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer helps you migrate routing requests.",
        )?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// Seeds `acme` with three routing chunks in distinct sections so each
/// carries its own ref.
fn seeded_three_chunks() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let source = devdocs_testkit::seed_source(conn, "acme", true)?;
        for (ordinal, section) in ["Routing", "Storage", "Cache"].iter().enumerate() {
            devdocs_testkit::seed_chunk(
                conn,
                source,
                i64::try_from(ordinal).unwrap(),
                &format!("[\"{section}\"]"),
                None,
                "The routing layer maps requests.",
            )?;
        }
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// A migration question retrieves the served version and the nearest
/// older version — the migration rung runs and labels the older hits.
#[test]
fn a_migration_question_retrieves_the_older_version() {
    let (dir, store) = seeded_two_versions();
    let query = Query::new("Migrate routing", Some("acme"), None);
    let envelope = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    let served: Vec<(String, VersionStatus)> = envelope
        .results
        .iter()
        .map(|item| (item.served_version.clone(), item.version_status))
        .collect();
    assert!(
        served.contains(&("1.0.0".to_owned(), VersionStatus::Alias)),
        "the served version must answer with the alias default, got {served:?}"
    );
    assert!(
        served.contains(&("0.9.0".to_owned(), VersionStatus::Fallback)),
        "the migration rung must fetch the older version, got {served:?}"
    );
}

/// Searching an empty migrated store misses with `not_indexed`, not
/// `low_confidence` — there is no active source to search.
#[test]
fn an_empty_index_misses_not_indexed() {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let query = Query::new("anything", None, None);
    let error = search(&store, &query, dir.path(), &Plan::defaults()).unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "an empty index must miss with not_indexed, got {error}"
    );
}

/// Resuming with `next_cursor` continues after the served chunk in the
/// same fusion order.
#[test]
fn search_resumes_after_the_cursor() {
    let (dir, store) = seeded_three_chunks();
    let query = Query::new("routing", Some("acme"), None);
    let full = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.next_cursor, None);
    let all_refs: Vec<String> = full
        .results
        .iter()
        .map(|item| item.doc_ref.clone())
        .collect();
    assert_eq!(all_refs.len(), 3);

    let budget = Plan {
        max_tokens: 8,
        ..Plan::defaults()
    };
    let first = search(&store, &query, dir.path(), &budget).unwrap();
    assert!(first.truncated);
    let cursor: u64 = first.next_cursor.unwrap().parse().unwrap();
    let resumed_plan = Plan {
        cursor: Some(cursor),
        ..budget
    };
    let resumed = search(&store, &query, dir.path(), &resumed_plan).unwrap();

    // The resume continues in the same fusion order: it serves the chunk
    // after the cursor's chunk and nothing before it.
    let served = first.results.first().unwrap().doc_ref.clone();
    let position = all_refs.iter().position(|r| *r == served).unwrap();
    let resumed_refs: Vec<String> = resumed
        .results
        .iter()
        .map(|item| item.doc_ref.clone())
        .collect();
    assert_eq!(resumed_refs, vec![all_refs[position + 1].clone()]);
}

/// Opening a ref whose library has no active source misses with
/// `not_indexed`.
#[test]
fn open_of_an_unindexed_library_misses() {
    let (_dir, store) = seeded();
    let error = open(&store, "docs://nope/v1/x", &Plan::defaults()).unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "unindexed library must miss, got {error}"
    );
}

// Verify's headerless-chunk branch (an empty section path) is
// unreachable from the CLI surface: the ref grammar requires a path, and
// a headerless chunk has neither title nor symbol to match — the branch
// carries a `// defensive:` anchor in serve.rs instead.
